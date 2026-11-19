//! The WebSocket bridge over real sockets: messages both ways, the
//! subprotocol the upstream chose, and upstreams that answer an upgrade with
//! anything but a valid 101 (T-11).

#![cfg(feature = "ws")]

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::time::Duration;

use proxy_net::{Pool, ProxyBuilder, Route, Running};
use ws_net::{AcceptPolicy, ClientHandshake, CloseCode, Message, blocking};

/// A WebSocket upstream that echoes every data message, choosing `chat`
/// when the client offers it.
fn echo_upstream() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let policy = AcceptPolicy::new()
        .with_protocols(&["chat"])
        .expect("protocols");
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
            let policy = policy.clone();
            std::thread::spawn(move || {
                let Ok((mut ws, _)) = blocking::accept_with(stream, &policy) else {
                    return;
                };
                loop {
                    match ws.read() {
                        Ok(m) if m.is_data() => {
                            if ws.send(&m).is_err() {
                                return;
                            }
                        }
                        Ok(_) => {}
                        Err(_) => return,
                    }
                }
            });
        }
    });
    addr
}

/// An upstream that answers any request with the fixed bytes `reply`.
fn fixed_upstream(reply: &'static [u8]) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut s) = stream else { continue };
            let _ = s.set_read_timeout(Some(Duration::from_secs(5)));
            let mut head = Vec::new();
            let mut b = [0u8; 1];
            while !head.ends_with(b"\r\n\r\n") {
                match s.read(&mut b) {
                    Ok(1) => head.push(b[0]),
                    _ => break,
                }
            }
            let _ = s.write_all(reply);
        }
    });
    addr
}

fn proxy_to(upstream: SocketAddr) -> (Running, SocketAddr) {
    let running = ProxyBuilder::new()
        .pool("app", Pool::new([upstream.to_string()]))
        .route(Route::any().to("app").websocket(true))
        .build()
        .expect("build")
        .serve("127.0.0.1:0")
        .expect("serve");
    let addr = running.local_addr().expect("addr");
    (running, addr)
}

#[test]
fn test_messages_cross_the_bridge_both_ways() {
    let (_p, addr) = proxy_to(echo_upstream());
    let stream = TcpStream::connect(addr).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("timeout");
    let hs = ClientHandshake::try_new()
        .expect("handshake")
        .with_protocols(&["chat", "other"]);
    let (mut ws, protocol) =
        blocking::connect_with(stream, &addr.to_string(), "/socket", &hs).expect("upgrade");
    assert_eq!(protocol.as_deref(), Some("chat"));
    for text in ["one", "two", "three"] {
        ws.send_text(text).expect("send");
        match ws.read().expect("read") {
            Message::Text(t) => assert_eq!(t, text),
            other => panic!("expected text, got {other:?}"),
        }
    }
    ws.send_binary(&[0, 1, 2, 255]).expect("send");
    match ws.read().expect("read") {
        Message::Binary(b) => assert_eq!(b, [0, 1, 2, 255]),
        other => panic!("expected binary, got {other:?}"),
    }
    ws.close(CloseCode::NORMAL, "done").expect("close");
}

/// Sends an upgrade through the proxy by hand and returns the status line.
fn upgrade_status(addr: SocketAddr) -> String {
    upgrade_status_at(addr, "/socket")
}

fn upgrade_status_at(addr: SocketAddr, path: &str) -> String {
    let mut s = TcpStream::connect(addr).expect("connect");
    s.set_read_timeout(Some(Duration::from_secs(5)))
        .expect("timeout");
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: h\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
         Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n"
    );
    s.write_all(request.as_bytes()).expect("write");
    let mut out = Vec::new();
    let mut b = [0u8; 1];
    while !out.ends_with(b"\r\n") {
        match s.read(&mut b) {
            Ok(1) => out.push(b[0]),
            _ => break,
        }
    }
    String::from_utf8_lossy(&out).trim().to_string()
}

#[test]
fn test_an_upstream_without_a_valid_101_gets_no_tunnel() {
    // T-11: a 200, and a 101 with the wrong accept key, both end in 502;
    // no tunnel opens.
    let refuses = fixed_upstream(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");
    let (_p, addr) = proxy_to(refuses);
    assert!(upgrade_status(addr).starts_with("HTTP/1.1 502"));
    let fake = fixed_upstream(
        b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
          Sec-WebSocket-Accept: AAAAAAAAAAAAAAAAAAAAAAAAAAA=\r\n\r\n",
    );
    let (_p2, addr2) = proxy_to(fake);
    assert!(upgrade_status(addr2).starts_with("HTTP/1.1 502"));
}

#[test]
fn test_upgrades_on_a_route_without_websocket_are_refused() {
    let upstream = echo_upstream();
    let running = ProxyBuilder::new()
        .pool("app", Pool::new([upstream.to_string()]))
        .route(Route::any().to("app"))
        .build()
        .expect("build")
        .serve("127.0.0.1:0")
        .expect("serve");
    let addr = running.local_addr().expect("addr");
    let status = upgrade_status(addr);
    assert!(!status.starts_with("HTTP/1.1 101"), "{status}");
}

#[test]
fn test_upgrades_get_the_path_checks() {
    // T-7 applies to WebSocket routes too: an ambiguous path never reaches
    // the upstream.
    let (_p, addr) = proxy_to(echo_upstream());
    for path in ["/a/../b", "/a%2fb", "/a/..;/b"] {
        let status = upgrade_status_at(addr, path);
        assert!(status.starts_with("HTTP/1.1 400"), "{path}: {status}");
    }
    assert!(upgrade_status_at(addr, "/fine").starts_with("HTTP/1.1 101"));
}

#[test]
#[ignore = "serve-net 0.8 can hold messages queued for a client that is also sending until its next ping (G-16); run by hand"]
fn test_pipelined_messages_all_come_back() {
    // Every message goes out before any echo is read, so echoes are queued
    // for the client while its own messages still arrive.
    let (_p, addr) = proxy_to(echo_upstream());
    let stream = TcpStream::connect(addr).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("timeout");
    let hs = ClientHandshake::try_new().expect("handshake");
    let (mut ws, _) =
        blocking::connect_with(stream, &addr.to_string(), "/socket", &hs).expect("upgrade");
    for i in 0..200 {
        ws.send_text(&format!("message {i}")).expect("send");
    }
    for i in 0..200 {
        match ws.read().expect("read") {
            Message::Text(t) => assert_eq!(t, format!("message {i}")),
            other => panic!("at {i}: expected text, got {other:?}"),
        }
    }
    ws.close(CloseCode::NORMAL, "done").expect("close");
}
