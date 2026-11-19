//! Live changes and the 0.5 upstream settings over real sockets: reload,
//! endpoint changes, draining, discovery and its allowlist, the PROXY
//! protocol, and request ids.

#![cfg(feature = "serve")]

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use proxy_net::client::{Discovery, DiscoveryError, StaticDiscovery};
use proxy_net::{Endpoint, EndpointAddr, Pool, ProxyBuilder, ProxyProtocol, Running};

/// The signature that starts a binary PROXY v2 header.
const V2_SIGNATURE: &[u8] = &[
    0x0d, 0x0a, 0x0d, 0x0a, 0x00, 0x0d, 0x0a, 0x51, 0x55, 0x49, 0x54, 0x0a,
];

/// An upstream that answers with its own name and records each
/// connection's PROXY header (when one is sent) and any `x-request-id`.
fn upstream(name: &'static str) -> (SocketAddr, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let seen = Arc::new(Mutex::new(Vec::new()));
    let record = Arc::clone(&seen);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut s) = stream else { continue };
            let record = Arc::clone(&record);
            std::thread::spawn(move || {
                let _ = s.set_read_timeout(Some(Duration::from_secs(5)));
                // The first 16 bytes tell a binary v2 header from a text line.
                let mut first = [0u8; 16];
                if s.read_exact(&mut first).is_err() {
                    return;
                }
                let mut head = Vec::new();
                if first.starts_with(V2_SIGNATURE) {
                    let len = usize::from(u16::from_be_bytes([first[14], first[15]]));
                    let mut rest = vec![0u8; len];
                    if s.read_exact(&mut rest).is_err() {
                        return;
                    }
                    record
                        .lock()
                        .expect("lock")
                        .push(format!("v2 {}", 16 + len));
                } else {
                    head.extend_from_slice(&first);
                }
                let mut b = [0u8; 1];
                while !head.ends_with(&[13, 10, 13, 10]) {
                    match s.read(&mut b) {
                        Ok(1) => head.push(b[0]),
                        _ => return,
                    }
                }
                let text = String::from_utf8_lossy(&head).into_owned();
                if let Some(line) = text.lines().next() {
                    if line.starts_with("PROXY") {
                        record.lock().expect("lock").push(line.to_string());
                    }
                }
                for line in text.lines() {
                    if let Some(id) = line.strip_prefix("x-request-id: ") {
                        record.lock().expect("lock").push(format!("id {id}"));
                    }
                }
                let body = name.as_bytes();
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = s.write_all(head.as_bytes());
                let _ = s.write_all(body);
            });
        }
    });
    (addr, seen)
}

fn serve(b: ProxyBuilder) -> (Running, SocketAddr) {
    let running = b
        .build()
        .expect("build")
        .serve("127.0.0.1:0")
        .expect("serve");
    let addr = running.local_addr().expect("addr");
    (running, addr)
}

/// The body of a GET through the proxy (it arrives chunked).
fn get(addr: SocketAddr) -> String {
    let mut s = TcpStream::connect(addr).expect("connect");
    s.set_read_timeout(Some(Duration::from_secs(5)))
        .expect("timeout");
    s.write_all(b"GET / HTTP/1.1\r\nHost: h\r\nConnection: close\r\n\r\n")
        .expect("write");
    let mut out = Vec::new();
    let _ = s.read_to_end(&mut out);
    let text = String::from_utf8_lossy(&out).into_owned();
    let raw = text
        .split("\r\n\r\n")
        .skip(1)
        .collect::<Vec<_>>()
        .join("\r\n\r\n");
    let mut body = String::new();
    let mut r = raw.as_str();
    while let Some(j) = r.find("\r\n") {
        let Ok(n) = usize::from_str_radix(r[..j].trim(), 16) else {
            return raw;
        };
        if n == 0 {
            break;
        }
        body.push_str(&r[j + 2..j + 2 + n]);
        r = &r[j + 2 + n + 2..];
    }
    body
}

