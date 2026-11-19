//! Attack tests over real sockets. A recording upstream parses everything it
//! receives with http-net, so each test can say exactly which requests
//! reached the origin.

#![cfg(feature = "serve")]

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use http_net::{Response, ServerConn, ServerEvent};
use proxy_net::{Pool, ProxyBuilder, Running};

/// What a recording upstream saw: each request's method, target, and body.
type Seen = Arc<Mutex<Vec<(String, String, Vec<u8>)>>>;

fn recording_upstream() -> (SocketAddr, Seen) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let seen: Seen = Arc::default();
    let record = Arc::clone(&seen);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut s) = stream else { continue };
            let record = Arc::clone(&record);
            std::thread::spawn(move || {
                let _ = s.set_read_timeout(Some(Duration::from_secs(5)));
                let mut conn = ServerConn::new();
                let mut buf = [0u8; 8192];
                let mut current: Option<(String, String, Vec<u8>)> = None;
                loop {
                    let mut answer = false;
                    loop {
                        match conn.poll() {
                            Ok(ServerEvent::Request(head)) => {
                                current = Some((
                                    head.method_str().to_string(),
                                    head.target().to_string(),
                                    Vec::new(),
                                ));
                            }
                            Ok(ServerEvent::Body(b)) => {
                                if let Some(c) = current.as_mut() {
                                    c.2.extend_from_slice(b);
                                }
                            }
                            Ok(ServerEvent::RequestEnd) => {
                                if let Some(c) = current.take() {
                                    record.lock().expect("lock").push(c);
                                }
                                answer = true;
                                break;
                            }
                            Ok(ServerEvent::NeedInput) => break,
                            Ok(_) => break,
                            Err(_) => return,
                        }
                    }
                    if answer {
                        let mut out = Vec::new();
                        if conn
                            .respond(&Response::new(200).body(&b"ok"[..]), &mut out)
                            .is_err()
                            || s.write_all(&out).is_err()
                        {
                            return;
                        }
                        continue;
                    }
                    match s.read(&mut buf) {
                        Ok(0) | Err(_) => return,
                        Ok(n) => {
                            let _ = conn.feed(&buf[..n]);
                        }
                    }
                }
            });
        }
    });
    (addr, seen)
}

fn proxy_to(up: SocketAddr) -> (Running, SocketAddr) {
    let running = ProxyBuilder::new()
        .pool("app", Pool::new([up.to_string()]))
        .build()
        .expect("build")
        .serve("127.0.0.1:0")
        .expect("serve");
    let addr = running.local_addr().expect("addr");
    (running, addr)
}

fn send(addr: SocketAddr, raw: &[u8]) -> String {
    let mut s = TcpStream::connect(addr).expect("connect");
    s.set_read_timeout(Some(Duration::from_secs(5)))
        .expect("timeout");
    s.write_all(raw).expect("write");
    let mut out = Vec::new();
    let _ = s.read_to_end(&mut out);
    String::from_utf8_lossy(&out).into_owned()
}

fn status(answer: &str) -> u16 {
    answer.get(9..12).and_then(|s| s.parse().ok()).unwrap_or(0)
}

fn settle(seen: &Seen, n: usize) -> Vec<(String, String, Vec<u8>)> {
    let deadline = Instant::now() + Duration::from_millis(500);
    loop {
        let v = seen.lock().expect("lock").clone();
        if v.len() >= n || Instant::now() >= deadline {
            return v;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn test_t1_smuggling_vectors_never_reach_the_upstream() {
    let (up, seen) = recording_upstream();
    let (_p, addr) = proxy_to(up);
    let vectors: [&[u8]; 6] = [
        // CL.TE
        b"POST / HTTP/1.1\r\nHost: h\r\nContent-Length: 6\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\nG",
        // TE.TE with an obfuscated second coding
        b"POST / HTTP/1.1\r\nHost: h\r\nTransfer-Encoding: chunked\r\nTransfer-Encoding: x\r\n\r\n0\r\n\r\n",
        // Two different lengths
        b"POST / HTTP/1.1\r\nHost: h\r\nContent-Length: 3\r\nContent-Length: 5\r\n\r\nabcde",
        // A bare LF in the head
        b"POST / HTTP/1.1\nHost: h\r\nContent-Length: 0\r\n\r\n",
        // Obsolete line folding
        b"GET / HTTP/1.1\r\nHost: h\r\nX-A: a\r\n b\r\n\r\n",
        // 0.CL: a GET whose body is a second request (T-4)
        b"GET / HTTP/1.1\r\nHost: h\r\nContent-Length: 34\r\n\r\nGET /admin HTTP/1.1\r\nHost: h\r\n\r\n",
    ];
    for v in vectors {
        let answer = send(addr, v);
        assert!(
            (400..=501).contains(&status(&answer)),
            "{:?} -> {answer}",
            String::from_utf8_lossy(v)
        );
    }
    assert!(
        settle(&seen, 1).is_empty(),
        "{:?}",
        seen.lock().expect("lock")
    );
}

#[test]
fn test_t1_a_request_in_a_body_stays_a_body() {
    let (up, seen) = recording_upstream();
    let (_p, addr) = proxy_to(up);
    let inner = b"GET /admin HTTP/1.1\r\nHost: h\r\n\r\n";
    let mut req = format!(
        "POST /upload HTTP/1.1\r\nHost: h\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        inner.len()
    )
    .into_bytes();
    req.extend_from_slice(inner);
    assert_eq!(status(&send(addr, &req)), 200);
    let v = settle(&seen, 1);
    assert_eq!(v.len(), 1, "{v:?}");
    assert_eq!((v[0].0.as_str(), v[0].1.as_str()), ("POST", "/upload"));
    assert_eq!(v[0].2, inner);
    // Nothing ever asked for /admin.
    assert!(settle(&seen, 2).iter().all(|r| r.1 != "/admin"));
}

#[test]
fn test_pipelined_requests_are_forwarded_one_by_one() {
    let (up, seen) = recording_upstream();
    let (_p, addr) = proxy_to(up);
    let answer = send(
        addr,
        b"GET /one HTTP/1.1\r\nHost: h\r\n\r\nGET /two HTTP/1.1\r\nHost: h\r\nConnection: close\r\n\r\n",
    );
    assert_eq!(answer.matches("HTTP/1.1 200").count(), 2, "{answer}");
    let v = settle(&seen, 2);
    let targets: Vec<&str> = v.iter().map(|r| r.1.as_str()).collect();
    assert_eq!(targets, ["/one", "/two"]);
}

#[test]
fn test_t13_a_small_body_is_held_before_the_upstream_sees_anything() {
    let (up, seen) = recording_upstream();
    let (_p, addr) = proxy_to(up);
    let mut s = TcpStream::connect(addr).expect("connect");
    s.set_read_timeout(Some(Duration::from_secs(5)))
        .expect("timeout");
    s.write_all(
        b"POST /slow HTTP/1.1\r\nHost: h\r\nContent-Length: 10\r\nConnection: close\r\n\r\nabc",
    )
    .expect("write");
    std::thread::sleep(Duration::from_millis(300));
    // The body is not complete: the upstream has seen no request at all.
    assert!(seen.lock().expect("lock").is_empty());
    s.write_all(b"defghij").expect("write");
    let mut out = Vec::new();
    let _ = s.read_to_end(&mut out);
    assert_eq!(status(&String::from_utf8_lossy(&out)), 200);
    let v = settle(&seen, 1);
    assert_eq!(v[0].2, b"abcdefghij");
}
