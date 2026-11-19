//! Several proxies at once over real sockets: a chain of two, a loop
//! between them, and an edge balancing over two inner proxies while one of
//! them stops.

#![cfg(feature = "serve")]

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use proxy_net::{Pool, ProxyBuilder, Route, Running};

/// An origin that records each request head and answers with its own name.
fn origin(name: &'static str) -> (SocketAddr, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let heads: Arc<Mutex<Vec<String>>> = Arc::default();
    let record = Arc::clone(&heads);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut s) = stream else { continue };
            let record = Arc::clone(&record);
            std::thread::spawn(move || {
                let _ = s.set_read_timeout(Some(Duration::from_secs(5)));
                loop {
                    let mut head = Vec::new();
                    let mut b = [0u8; 1];
                    while !head.ends_with(b"\r\n\r\n") {
                        match s.read(&mut b) {
                            Ok(1) => head.push(b[0]),
                            _ => return,
                        }
                    }
                    record
                        .lock()
                        .expect("lock")
                        .push(String::from_utf8_lossy(&head).into_owned());
                    let reply = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{name}",
                        name.len()
                    );
                    if s.write_all(reply.as_bytes()).is_err() {
                        return;
                    }
                }
            });
        }
    });
    (addr, heads)
}

fn proxy(pseudonym: &str, upstreams: &[SocketAddr]) -> (Running, SocketAddr) {
    let running = ProxyBuilder::new()
        .via(pseudonym)
        .pool("next", Pool::new(upstreams.iter().map(ToString::to_string)))
        .route(Route::any().to("next"))
        .build()
        .expect("build")
        .serve("127.0.0.1:0")
        .expect("serve");
    let addr = running.local_addr().expect("addr");
    (running, addr)
}

fn get(addr: SocketAddr) -> String {
    let mut s = TcpStream::connect(addr).expect("connect");
    s.set_read_timeout(Some(Duration::from_secs(10)))
        .expect("timeout");
    s.write_all(b"GET /x HTTP/1.1\r\nHost: site.example\r\nConnection: close\r\n\r\n")
        .expect("write");
    let mut out = Vec::new();
    let _ = s.read_to_end(&mut out);
    String::from_utf8_lossy(&out).into_owned()
}

#[test]
fn test_a_chain_of_two_proxies_adds_each_hop_once() {
    let (origin_addr, heads) = origin("origin");
    let (_inner, inner) = proxy("inner", &[origin_addr]);
    let (_edge, edge) = proxy("edge", &[inner]);
    let r = get(edge);
    // On the way back each proxy adds its Via too, nearest first.
    let lower = r.to_ascii_lowercase();
    assert!(lower.find("via: 1.1 inner") < lower.find("via: 1.1 edge"), "{r}");
    assert!(
        r.starts_with("HTTP/1.1 200") && r.contains("origin"),
        "{r}"
    );
    let heads = heads.lock().expect("lock").clone();
    assert_eq!(heads.len(), 1);
    let h = heads[0].to_ascii_lowercase();
    // Each proxy adds its own Via, in order, and only once.
    assert!(h.contains("1.1 edge") && h.contains("1.1 inner"), "{h}");
    assert!(h.find("1.1 edge") < h.find("1.1 inner"), "{h}");
    assert_eq!(h.matches("1.1 edge").count(), 1, "{h}");
    // The client's Host goes through both.
    assert!(h.contains("\r\nhost: site.example"), "{h}");
}

#[test]
fn test_a_loop_between_two_proxies_ends_with_508() {
    // `a` forwards to `b`, and `b` back to `a`: the request comes back to
    // `a` carrying a's own Via, and `a` refuses it.
    let placeholder = TcpListener::bind("127.0.0.1:0").expect("bind");
    let a_listen = placeholder.local_addr().expect("addr");
    drop(placeholder);
    let (_b, b) = proxy("b", &[a_listen]);
    let a = ProxyBuilder::new()
        .via("a")
        .pool("next", Pool::new([b.to_string()]))
        .route(Route::any().to("next"))
        .build()
        .expect("build")
        .serve(a_listen)
        .expect("serve");
    let r = get(a_listen);
    assert!(r.starts_with("HTTP/1.1 508"), "{r}");
    drop(a);
}

#[test]
fn test_an_edge_keeps_serving_when_one_inner_proxy_stops() {
    let (origin_addr, _heads) = origin("origin");
    let (inner1_running, inner1) = proxy("inner1", &[origin_addr]);
    let (_inner2_running, inner2) = proxy("inner2", &[origin_addr]);
    let (_edge, edge) = proxy("edge", &[inner1, inner2]);
    for _ in 0..10 {
        let r = get(edge);
        assert!(r.starts_with("HTTP/1.1 200"), "{r}");
    }
    // One inner proxy goes away; idempotent requests that pick it fail to
    // connect and are retried on the other, and it is ejected.
    drop(inner1_running);
    std::thread::sleep(Duration::from_millis(200));
    let mut ok = 0;
    for _ in 0..40 {
        if get(edge).starts_with("HTTP/1.1 200") {
            ok += 1;
        }
    }
    assert_eq!(ok, 40, "requests failed after one inner proxy stopped");
}
