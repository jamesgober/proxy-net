//! Real sockets: a client, the proxy (on serve-net), and upstreams (serve-net
//! servers, or raw listeners that misbehave on purpose).

#![cfg(feature = "serve")]

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use proxy_net::core::EndpointState;
use proxy_net::{
    EndpointAddr, HealthCheck, Observer, Pool, ProxyBuilder, Retry, Route, Running, Timeouts,
};

/// An upstream that answers with what it received.
fn echo_upstream() -> (serve_net::Running, SocketAddr) {
    let server = serve_net::http("127.0.0.1:0", |req, reply| {
        if req.path() == "/big" {
            let mut stream = reply.stream();
            let _ = stream.send(&vec![b'z'; 2 * 1024 * 1024]);
            return;
        }
        let h = |n: &str| String::from_utf8_lossy(req.header(n).unwrap_or(b"-")).into_owned();
        let text = format!(
            "method={}\npath={}\nquery={}\nhost={}\nxff={}\nfwd={}\nvia={}\nsecret={}\nbody={}\n",
            req.head().method_str(),
            req.path(),
            req.query().unwrap_or("-"),
            h("host"),
            h("x-forwarded-for"),
            h("forwarded"),
            h("via"),
            h("x-secret"),
            req.body_bytes().len(),
        );
        let _ = reply.header("x-upstream", "echo").text(200, &text);
    })
    .expect("upstream");
    let addr = server.local_addr().expect("addr");
    (server, addr)
}

/// A raw upstream: reads one request head per connection and answers with
/// `answer(path)`; `None` answers nothing and holds the connection.
fn raw_upstream(answer: fn(&str) -> Option<Vec<u8>>) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut s) = stream else { continue };
            std::thread::spawn(move || {
                let _ = s.set_read_timeout(Some(Duration::from_secs(5)));
                let mut head = Vec::new();
                let mut b = [0u8; 1];
                while !head.ends_with(b"\r\n\r\n") {
                    match s.read(&mut b) {
                        Ok(1) => head.push(b[0]),
                        _ => return,
                    }
                }
                let text = String::from_utf8_lossy(&head).into_owned();
                let path = text.split(' ').nth(1).unwrap_or("/").to_string();
                match answer(&path) {
                    Some(bytes) => {
                        let _ = s.write_all(&bytes);
                        std::thread::sleep(Duration::from_millis(200));
                    }
                    None => std::thread::sleep(Duration::from_secs(3)),
                }
            });
        }
    });
    addr
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

/// Sends `raw` and reads the whole answer (requests ask for `close`).
fn send(addr: SocketAddr, raw: &[u8]) -> String {
    let mut s = TcpStream::connect(addr).expect("connect");
    s.set_read_timeout(Some(Duration::from_secs(10)))
        .expect("timeout");
    s.write_all(raw).expect("write");
    let mut out = Vec::new();
    let _ = s.read_to_end(&mut out);
    String::from_utf8_lossy(&out).into_owned()
}

fn get(addr: SocketAddr, path: &str) -> String {
    send(
        addr,
        format!("GET {path} HTTP/1.1\r\nHost: example.com\r\nConnection: close\r\n\r\n").as_bytes(),
    )
}

fn status(answer: &str) -> u16 {
    answer.get(9..12).and_then(|s| s.parse().ok()).unwrap_or(0)
}

/// The body of a chunked or length-framed answer (for the echo upstream's
/// small text replies).
fn body(answer: &str) -> String {
    let Some(i) = answer.find("\r\n\r\n") else {
        return String::new();
    };
    let (head, rest) = (&answer[..i], &answer[i + 4..]);
    if head
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked")
    {
        let mut out = String::new();
        let mut r = rest;
        while let Some(j) = r.find("\r\n") {
            let n = usize::from_str_radix(r[..j].trim(), 16).unwrap_or(0);
            if n == 0 {
                break;
            }
            out.push_str(&r[j + 2..j + 2 + n]);
            r = &r[j + 2 + n + 2..];
        }
        out
    } else {
        rest.to_string()
    }
}

