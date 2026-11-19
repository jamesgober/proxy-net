//! TLS toward upstreams (feature `tls`): settings that cannot load refuse
//! the build, and an upstream that does not complete a verified handshake
//! gets no request.

#![cfg(all(feature = "tls", feature = "serve"))]

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use proxy_net::client::TlsConnector;
use proxy_net::{Pool, ProxyBuilder, UpstreamTls};

#[test]
fn test_roots_that_do_not_parse_refuse_the_build() {
    let tls = UpstreamTls::new().roots_pem(b"-----BEGIN CERTIFICATE-----\nnot base64\n");
    assert!(TlsConnector::new(&tls).is_err());
    let built = ProxyBuilder::new()
        .pool("app", Pool::new(["127.0.0.1:1"]).tls(tls))
        .build();
    assert!(built.is_err());
}

#[test]
fn test_a_client_certificate_that_does_not_load_refuses_the_build() {
    let tls = UpstreamTls::new().client_certificate(b"no chain", b"no key");
    assert!(TlsConnector::new(&tls).is_err());
}

#[test]
fn test_public_roots_build() {
    assert!(TlsConnector::new(&UpstreamTls::new()).is_ok());
    assert!(TlsConnector::new(&UpstreamTls::new().server_name("example.com")).is_ok());
}

/// An upstream that speaks plain HTTP, so any TLS handshake fails, and
/// notes whether a request line ever arrived in the clear.
fn plain_upstream() -> (SocketAddr, Arc<AtomicBool>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let saw_request = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&saw_request);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut s) = stream else { continue };
            let _ = s.set_read_timeout(Some(Duration::from_secs(5)));
            let mut buf = [0u8; 512];
            if let Ok(n) = s.read(&mut buf) {
                if buf[..n].starts_with(b"GET ") {
                    flag.store(true, Ordering::Release);
                }
            }
            let _ = s.write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\n\r\n");
        }
    });
    (addr, saw_request)
}

#[test]
fn test_an_upstream_without_tls_answers_502() {
    let (up, saw_request) = plain_upstream();
    let running = ProxyBuilder::new()
        .pool("app", Pool::new([up.to_string()]).tls(UpstreamTls::new()))
        .build()
        .expect("build")
        .serve("127.0.0.1:0")
        .expect("serve");
    let addr = running.local_addr().expect("addr");
    let mut s = TcpStream::connect(addr).expect("connect");
    s.set_read_timeout(Some(Duration::from_secs(10)))
        .expect("timeout");
    s.write_all(b"GET / HTTP/1.1\r\nHost: h\r\nConnection: close\r\n\r\n")
        .expect("write");
    let mut out = Vec::new();
    let _ = s.read_to_end(&mut out);
    let text = String::from_utf8_lossy(&out);
    assert!(text.starts_with("HTTP/1.1 502"), "{text}");
    assert!(!saw_request.load(Ordering::Acquire));
}
