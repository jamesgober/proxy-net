//! The rest of the attack suite over real sockets: one test per threat in
//! PLAN section 4 not covered elsewhere.
//!
//! Where the other threats are tested: T-1, T-4, and T-13 in
//! `attacks_sockets.rs`; T-2 in `proxying.rs`; T-3, T-5, T-6, T-7, T-10,
//! T-14, T-15, and T-17 in `attacks.rs`; T-7 and T-11 for upgrades in
//! `websocket.rs`; T-11 and T-19 for `wss` in `websocket_tls.rs`; T-19 for
//! a plain upstream behind TLS in `tls.rs`; T-20 in `live.rs`.

#![cfg(feature = "serve")]

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use proxy_net::{HeaderRule, Limits, Pool, ProxyBuilder, Route, Running};

/// What a scripted upstream saw.
#[derive(Clone, Default)]
struct Seen {
    connections: Arc<AtomicUsize>,
    heads: Arc<Mutex<Vec<String>>>,
}

impl Seen {
    fn heads(&self) -> Vec<String> {
        self.heads.lock().expect("lock").clone()
    }
    fn connections(&self) -> usize {
        self.connections.load(Ordering::SeqCst)
    }
}

/// A keep-alive upstream that answers each request head (bodies ignored)
/// with `answer(head, n)`, where `n` counts the requests on that connection
/// from 0. An answer ending in `None` closes the connection after writing.
fn upstream(answer: fn(&str, usize) -> (Vec<u8>, bool)) -> (SocketAddr, Seen) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let seen = Seen::default();
    let s2 = seen.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut s) = stream else { continue };
            s2.connections.fetch_add(1, Ordering::SeqCst);
            let seen = s2.clone();
            std::thread::spawn(move || {
                let _ = s.set_read_timeout(Some(Duration::from_secs(5)));
                let mut n = 0;
                loop {
                    let mut head = Vec::new();
                    let mut b = [0u8; 1];
                    while !head.ends_with(b"\r\n\r\n") {
                        match s.read(&mut b) {
                            Ok(1) => head.push(b[0]),
                            _ => return,
                        }
                    }
                    let text = String::from_utf8_lossy(&head).into_owned();
                    seen.heads.lock().expect("lock").push(text.clone());
                    let (bytes, keep) = answer(&text, n);
                    if s.write_all(&bytes).is_err() || !keep {
                        return;
                    }
                    n += 1;
                }
            });
        }
    });
    (addr, seen)
}

fn ok(_: &str, _: usize) -> (Vec<u8>, bool) {
    (
        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok".to_vec(),
        true,
    )
}

fn serve(builder: ProxyBuilder) -> (Running, SocketAddr) {
    let running = builder
        .build()
        .expect("build")
        .serve("127.0.0.1:0")
        .expect("serve");
    let addr = running.local_addr().expect("addr");
    (running, addr)
}

/// Sends `request` on a new connection and returns everything until the
/// proxy closes it (or 5 s pass).
fn send(addr: SocketAddr, request: &[u8]) -> String {
    let mut s = TcpStream::connect(addr).expect("connect");
    s.set_read_timeout(Some(Duration::from_secs(5)))
        .expect("timeout");
    s.write_all(request).expect("write");
    let mut out = Vec::new();
    let _ = s.read_to_end(&mut out);
    String::from_utf8_lossy(&out).into_owned()
}