#[test]
fn test_reload_switches_endpoints_while_running() {
    let (a, _) = upstream("a");
    let (b, _) = upstream("b");
    let (p, addr) = serve(ProxyBuilder::new().pool("app", Pool::new([a.to_string()])));
    assert_eq!(get(addr), "a");
    p.handle()
        .reload(ProxyBuilder::new().pool("app", Pool::new([b.to_string()])))
        .expect("reload");
    assert_eq!(get(addr), "b");
    // A bad configuration leaves the running one as it was.
    assert!(p.handle().reload(ProxyBuilder::new()).is_err());
    assert_eq!(get(addr), "b");
}

#[test]
fn test_set_endpoints_and_drain() {
    let (a, _) = upstream("a");
    let (b, _) = upstream("b");
    let (p, addr) = serve(ProxyBuilder::new().pool("app", Pool::new([a.to_string()])));
    let h = p.handle();
    let both = [
        Endpoint::parse(&a.to_string()).expect("a"),
        Endpoint::parse(&b.to_string()).expect("b"),
    ];
    h.set_endpoints("app", &both).expect("set");
    let seen: Vec<String> = (0..4).map(|_| get(addr)).collect();
    assert!(
        seen.contains(&"a".to_string()) && seen.contains(&"b".to_string()),
        "{seen:?}"
    );
    assert!(h.drain_endpoint("app", &EndpointAddr::Ip(a)));
    for _ in 0..4 {
        assert_eq!(get(addr), "b");
    }
    let nowhere = EndpointAddr::Ip("127.0.0.1:9".parse().expect("addr"));
    assert!(!h.drain_endpoint("app", &nowhere));
    assert!(h.set_endpoints("nope", &[]).is_err());
}

struct Changing {
    calls: AtomicUsize,
    first: SocketAddr,
    then: SocketAddr,
}

impl Discovery for Changing {
    fn endpoints(&self) -> Result<Vec<Endpoint>, DiscoveryError> {
        let n = self.calls.fetch_add(1, Ordering::Relaxed);
        let addr = if n == 0 { self.first } else { self.then };
        Ok(vec![Endpoint::new(EndpointAddr::Ip(addr))])
    }

    fn refresh_every(&self) -> Option<Duration> {
        Some(Duration::from_millis(200))
    }
}

