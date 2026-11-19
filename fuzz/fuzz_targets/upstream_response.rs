//! A whole exchange through the forwarder against an upstream that answers
//! with arbitrary bytes (an in-memory connector). Oracle: no panic; a
//! response head that gets through has a valid status and no hop-by-hop or
//! framing field; reading the body ends (with the response or a failure)
//! without panicking.

#![no_main]

use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use libfuzzer_sys::fuzz_target;
use proxy_net::client::{Connector, RequestParts, UpstreamIo};
use proxy_net::core::{ErrorClass, TimeoutKind};
use proxy_net::{EndpointAddr, Pool, PoolLimits, Proxy, ProxyBuilder, Retry, Timeouts};

static INPUT: Mutex<Vec<u8>> = Mutex::new(Vec::new());

struct Canned {
    data: Vec<u8>,
    at: usize,
}

impl UpstreamIo for Canned {
    fn send(&mut self, _bytes: &[u8], _deadline: Instant) -> Result<(), ErrorClass> {
        Ok(())
    }
    fn recv(&mut self, buf: &mut [u8], _deadline: Instant, _kind: TimeoutKind) -> Result<usize, ErrorClass> {
        // Hand the bytes out in small, uneven pieces.
        let n = (self.data.len() - self.at).min(buf.len()).min(1 + self.at % 7);
        buf[..n].copy_from_slice(&self.data[self.at..self.at + n]);
        self.at += n;
        Ok(n)
    }
    fn is_reusable(&mut self) -> bool {
        false
    }
    fn close(&mut self) {}
}

struct FromInput;

impl Connector for FromInput {
    fn connect(&self, _e: &EndpointAddr, _p: &[u8], _d: Instant) -> Result<Box<dyn UpstreamIo>, ErrorClass> {
        let data = INPUT.lock().map(|g| g.clone()).unwrap_or_default();
        Ok(Box::new(Canned { data, at: 0 }))
    }
}

fn proxy() -> &'static Proxy {
    static P: OnceLock<Proxy> = OnceLock::new();
    P.get_or_init(|| {
        ProxyBuilder::new()
            .pool(
                "p",
                Pool::new(["10.0.0.1:80"])
                    .retry(Retry::off())
                    .limits(PoolLimits::default().max_idle(0))
                    .timeouts(Timeouts::default().response_head(Duration::from_secs(1))),
            )
            .connector(FromInput)
            .build()
            .expect("the fixed configuration builds")
    })
}

const HOP: [&str; 6] = ["connection", "keep-alive", "transfer-encoding", "content-length", "upgrade", "te"];

fuzz_target!(|data: &[u8]| {
    if let Ok(mut g) = INPUT.lock() {
        g.clear();
        g.extend_from_slice(data);
    }
    let parts = RequestParts {
        method: "GET".into(),
        target: "/".into(),
        host: b"example.com"[..].into(),
        ..RequestParts::default()
    };
    let Some(mut forward) = proxy().forward(0, parts) else {
        return;
    };
    if forward.push_body(b"", true).is_err() {
        return;
    }
    let Ok(head) = forward.response() else {
        return;
    };
    assert!((200..=599).contains(&head.status), "{}", head.status);
    for (name, _) in head.fields.iter() {
        assert!(!HOP.contains(&name), "{name}");
    }
    let mut out = Vec::new();
    for _ in 0..10_000 {
        out.clear();
        match forward.read_body(&mut out, 4096) {
            Ok(true) | Err(_) => return,
            Ok(false) => {}
        }
    }
});