#[test]
fn test_get_is_forwarded_with_rebuilt_forwarding_headers() {
    let (_up, up) = echo_upstream();
    let (_p, addr) = serve(ProxyBuilder::new().pool("app", Pool::new([up.to_string()])));
    let answer = send(
        addr,
        b"GET /a/b?x=1 HTTP/1.1\r\nHost: example.com\r\nX-Forwarded-For: 6.6.6.6\r\n\
          Forwarded: for=6.6.6.6\r\nConnection: close, x-secret\r\nX-Secret: s\r\n\r\n",
    );
    assert_eq!(status(&answer), 200, "{answer}");
    let b = body(&answer);
    assert!(
        b.contains("path=/a/b\n") && b.contains("query=x=1\n"),
        "{b}"
    );
    assert!(b.contains("host=example.com\n"), "{b}");
    // T-3: the client's values are gone; the proxy wrote its own.
    assert!(b.contains("xff=127.0.0.1\n"), "{b}");
    assert!(
        b.contains("fwd=for=127.0.0.1;proto=http;host=example.com\n"),
        "{b}"
    );
    assert!(b.contains("via=1.1 proxy-net\n"), "{b}");
    // A field the client named in Connection is hop-by-hop.
    assert!(b.contains("secret=-\n"), "{b}");
    // The response carries Via and the upstream's own fields.
    let lower = answer.to_ascii_lowercase();
    assert!(lower.contains("via: 1.1 proxy-net"), "{answer}");
    assert!(lower.contains("x-upstream: echo"), "{answer}");
}

#[test]
fn test_small_and_large_bodies_reach_the_upstream() {
    let (_up, up) = echo_upstream();
    let (_p, addr) = serve(ProxyBuilder::new().pool("app", Pool::new([up.to_string()])));
    for size in [1usize, 1000, 64 * 1024, 200 * 1024] {
        let mut req = format!(
            "POST /upload HTTP/1.1\r\nHost: h\r\nContent-Length: {size}\r\nConnection: close\r\n\r\n"
        )
        .into_bytes();
        req.extend(std::iter::repeat_n(b'a', size));
        let answer = send(addr, &req);
        assert_eq!(status(&answer), 200, "{size}: {answer}");
        assert!(body(&answer).contains(&format!("body={size}\n")), "{size}");
    }
    // A chunked request body.
    let answer = send(
        addr,
        b"POST /c HTTP/1.1\r\nHost: h\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n\
          5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n",
    );
    assert_eq!(status(&answer), 200, "{answer}");
    assert!(body(&answer).contains("body=11\n"), "{answer}");
}

#[test]
fn test_large_response_is_streamed_whole() {
    let (_up, up) = echo_upstream();
    let (_p, addr) = serve(ProxyBuilder::new().pool("app", Pool::new([up.to_string()])));
    let answer = get(addr, "/big");
    assert_eq!(status(&answer), 200);
    assert_eq!(body(&answer).len(), 2 * 1024 * 1024);
}

#[derive(Default)]
struct Reuse(AtomicU64, AtomicU64);

impl Observer for Reuse {
    fn connected(&self, _e: &EndpointAddr, _took: Duration, reused: bool) {
        let c = if reused { &self.0 } else { &self.1 };
        let _ = c.fetch_add(1, Ordering::Relaxed);
    }
}

#[derive(Clone, Default)]
struct Shared(Arc<Reuse>);

impl Observer for Shared {
    fn connected(&self, e: &EndpointAddr, took: Duration, reused: bool) {
        self.0.connected(e, took, reused);
    }
}

#[test]
fn test_upstream_connections_are_reused() {
    let (_up, up) = echo_upstream();
    let seen = Shared::default();
    let (_p, addr) = serve(
        ProxyBuilder::new()
            .pool("app", Pool::new([up.to_string()]))
            .observer(seen.clone()),
    );
    for _ in 0..5 {
        assert_eq!(status(&get(addr, "/")), 200);
    }
    let (reused, new) = (
        seen.0.0.load(Ordering::Relaxed),
        seen.0.1.load(Ordering::Relaxed),
    );
    assert_eq!(reused + new, 5);
    assert!(reused >= 3, "reused {reused}, new {new}");
}

#[test]
fn test_a_dead_endpoint_is_retried_for_every_method() {
    // Port 1 on loopback refuses connections: the request never leaves, so
    // GET and POST both retry on the live endpoint.
    let (_up, up) = echo_upstream();
    let (p, addr) = serve(ProxyBuilder::new().pool(
        "app",
        Pool::new(["127.0.0.1:1".to_string(), up.to_string()]),
    ));
    for i in 0..6 {
        assert_eq!(status(&get(addr, "/")), 200, "GET {i}");
        let answer = send(
            addr,
            b"POST / HTTP/1.1\r\nHost: h\r\nContent-Length: 2\r\nConnection: close\r\n\r\nhi",
        );
        assert_eq!(status(&answer), 200, "POST {i}: {answer}");
    }
    assert!(p.handle().stats().retries >= 1);
}

