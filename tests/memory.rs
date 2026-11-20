//! Heap held per active exchange: many requests in flight at once, the
//! origin holding each response, and the live heap compared with the idle
//! proxy's.
//!
//! Live bytes are counted with a global allocator. The origin's and the
//! clients' threads are started before the measurement and marked, so only
//! the proxy's allocations (serve-net, http-net, net-io, and proxy-net) are
//! counted: the client connection's state, the upstream connection, and the
//! exchange. Thread stacks are not heap and are not counted; the workers are
//! started with the server either way.

#![cfg(feature = "serve")]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Barrier};
use std::time::Duration;

struct Live;

static LIVE: AtomicI64 = AtomicI64::new(0);

thread_local! {
    // Const-initialised with no destructor, so the allocator can read it.
    static NOT_PROXY: Cell<bool> = const { Cell::new(false) };
}

fn counted() -> bool {
    !NOT_PROXY.with(Cell::get)
}

// SAFETY: forwards to the system allocator unchanged; the counter is a
// relaxed atomic, which allocates nothing.
unsafe impl GlobalAlloc for Live {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if counted() {
            LIVE.fetch_add(layout.size() as i64, Ordering::Relaxed);
        }
        // SAFETY: the caller's layout, as given.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if counted() {
            LIVE.fetch_sub(layout.size() as i64, Ordering::Relaxed);
        }
        // SAFETY: `ptr` came from `alloc` above with this layout.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if counted() {
            LIVE.fetch_add(new_size as i64 - layout.size() as i64, Ordering::Relaxed);
        }
        // SAFETY: as for `dealloc`; `new_size` is the caller's.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: Live = Live;

const REPLY: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Length: 13\r\n\r\nhello, world\n";

/// `threads` origin threads sharing one listener; each holds a request's
/// response for `hold` after its head arrives.
fn slow_origin(threads: usize, hold: Duration) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    for _ in 0..threads {
        let listener = listener.try_clone().expect("clone");
        std::thread::spawn(move || {
            NOT_PROXY.with(|n| n.set(true));
            for stream in listener.incoming() {
                let Ok(mut s) = stream else { continue };
                let mut buf = [0u8; 4096];
                let mut filled = 0;
                loop {
                    let n = match s.read(&mut buf[filled..]) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => n,
                    };
                    filled += n;
                    if buf[..filled].windows(4).any(|w| w == b"\r\n\r\n") {
                        std::thread::sleep(hold);
                        if s.write_all(REPLY).is_err() {
                            break;
                        }
                        filled = 0;
                    }
                }
            }
        });
    }
    addr
}

/// The tests here measure one process-wide counter; one at a time.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Live heap bytes per exchange with `IN_FLIGHT` requests held by the
/// origin at once, under `inbound`.
fn heap_per_exchange(inbound: proxy_net::Inbound) -> i64 {
    const IN_FLIGHT: usize = 64;
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let up = slow_origin(IN_FLIGHT + 4, Duration::from_millis(1500));
    let proxy = proxy_net::ProxyBuilder::new()
        .pool("p", proxy_net::Pool::new([up.to_string()]))
        .build()
        .expect("build")
        .serve_with("127.0.0.1:0", inbound.max_per_source(128))
        .expect("serve");
    let addr = proxy.local_addr().expect("addr");

    // Clients, started now so their own allocations happen outside the
    // window: each waits, sends one request, and reads the answer.
    let go = Arc::new(Barrier::new(IN_FLIGHT + 1));
    let done = Arc::new(Barrier::new(IN_FLIGHT + 1));
    let mut clients = Vec::new();
    for _ in 0..IN_FLIGHT {
        let (go, done) = (Arc::clone(&go), Arc::clone(&done));
        clients.push(std::thread::spawn(move || {
            NOT_PROXY.with(|n| n.set(true));
            let _ = go.wait();
            let mut s = TcpStream::connect(addr).expect("connect");
            s.set_read_timeout(Some(Duration::from_secs(10)))
                .expect("timeout");
            s.write_all(b"GET /slow HTTP/1.1\r\nHost: h\r\n\r\n")
                .expect("write");
            let mut out = Vec::with_capacity(512);
            let mut b = [0u8; 512];
            while !out.ends_with(b"\r\n0\r\n\r\n") {
                match s.read(&mut b) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => out.extend_from_slice(&b[..n]),
                }
            }
            assert!(
                out.starts_with(b"HTTP/1.1 200"),
                "{}",
                String::from_utf8_lossy(&out)
            );
            let _ = done.wait();
        }));
    }
    // Let the proxy settle (worker threads started, checker running).
    std::thread::sleep(Duration::from_millis(300));
    let idle = LIVE.load(Ordering::SeqCst);
    let _ = go.wait();
    // Every request is now waiting for the origin: sample the live heap
    // while all of them are held.
    std::thread::sleep(Duration::from_millis(700));
    let active = LIVE.load(Ordering::SeqCst);
    let _ = done.wait();
    for c in clients {
        c.join().expect("client");
    }
    let per = (active - idle) / IN_FLIGHT as i64;
    println!(
        "heap per active exchange, read buffer {}: {per} bytes ({} in flight, {} bytes above idle)",
        inbound.read_buffer_value(),
        IN_FLIGHT,
        active - idle
    );
    assert!(per > 0, "nothing was measured");
    per
}

#[test]
fn test_heap_per_active_exchange_with_small_reads_is_under_20_kib() {
    // The 1.0 target, with 4 KiB client reads.
    let per = heap_per_exchange(proxy_net::Inbound::new().read_buffer(4096));
    assert!(per < 20 * 1024, "{per} bytes per active exchange");
}

#[test]
fn test_heap_per_active_exchange_by_default_is_bounded() {
    // The default reads 16 KiB at a time (measured at about 28 KiB in 0.7);
    // this bound catches growth.
    let per = heap_per_exchange(proxy_net::Inbound::new());
    assert!(per < 32 * 1024, "{per} bytes per active exchange");
}
