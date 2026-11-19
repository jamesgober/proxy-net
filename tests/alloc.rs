//! Heap allocations per proxied request, counted with a global allocator.
//!
//! The origin and the client in this test allocate nothing per request, so
//! every allocation counted in the measured window is the proxy's (serve-net,
//! http-net, and proxy-net together). The bound is checked on every platform
//! so a change that adds allocations to the request path fails here.

#![cfg(feature = "serve")]

use std::alloc::{GlobalAlloc, Layout, System};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

struct Counting;

static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);
static BYTES: AtomicU64 = AtomicU64::new(0);
static ON: AtomicBool = AtomicBool::new(false);

// SAFETY: forwards to the system allocator unchanged; the counters are
// relaxed atomics, which allocate nothing.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if ON.load(Ordering::Relaxed) {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
            BYTES.fetch_add(layout.size() as u64, Ordering::Relaxed);
        }
        // SAFETY: the caller's layout, as given.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: `ptr` came from `alloc` above with this layout.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if ON.load(Ordering::Relaxed) {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
            BYTES.fetch_add(new_size as u64, Ordering::Relaxed);
        }
        // SAFETY: as for `dealloc`; `new_size` is the caller's.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

const REPLY: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Length: 13\r\n\r\nhello, world\n";

/// A keep-alive origin that answers every request head with `REPLY`, using
/// fixed buffers only.
fn origin() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut s) = stream else { continue };
            std::thread::spawn(move || {
                let _ = s.set_nodelay(true);
                let mut buf = [0u8; 8192];
                let mut filled = 0;
                loop {
                    let n = match s.read(&mut buf[filled..]) {
                        Ok(0) | Err(_) => return,
                        Ok(n) => n,
                    };
                    filled += n;
                    while let Some(end) = find_head_end(&buf[..filled]) {
                        if s.write_all(REPLY).is_err() {
                            return;
                        }
                        buf.copy_within(end..filled, 0);
                        filled -= end;
                    }
                    if filled == buf.len() {
                        return;
                    }
                }
            });
        }
    });
    addr
}

fn find_head_end(b: &[u8]) -> Option<usize> {
    b.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4)
}

/// Sends `count` keep-alive requests on `s` and reads each response.
fn exchange(s: &mut TcpStream, count: usize) {
    const REQUEST: &[u8] = b"GET /a HTTP/1.1\r\nHost: example.test\r\n\r\n";
    let mut buf = [0u8; 4096];
    for _ in 0..count {
        s.write_all(REQUEST).expect("write");
        let mut filled = 0;
        loop {
            let n = s.read(&mut buf[filled..]).expect("read");
            assert!(n > 0, "closed");
            filled += n;
            // serve-net 0.8 sends every streamed reply chunked (gap G-5).
            if buf[..filled].ends_with(b"\r\n0\r\n\r\n") {
                break;
            }
        }
        assert!(buf.starts_with(b"HTTP/1.1 200"));
    }
}

/// The most heap allocations one proxied keep-alive GET may make in steady
/// state, across serve-net, http-net, and proxy-net. Measured at 10 in 0.6:
/// 7 build http-net's `Request` (it cannot be reused, gap G-17), and 3 are
/// the method, target, and `Host` copies in `RequestParts`. serve-net and
/// http-net's response parsing make none.
const MAX_PER_REQUEST: u64 = 12;

#[test]
fn test_allocations_per_proxied_request_are_bounded() {
    let up = origin();
    let running = proxy_net::proxy("127.0.0.1:0", [up.to_string()]).expect("proxy");
    let addr = running.local_addr().expect("addr");
    ALLOCATIONS.store(0, Ordering::SeqCst);
    BYTES.store(0, Ordering::SeqCst);
    // serve-net ends a connection after 1000 requests: measure 800 on each
    // of four, after a warm-up that fills the upstream pool and grows the
    // buffers to their working size.
    let (connections, each) = (4u64, 800u64);
    for _ in 0..connections {
        let mut s = TcpStream::connect(addr).expect("connect");
        s.set_nodelay(true).expect("nodelay");
        s.set_read_timeout(Some(Duration::from_secs(10)))
            .expect("timeout");
        exchange(&mut s, 50);
        ON.store(true, Ordering::SeqCst);
        exchange(&mut s, each as usize);
        ON.store(false, Ordering::SeqCst);
    }
    let requests = connections * each;
    let count = ALLOCATIONS.load(Ordering::SeqCst);
    let bytes = BYTES.load(Ordering::SeqCst);
    let per = count as f64 / requests as f64;
    println!(
        "allocations per request: {per:.2} ({count} in {requests}); bytes per request: {:.0}",
        bytes as f64 / requests as f64
    );
    assert!(
        count <= MAX_PER_REQUEST * requests,
        "{per:.2} allocations per request, bound {MAX_PER_REQUEST}"
    );
}
