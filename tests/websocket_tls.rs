//! The WebSocket bridge toward TLS upstreams (`wss`): the certificate is
//! verified before any upgrade is sent, and messages cross in both
//! directions while one thread reads the upstream and another writes it.

#![cfg(all(feature = "ws", feature = "tls"))]

use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use proxy_net::{Pool, ProxyBuilder, Route, Running, UpstreamTls};
use ws_net::{AcceptPolicy, ClientHandshake, CloseCode, Message, blocking};

const CA: &[u8] = include_bytes!("data/ca.pem");
const CHAIN: &[u8] = include_bytes!("data/upstream.pem");
const KEY: &[u8] = include_bytes!("data/upstream.key");

/// A `wss` upstream that echoes every data message, choosing `chat` when
/// offered. `upgraded` is set when any upgrade reached it.
fn tls_echo_upstream() -> (SocketAddr, Arc<AtomicBool>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let server = Arc::new(tls_net::server_from_pem(CHAIN, KEY).expect("server"));
    let policy = AcceptPolicy::new()
        .with_protocols(&["chat"])
        .expect("protocols");
    let upgraded = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&upgraded);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
            let (server, policy, flag) = (Arc::clone(&server), policy.clone(), Arc::clone(&flag));
            std::thread::spawn(move || {
                let Ok(tls) = server.accept_stream(stream) else {
                    return;
                };
                let Ok((mut ws, _)) = blocking::accept_with(tls, &policy) else {
                    return;
                };
                flag.store(true, Ordering::Release);
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
    (addr, upgraded)
}

fn proxy_to(upstream: SocketAddr, tls: UpstreamTls) -> (Running, SocketAddr) {
    let running = ProxyBuilder::new()
        .pool("app", Pool::new([upstream.to_string()]).tls(tls))
        .route(Route::any().to("app").websocket(true))
        .build()
        .expect("build")
        .serve("127.0.0.1:0")
        .expect("serve");
    let addr = running.local_addr().expect("addr");
    (running, addr)
}

fn client(addr: SocketAddr) -> (blocking::WebSocket<TcpStream>, Option<String>) {
    let stream = TcpStream::connect(addr).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("timeout");
    let hs = ClientHandshake::try_new()
        .expect("handshake")
        .with_protocols(&["chat"]);
    blocking::connect_with(stream, &addr.to_string(), "/socket", &hs).expect("upgrade")
}

#[test]
fn test_messages_cross_a_wss_bridge_both_ways() {
    let (up, _) = tls_echo_upstream();
    let (_p, addr) = proxy_to(up, UpstreamTls::new().roots_pem(CA));
    let (mut ws, protocol) = client(addr);
    assert_eq!(protocol.as_deref(), Some("chat"));
    for text in ["one", "two", "three"] {
        ws.send_text(text).expect("send");
        match ws.read().expect("read") {
            Message::Text(t) => assert_eq!(t, text),
            other => panic!("expected text, got {other:?}"),
        }
    }
    // Several TLS records each way, within serve-net's message limit.
    let big: Vec<u8> = (0..60_000u32).map(|i| (i % 251) as u8).collect();
    ws.send_binary(&big).expect("send");
    match ws.read().expect("read") {
        Message::Binary(b) => assert_eq!(b, big),
        other => panic!("expected binary, got {other:?}"),
    }
    ws.close(CloseCode::NORMAL, "done").expect("close");
}

#[test]
#[ignore = "serve-net 0.8 can hold messages queued for a client that is also sending until its next ping (G-16); run by hand"]
fn test_a_wss_bridge_carries_both_directions_at_once() {
    // Every message goes out before any echo is read, so the upstream's
    // replies are decrypted on the reading thread while later messages
    // are sealed on the writing one.
    let (up, _) = tls_echo_upstream();
    let (_p, addr) = proxy_to(
        up,
        UpstreamTls::new().roots_pem(CA).server_name("localhost"),
    );
    let (mut ws, _) = client(addr);
    let count = 200;
    for i in 0..count {
        ws.send_text(&format!("message {i}")).expect("send");
    }
    for i in 0..count {
        match ws.read().expect("read") {
            Message::Text(t) => assert_eq!(t, format!("message {i}")),
            other => panic!("at {i}: expected text, got {other:?}"),
        }
    }
    ws.close(CloseCode::NORMAL, "done").expect("close");
}

/// A `wss` upstream that sends ten messages, 50 ms apart, without being
/// asked.
fn tls_pushing_upstream() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let server = Arc::new(tls_net::server_from_pem(CHAIN, KEY).expect("server"));
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let server = Arc::clone(&server);
            std::thread::spawn(move || {
                let Ok(tls) = server.accept_stream(stream) else {
                    return;
                };
                let Ok((mut ws, _)) = blocking::accept_with(tls, &AcceptPolicy::new()) else {
                    return;
                };
                for i in 0..10 {
                    std::thread::sleep(Duration::from_millis(50));
                    if ws.send_text(&format!("push {i}")).is_err() {
                        return;
                    }
                }
                let _ = ws.read();
            });
        }
    });
    addr
}

#[test]
fn test_a_wss_upstream_reaches_a_silent_client() {
    // The reading thread delivers while nothing is written.
    let (_p, addr) = proxy_to(tls_pushing_upstream(), UpstreamTls::new().roots_pem(CA));
    let stream = TcpStream::connect(addr).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("timeout");
    let hs = ClientHandshake::try_new().expect("handshake");
    let (mut ws, _) =
        blocking::connect_with(stream, &addr.to_string(), "/socket", &hs).expect("upgrade");
    let start = std::time::Instant::now();
    for i in 0..10 {
        match ws.read().expect("read") {
            Message::Text(t) => assert_eq!(t, format!("push {i}")),
            other => panic!("expected text, got {other:?}"),
        }
    }
    // Ten pushes 50 ms apart, well inside serve-net's ping interval.
    assert!(start.elapsed() < Duration::from_secs(5));
    ws.close(CloseCode::NORMAL, "done").expect("close");
}

#[test]
fn test_a_wss_upstream_with_an_untrusted_certificate_gets_no_upgrade() {
    // The public roots do not include the test CA: the handshake fails, the
    // client gets 502, and no upgrade reaches the upstream.
    let (up, upgraded) = tls_echo_upstream();
    let (_p, addr) = proxy_to(up, UpstreamTls::new());
    let stream = TcpStream::connect(addr).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("timeout");
    let hs = ClientHandshake::try_new().expect("handshake");
    let result = blocking::connect_with(stream, &addr.to_string(), "/socket", &hs);
    assert!(result.is_err());
    assert!(!upgraded.load(Ordering::Acquire));
}

#[test]
fn test_a_wss_upstream_for_another_name_gets_no_upgrade() {
    let (up, upgraded) = tls_echo_upstream();
    let (_p, addr) = proxy_to(
        up,
        UpstreamTls::new().roots_pem(CA).server_name("other.test"),
    );
    let stream = TcpStream::connect(addr).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("timeout");
    let hs = ClientHandshake::try_new().expect("handshake");
    assert!(blocking::connect_with(stream, &addr.to_string(), "/socket", &hs).is_err());
    assert!(!upgraded.load(Ordering::Acquire));
}