#[test]
fn test_no_answer_is_504() {
    let silent = raw_upstream(|_| None);
    let (_p, addr) = serve(
        ProxyBuilder::new().pool(
            "app",
            Pool::new([silent.to_string()])
                .timeouts(Timeouts::default().response_head(Duration::from_millis(300)))
                .retry(Retry::off()),
        ),
    );
    let started = Instant::now();
    let answer = get(addr, "/");
    assert_eq!(status(&answer), 504, "{answer}");
    assert!(started.elapsed() < Duration::from_secs(3));
}

#[test]
fn test_bytes_after_a_response_count_as_desync() {
    // T-2, idea I-2: the extra bytes never reach the client and the
    // connection is not reused.
    let up = raw_upstream(|_| {
        Some(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nokHTTP/1.1 200 OK\r\n\r\n".to_vec())
    });
    let (p, addr) = serve(ProxyBuilder::new().pool("app", Pool::new([up.to_string()])));
    let answer = get(addr, "/");
    assert_eq!(status(&answer), 200, "{answer}");
    assert_eq!(body(&answer), "ok");
    let deadline = Instant::now() + Duration::from_secs(2);
    while p.handle().stats().desyncs == 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(p.handle().stats().desyncs, 1);
}

#[test]
fn test_ambiguous_upstream_framing_is_502() {
    // T-2: a response with both Content-Length and Transfer-Encoding.
    let up = raw_upstream(|_| {
        Some(b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n0\r\n\r\n".to_vec())
    });
    let (_p, addr) = serve(
        ProxyBuilder::new()
            .pool("app", Pool::new([up.to_string()]))
            .route(Route::any().to("app")),
    );
    assert_eq!(status(&get(addr, "/")), 502);
}

#[test]
fn test_refusals_before_forwarding() {
    let (_up, up) = echo_upstream();
    let (p, addr) = serve(
        ProxyBuilder::new()
            .pool("app", Pool::new([up.to_string()]))
            .route(Route::host("example.com").path("/a").to("app")),
    );
    assert_eq!(status(&get(addr, "/b")), 404);
    assert_eq!(status(&get(addr, "/a/../admin")), 400);
    assert_eq!(
        status(&send(
            addr,
            b"GET /a HTTP/1.1\r\nHost: other.org\r\nConnection: close\r\n\r\n"
        )),
        404
    );
    assert_eq!(
        status(&send(addr, b"GET /a HTTP/1.1\r\nHost: example.com\r\nContent-Length: 2\r\nConnection: close\r\n\r\nhi")),
        400
    );
    assert_eq!(
        status(&send(
            addr,
            b"TRACE /a HTTP/1.1\r\nHost: example.com\r\nConnection: close\r\n\r\n"
        )),
        405
    );
    assert_eq!(
        status(&send(addr, b"GET /a HTTP/1.1\r\nHost: example.com\r\nVia: 1.1 proxy-net\r\nConnection: close\r\n\r\n")),
        508
    );
    assert!(p.handle().stats().refused >= 6);
}

#[test]
fn test_active_health_check_takes_a_failing_endpoint_out() {
    let (_good, good) = echo_upstream();
    let bad = raw_upstream(|path| {
        if path == "/health" {
            Some(b"HTTP/1.1 500 Error\r\nContent-Length: 0\r\n\r\n".to_vec())
        } else {
            Some(b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\nbad".to_vec())
        }
    });
    let (p, addr) = serve(
        ProxyBuilder::new().pool(
            "app",
            Pool::new([good.to_string(), bad.to_string()]).health(
                HealthCheck::http("/health")
                    .interval(Duration::from_millis(100))
                    .timeout(Duration::from_millis(80))
                    .rise(1)
                    .fall(1),
            ),
        ),
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let h = p.handle().health();
        let states: Vec<EndpointState> = h.pools[0].endpoints.iter().map(|e| e.state).collect();
        if states == [EndpointState::Healthy, EndpointState::Unhealthy] {
            break;
        }
        assert!(Instant::now() < deadline, "{states:?}");
        std::thread::sleep(Duration::from_millis(20));
    }
    for _ in 0..10 {
        let answer = get(addr, "/");
        assert_eq!(status(&answer), 200);
        assert!(body(&answer).starts_with("method="), "{answer}");
    }
}

#[test]
fn test_tier1_proxy_function() {
    let (_up, up) = echo_upstream();
    let running = proxy_net::proxy("127.0.0.1:0", [up.to_string()]).expect("proxy");
    let addr = running.local_addr().expect("addr");
    assert_eq!(status(&get(addr, "/")), 200);
    let report = running.drain(Duration::from_secs(2));
    let _ = report;
}