#[test]
fn test_discovery_refreshes_the_pool() {
    let (a, _) = upstream("a");
    let (b, _) = upstream("b");
    let (_p, addr) = serve(
        ProxyBuilder::new().pool(
            "app",
            Pool::new(Vec::<&str>::new())
                .discovery(Changing {
                    calls: AtomicUsize::new(0),
                    first: a,
                    then: b,
                })
                .discovery_allow(&["127.0.0.0/8"]),
        ),
    );
    assert_eq!(get(addr), "a");
    let deadline = Instant::now() + Duration::from_secs(5);
    while get(addr) != "b" {
        assert!(Instant::now() < deadline, "discovery never moved the pool");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn test_discovery_outside_the_allowlist_is_refused() {
    // T-20: a discovered endpoint outside the allowed ranges never takes
    // traffic; the static endpoint stays.
    let (a, _) = upstream("a");
    let outside = vec![Endpoint::parse("10.99.0.1:80").expect("e")];
    let (_p, addr) = serve(
        ProxyBuilder::new().pool(
            "app",
            Pool::new([a.to_string()])
                .discovery(StaticDiscovery::new(outside))
                .discovery_allow(&["127.0.0.0/8"]),
        ),
    );
    assert_eq!(get(addr), "a");
    // Without an allowlist, a discovered loopback address is refused too.
    let (_p2, addr2) = serve(ProxyBuilder::new().pool(
        "app",
        Pool::new([a.to_string()]).discovery(StaticDiscovery::new(vec![
            Endpoint::parse("127.0.0.1:9").expect("e"),
        ])),
    ));
    assert_eq!(get(addr2), "a");
}

#[test]
fn test_proxy_protocol_headers_reach_the_upstream() {
    let (v1, seen1) = upstream("v1");
    let pool = Pool::new([v1.to_string()]).proxy_protocol(ProxyProtocol::V1);
    let (_p, addr) = serve(ProxyBuilder::new().pool("app", pool));
    assert_eq!(get(addr), "v1");
    let line = seen1
        .lock()
        .expect("lock")
        .first()
        .cloned()
        .unwrap_or_default();
    assert!(
        line.starts_with("PROXY TCP4 127.0.0.1 127.0.0.1 "),
        "{line}"
    );
    let (v2, seen2) = upstream("v2");
    let pool = Pool::new([v2.to_string()]).proxy_protocol(ProxyProtocol::v2());
    let (_p2, addr2) = serve(ProxyBuilder::new().pool("app", pool));
    assert_eq!(get(addr2), "v2");
    let line = seen2
        .lock()
        .expect("lock")
        .first()
        .cloned()
        .unwrap_or_default();
    assert!(line.starts_with("v2 "), "{line}");
}

#[test]
fn test_request_ids_are_added() {
    let (a, seen) = upstream("a");
    let (_p, addr) = serve(
        ProxyBuilder::new()
            .pool("app", Pool::new([a.to_string()]))
            .request_id("x-request-id"),
    );
    for _ in 0..3 {
        assert_eq!(get(addr), "a");
    }
    let ids: Vec<String> = seen
        .lock()
        .expect("lock")
        .iter()
        .filter(|l| l.starts_with("id "))
        .cloned()
        .collect();
    assert_eq!(ids.len(), 3, "{ids:?}");
    assert!(ids.iter().all(|i| i.len() == 3 + 32), "{ids:?}");
    assert_ne!(ids[0], ids[1]);
}

#[test]
fn test_endpoints_added_live_ramp_up() {
    let (a, _) = upstream("a");
    let (b, _) = upstream("b");
    let slow = Duration::from_secs(600);
    let (p, addr) =
        serve(ProxyBuilder::new().pool("app", Pool::new([a.to_string()]).slow_start(slow)));
    assert_eq!(get(addr), "a");
    let h = p.handle();
    h.reload(ProxyBuilder::new().pool(
        "app",
        Pool::new([a.to_string(), b.to_string()]).slow_start(slow),
    ))
    .expect("reload");
    let ramps = |h: &proxy_net::ProxyHandle| -> Vec<u32> {
        h.health().pools[0]
            .endpoints
            .iter()
            .map(|e| e.ramp)
            .collect()
    };
    let r = ramps(&h);
    assert_eq!(r[0], 100);
    assert!((10..20).contains(&r[1]), "{r:?}");
    // A second reload that keeps both endpoints keeps the ramp running.
    h.reload(ProxyBuilder::new().pool(
        "app",
        Pool::new([a.to_string(), b.to_string()]).slow_start(slow),
    ))
    .expect("reload");
    let r = ramps(&h);
    assert!((10..20).contains(&r[1]), "{r:?}");
    let picked_b = (0..40).filter(|_| get(addr) == "b").count();
    assert!(picked_b < 15, "{picked_b}");
}

#[test]
fn test_a_request_spanning_a_reload_is_checked_under_the_new_routes() {
    let (a, _) = upstream("a");
    let (p, addr) = serve(ProxyBuilder::new().pool("app", Pool::new([a.to_string()])));
    let mut s = TcpStream::connect(addr).expect("connect");
    s.set_read_timeout(Some(Duration::from_secs(5)))
        .expect("timeout");
    // The head is admitted under the first configuration...
    s.write_all(b"POST / HTTP/1.1\r\nHost: h\r\nContent-Length: 4\r\nConnection: close\r\n\r\n")
        .expect("write");
    std::thread::sleep(Duration::from_millis(200));
    // ...and the body arrives after a reload that allows only GET.
    p.handle()
        .reload(
            ProxyBuilder::new()
                .pool("app", Pool::new([a.to_string()]))
                .route(proxy_net::Route::any().methods(&["GET"]).to("app")),
        )
        .expect("reload");
    s.write_all(b"body").expect("write");
    let mut out = Vec::new();
    let _ = s.read_to_end(&mut out);
    let text = String::from_utf8_lossy(&out);
    // A route for GET only: POST matches none, as it would in a new head.
    assert!(text.starts_with("HTTP/1.1 404"), "{text}");
}