fn status(response: &str) -> u16 {
    response
        .get(9..12)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

#[test]
fn test_t8_host_routing_follows_one_reading_of_the_host() {
    let (a, seen_a) = upstream(ok);
    let (b, seen_b) = upstream(ok);
    let (_p, addr) = serve(
        ProxyBuilder::new()
            .pool("a", Pool::new([a.to_string()]))
            .pool("b", Pool::new([b.to_string()]))
            .route(Route::host("a.example").to("a"))
            .route(Route::host("b.example").to("b")),
    );
    // A host no route serves reaches no upstream.
    let r = send(
        addr,
        b"GET / HTTP/1.1\r\nHost: c.example\r\nConnection: close\r\n\r\n",
    );
    assert_eq!(status(&r), 404, "{r}");
    // Case, a port, and a trailing dot are one host.
    let r = send(
        addr,
        b"GET / HTTP/1.1\r\nHost: A.Example.:80\r\nConnection: close\r\n\r\n",
    );
    assert_eq!(status(&r), 200, "{r}");
    assert_eq!(seen_a.heads().len(), 1);
    // Two Host fields are refused before routing.
    let r = send(
        addr,
        b"GET / HTTP/1.1\r\nHost: a.example\r\nHost: b.example\r\nConnection: close\r\n\r\n",
    );
    assert_eq!(status(&r), 400, "{r}");
    // An absolute target naming another host than Host: whichever way it is
    // routed, the upstream that gets it sees the host it was routed by.
    let _ = send(
        addr,
        b"GET http://b.example/x HTTP/1.1\r\nHost: a.example\r\nConnection: close\r\n\r\n",
    );
    for h in seen_a.heads() {
        assert!(
            h.to_ascii_lowercase().contains("\r\nhost: a.example"),
            "{h}"
        );
    }
    for h in seen_b.heads() {
        assert!(
            h.to_ascii_lowercase().contains("\r\nhost: b.example"),
            "{h}"
        );
    }
    assert!(seen_a.heads().len() + seen_b.heads().len() <= 2);
}

#[test]
fn test_t9_no_request_names_the_address_the_proxy_dials() {
    // An internal service the proxy must never be steered to.
    let internal = TcpListener::bind("127.0.0.1:0").expect("bind");
    let internal_addr = internal.local_addr().expect("addr");
    internal.set_nonblocking(true).expect("nonblocking");
    let (up, _seen) = upstream(ok);
    let (_p, addr) = serve(
        ProxyBuilder::new()
            .pool("app", Pool::new([up.to_string()]))
            .route(Route::any().to("app")),
    );
    let i = internal_addr.to_string();
    for request in [
        format!("GET http://{i}/admin HTTP/1.1\r\nHost: {i}\r\nConnection: close\r\n\r\n"),
        format!("GET /admin HTTP/1.1\r\nHost: {i}\r\nConnection: close\r\n\r\n"),
        format!("CONNECT {i} HTTP/1.1\r\nHost: {i}\r\nConnection: close\r\n\r\n"),
        format!(
            "GET /admin HTTP/1.1\r\nHost: x\r\nX-Forwarded-Host: {i}\r\nForwarded: host={i}\r\nConnection: close\r\n\r\n"
        ),
    ] {
        let _ = send(addr, request.as_bytes());
    }
    std::thread::sleep(Duration::from_millis(200));
    assert!(
        internal.accept().is_err(),
        "the proxy connected to an address a request named"
    );
}

fn leftover_then_ok(_: &str, n: usize) -> (Vec<u8>, bool) {
    if n == 0 {
        // A complete response followed by bytes that would be read as the
        // next one on a reused connection.
        (b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nfirstHTTP/1.1 200 OK\r\nContent-Length: 6\r\n\r\nforged".to_vec(), true)
    } else {
        (
            b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\n\r\nsecond".to_vec(),
            true,
        )
    }
}

fn close_announced(_: &str, _: usize) -> (Vec<u8>, bool) {
    (
        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok".to_vec(),
        true,
    )
}

#[test]
fn test_t12_a_connection_with_any_irregularity_is_not_reused() {
    // Leftover bytes after a response: the next client gets a fresh
    // connection and the genuine response, never the forged one.
    let (up, seen) = upstream(leftover_then_ok);
    let (_p, addr) = serve(
        ProxyBuilder::new()
            .pool("app", Pool::new([up.to_string()]))
            .route(Route::any().to("app")),
    );
    let first = send(
        addr,
        b"GET /1 HTTP/1.1\r\nHost: h\r\nConnection: close\r\n\r\n",
    );
    assert!(first.contains("first"), "{first}");
    let second = send(
        addr,
        b"GET /2 HTTP/1.1\r\nHost: h\r\nConnection: close\r\n\r\n",
    );
    assert!(!second.contains("forged"), "{second}");
    assert!(
        second.contains("first") || second.contains("second"),
        "{second}"
    );
    assert_eq!(
        seen.connections(),
        2,
        "the desynchronised connection was reused"
    );

    // An upstream that announces it closes: never reused either.
    let (up, seen) = upstream(close_announced);
    let (_p2, addr) = serve(
        ProxyBuilder::new()
            .pool("app", Pool::new([up.to_string()]))
            .route(Route::any().to("app")),
    );
    for _ in 0..3 {
        let r = send(
            addr,
            b"GET / HTTP/1.1\r\nHost: h\r\nConnection: close\r\n\r\n",
        );
        assert_eq!(status(&r), 200, "{r}");
    }
    assert_eq!(seen.connections(), 3);
}

fn big_head(_: &str, _: usize) -> (Vec<u8>, bool) {
    let mut r = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nX-Pad: ".to_vec();
    r.resize(r.len() + 8 * 1024, b'a');
    r.extend_from_slice(b"\r\n\r\nok");
    (r, true)
}

fn many_fields(_: &str, _: usize) -> (Vec<u8>, bool) {
    let mut r = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n".to_vec();
    for i in 0..100 {
        r.extend_from_slice(format!("X-F{i}: v\r\n").as_bytes());
    }
    r.extend_from_slice(b"\r\nok");
    (r, true)
}

fn big_body(_: &str, _: usize) -> (Vec<u8>, bool) {
    let mut r = b"HTTP/1.1 200 OK\r\nContent-Length: 100000\r\n\r\n".to_vec();
    r.resize(r.len() + 100_000, b'z');
    (r, true)
}

#[test]
fn test_t16_every_buffer_has_a_bound() {
    let limits = Limits::default()
        .max_response_head(4096)
        .max_response_fields(64)
        .max_request_body(2048)
        .request_buffer(1024)
        .max_response_body(10_000);
    let proxy = |up: SocketAddr| {
        serve(
            ProxyBuilder::new()
                .limits(limits.clone())
                .pool("app", Pool::new([up.to_string()]))
                .route(Route::any().to("app")),
        )
    };
    // A response head over the limit, and one with too many fields: 502.
    for answer in [big_head as fn(&str, usize) -> (Vec<u8>, bool), many_fields] {
        let (up, _) = upstream(answer);
        let (_p, addr) = proxy(up);
        let r = send(
            addr,
            b"GET / HTTP/1.1\r\nHost: h\r\nConnection: close\r\n\r\n",
        );
        assert_eq!(status(&r), 502, "{}", &r[..r.len().min(200)]);
    }
    // A request head the inbound side cannot hold, and a body over the
    // limit: refused, and the upstream sees neither.
    let (up, seen) = upstream(ok);
    let (_p, addr) = proxy(up);
    let mut huge = b"GET / HTTP/1.1\r\nHost: h\r\nX-Pad: ".to_vec();
    huge.resize(huge.len() + 256 * 1024, b'a');
    huge.extend_from_slice(b"\r\nConnection: close\r\n\r\n");
    let r = send(addr, &huge);
    assert!(
        matches!(status(&r), 400 | 413 | 431) || r.is_empty(),
        "{}",
        &r[..r.len().min(200)]
    );
    let r = send(
        addr,
        b"POST / HTTP/1.1\r\nHost: h\r\nContent-Length: 4096\r\nConnection: close\r\n\r\n",
    );
    assert_eq!(status(&r), 413, "{r}");
    assert!(seen.heads().is_empty(), "{:?}", seen.heads());
    // A response body over the limit is cut: the client never gets more.
    let (up, _) = upstream(big_body);
    let (_p, addr) = proxy(up);
    let r = send(
        addr,
        b"GET / HTTP/1.1\r\nHost: h\r\nConnection: close\r\n\r\n",
    );
    assert!(
        r.matches('z').count() <= 10_000 + 16 * 1024,
        "{} bytes of body",
        r.matches('z').count()
    );
    assert!(
        !r.ends_with("\r\n0\r\n\r\n"),
        "a cut body must not look complete"
    );
}

fn identifying(_: &str, _: usize) -> (Vec<u8>, bool) {
    (
        b"HTTP/1.1 200 OK\r\nServer: origin-7 (internal 10.9.8.7)\r\nContent-Length: 2\r\n\r\nok"
            .to_vec(),
        true,
    )
}

#[test]
fn test_t18_answers_do_not_name_the_topology() {
    // An endpoint nothing listens on: the 502 names a class only.
    let dead = TcpListener::bind("127.0.0.1:0").expect("bind");
    let dead_addr = dead.local_addr().expect("addr");
    drop(dead);
    let (_p, addr) = serve(
        ProxyBuilder::new()
            .pool("app", Pool::new([dead_addr.to_string()]))
            .route(Route::any().to("app")),
    );
    let r = send(
        addr,
        b"GET / HTTP/1.1\r\nHost: h\r\nConnection: close\r\n\r\n",
    );
    assert_eq!(status(&r), 502, "{r}");
    assert!(!r.contains(&dead_addr.port().to_string()), "{r}");
    assert!(!r.contains("127.0.0.1"), "{r}");
    // A response rule removes the upstream's `Server`.
    let (up, _) = upstream(identifying);
    let (_p2, addr) = serve(
        ProxyBuilder::new()
            .pool("app", Pool::new([up.to_string()]))
            .route(
                Route::any()
                    .to("app")
                    .response_header(HeaderRule::remove("server")),
            ),
    );
    let r = send(
        addr,
        b"GET / HTTP/1.1\r\nHost: h\r\nConnection: close\r\n\r\n",
    );
    assert_eq!(status(&r), 200, "{r}");
    assert!(!r.contains("10.9.8.7") && !r.contains("origin-7"), "{r}");
}

fn trailers(_: &str, _: usize) -> (Vec<u8>, bool) {
    (
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nTrailer: X-Evil\r\n\r\n2\r\nok\r\n0\r\nX-Evil: 1\r\nContent-Length: 999\r\nSet-Cookie: session=stolen\r\n\r\n"
            .to_vec(),
        true,
    )
}

#[test]
fn test_t21_trailers_never_reach_the_head_or_the_client() {
    let (up, _) = upstream(trailers);
    let (_p, addr) = serve(
        ProxyBuilder::new()
            .pool("app", Pool::new([up.to_string()]))
            .route(Route::any().to("app")),
    );
    let r = send(
        addr,
        b"GET / HTTP/1.1\r\nHost: h\r\nConnection: close\r\n\r\n",
    );
    assert_eq!(status(&r), 200, "{r}");
    let lower = r.to_ascii_lowercase();
    assert!(!lower.contains("x-evil"), "{r}");
    assert!(!lower.contains("set-cookie"), "{r}");
    assert!(!lower.contains("content-length: 999"), "{r}");
    assert!(r.ends_with("ok\r\n0\r\n\r\n"), "{r}");
}

#[test]
fn test_t22_unkeyed_client_headers_do_not_reach_the_upstream_and_nothing_is_cached() {
    let (up, seen) = upstream(ok);
    let (_p, addr) = serve(
        ProxyBuilder::new()
            .pool("app", Pool::new([up.to_string()]))
            .route(Route::any().to("app")),
    );
    let request = b"GET /page HTTP/1.1\r\nHost: site.example\r\nX-Forwarded-Host: evil.example\r\nX-Forwarded-Proto: gopher\r\nForwarded: host=evil.example\r\nX-Original-URL: /admin\r\nConnection: close\r\n\r\n";
    for _ in 0..2 {
        let r = send(addr, request);
        assert_eq!(status(&r), 200, "{r}");
    }
    let heads = seen.heads();
    // proxy-net does not cache: both requests reached the upstream.
    assert_eq!(heads.len(), 2);
    for h in heads {
        let lower = h.to_ascii_lowercase();
        assert!(!lower.contains("evil.example"), "{h}");
        assert!(!lower.contains("gopher"), "{h}");
        assert!(lower.contains("x-forwarded-host: site.example"), "{h}");
    }
}

#[cfg(feature = "tls")]
mod t19 {
    use super::*;
    use proxy_net::UpstreamTls;
    use std::sync::atomic::AtomicBool;

    const CA: &[u8] = include_bytes!("data/ca.pem");
    const CHAIN: &[u8] = include_bytes!("data/upstream.pem");
    const KEY: &[u8] = include_bytes!("data/upstream.key");

    /// A TLS upstream answering `ok`; `decrypted` is set when a request
    /// head arrived over TLS.
    fn tls_upstream() -> (SocketAddr, Arc<AtomicBool>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let server = Arc::new(tls_net::server_from_pem(CHAIN, KEY).expect("server"));
        let decrypted = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&decrypted);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                let (server, flag) = (Arc::clone(&server), Arc::clone(&flag));
                std::thread::spawn(move || {
                    let Ok(mut tls) = server.accept_stream(stream) else {
                        return;
                    };
                    let mut head = Vec::new();
                    let mut b = [0u8; 1];
                    while !head.ends_with(b"\r\n\r\n") {
                        match tls.read(&mut b) {
                            Ok(1) => head.push(b[0]),
                            _ => return,
                        }
                    }
                    flag.store(true, Ordering::SeqCst);
                    let _ = tls.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok");
                    let _ = tls.flush();
                });
            }
        });
        (addr, decrypted)
    }

    #[test]
    fn test_t19_an_upstream_with_an_untrusted_certificate_gets_no_request() {
        let (up, decrypted) = tls_upstream();
        let (_p, addr) = serve(
            ProxyBuilder::new()
                .pool("app", Pool::new([up.to_string()]).tls(UpstreamTls::new()))
                .route(Route::any().to("app")),
        );
        let r = send(
            addr,
            b"GET / HTTP/1.1\r\nHost: h\r\nConnection: close\r\n\r\n",
        );
        assert_eq!(status(&r), 502, "{r}");
        assert!(!decrypted.load(Ordering::SeqCst));
        // The same upstream with its CA trusted: the control.
        let (_p2, addr) = serve(
            ProxyBuilder::new()
                .pool(
                    "app",
                    Pool::new([up.to_string()]).tls(UpstreamTls::new().roots_pem(CA)),
                )
                .route(Route::any().to("app")),
        );
        let r = send(
            addr,
            b"GET / HTTP/1.1\r\nHost: h\r\nConnection: close\r\n\r\n",
        );
        assert_eq!(status(&r), 200, "{r}");
        assert!(decrypted.load(Ordering::SeqCst));
    }
}
