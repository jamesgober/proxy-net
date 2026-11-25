//! A long soak of proxy-net under mixed traffic and faults.
//!
//! ```text
//! cargo bench --bench soak -- [--minutes M] [--connections N] [--pace-us P]
//! ```
//!
//! In one process: three origins (one plain, one answering chunked bodies
//! of random size, one that closes connections often and goes down for 10 s
//! every 5 minutes), proxy-net in front of them with active health checks,
//! and closed-loop clients sending a mix of small and large `GET`s, `POST`s
//! with bodies, requests the path policy refuses, and requests whose client
//! leaves half way (0.1%). Every 2 minutes a pool's endpoints are replaced, and
//! every 10 minutes the whole configuration is reloaded.
//!
//! One line per minute: requests and their outcomes, latency, the live heap,
//! the resident set (Linux), and the proxy's counters. At the end, after the
//! load stops: every exchange accounted for, no upstream connection in use,
//! and the live heap's growth over the second half of the run. The exit
//! status is non-zero when a check fails.

use std::alloc::{GlobalAlloc, Layout, System};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use proxy_net::{Endpoint, HealthCheck, Inbound, Pool, ProxyBuilder, Route};

struct Live;
static LIVE: AtomicI64 = AtomicI64::new(0);

// SAFETY: forwards to the system allocator unchanged; the counter is a
// relaxed atomic, which allocates nothing.
unsafe impl GlobalAlloc for Live {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        LIVE.fetch_add(layout.size() as i64, Ordering::Relaxed);
        // SAFETY: the caller's layout.
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE.fetch_sub(layout.size() as i64, Ordering::Relaxed);
        // SAFETY: `ptr` came from `alloc` with this layout.
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        LIVE.fetch_add(new_size as i64 - layout.size() as i64, Ordering::Relaxed);
        // SAFETY: as for `dealloc`.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: Live = Live;

/// A small deterministic generator per thread.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// 13 bytes with a length.
    Plain,
    /// Chunked bodies of 0 to 64 KiB.
    Chunked,
    /// Plain, but closes after a few requests, and down 10 s in every 5 min.
    Flaky,
}

fn find_head(b: &[u8]) -> Option<usize> {
    b.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4)
}

fn origin(kind: Kind, seed: u64) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let start = Instant::now();
    std::thread::spawn(move || {
        let mut n = 0u64;
        for stream in listener.incoming() {
            let Ok(s) = stream else { continue };
            n += 1;
            std::thread::spawn(move || serve(s, kind, seed ^ n, start));
        }
    });
    addr
}

fn down(kind: Kind, start: Instant) -> bool {
    kind == Kind::Flaky && start.elapsed().as_secs() % 300 >= 290
}

fn serve(mut s: TcpStream, kind: Kind, seed: u64, start: Instant) {
    let mut rng = Rng(seed | 1);
    let _ = s.set_nodelay(true);
    let _ = s.set_read_timeout(Some(Duration::from_secs(120)));
    let mut buf = vec![0u8; 64 * 1024];
    let mut filled = 0;
    let mut served = 0u64;
    loop {
        if down(kind, start) {
            return;
        }
        let n = match s.read(&mut buf[filled..]) {
            Ok(0) | Err(_) => return,
            Ok(n) => n,
        };
        filled += n;
        while let Some(end) = find_head(&buf[..filled]) {
            let head = String::from_utf8_lossy(&buf[..end]).to_ascii_lowercase();
            let length: usize = head
                .split("\r\n")
                .find_map(|l| l.strip_prefix("content-length:"))
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(0);
            let chunked = head.contains("transfer-encoding: chunked");
            let total = if chunked {
                match buf[end..filled].windows(5).position(|w| w == b"0\r\n\r\n") {
                    Some(p) => end + p + 5,
                    None => break,
                }
            } else {
                end + length
            };
            if total > filled {
                if total > buf.len() {
                    buf.resize(total, 0);
                }
                break;
            }
            let reply: Vec<u8> = match kind {
                Kind::Chunked if head.starts_with("get /big") => {
                    let size = rng.below(64 * 1024) as usize;
                    let mut r = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec();
                    let mut left = size;
                    while left > 0 {
                        let part = left.min(8 * 1024);
                        r.extend_from_slice(format!("{part:x}\r\n").as_bytes());
                        r.resize(r.len() + part, b'b');
                        r.extend_from_slice(b"\r\n");
                        left -= part;
                    }
                    r.extend_from_slice(b"0\r\n\r\n");
                    r
                }
                _ => b"HTTP/1.1 200 OK\r\nContent-Length: 13\r\n\r\nhello, world\n".to_vec(),
            };
            if s.write_all(&reply).is_err() {
                return;
            }
            served += 1;
            buf.copy_within(total..filled, 0);
            filled -= total;
            if kind == Kind::Flaky && served >= 50 + rng.below(200) {
                return;
            }
        }
    }
}

#[derive(Default)]
struct Tally {
    ok: AtomicU64,
    refused_as_expected: AtomicU64,
    upstream_errors: AtomicU64,
    unexpected: AtomicU64,
    aborted: AtomicU64,
    io_errors: AtomicU64,
    /// I/O errors by where they happened: connect, write, before the head,
    /// in the body, a head that did not parse.
    io_by_stage: [AtomicU64; 5],
}

const STAGES: [&str; 5] = [
    "connect",
    "write",
    "before the head",
    "in the body",
    "bad head",
];

/// Reads one response; the status, and whether the connection stays open,
/// or the stage it failed at (an index into `STAGES`).
fn response(s: &mut TcpStream, buf: &mut Vec<u8>) -> Result<(u16, bool), usize> {
    let mut filled = 0;
    let mut chunk = [0u8; 16 * 1024];
    let end = loop {
        if let Some(e) = find_head(&buf[..filled]) {
            break e;
        }
        let n = s.read(&mut chunk).ok().filter(|&n| n > 0).ok_or(2usize)?;
        buf.resize(filled + n, 0);
        buf[filled..filled + n].copy_from_slice(&chunk[..n]);
        filled += n;
    };
    let head = String::from_utf8_lossy(&buf[..end]).to_ascii_lowercase();
    let status: u16 = head.get(9..12).and_then(|v| v.parse().ok()).ok_or(4usize)?;
    let keep = !head.contains("\r\nconnection: close");
    let length = head
        .split("\r\n")
        .find_map(|l| l.strip_prefix("content-length:"))
        .and_then(|v| v.trim().parse::<usize>().ok());
    let mut body: Vec<u8> = buf[end..filled].to_vec();
    match length {
        Some(n) => {
            while body.len() < n {
                let k = s.read(&mut chunk).ok().filter(|&k| k > 0).ok_or(3usize)?;
                body.extend_from_slice(&chunk[..k]);
            }
        }
        None if head.contains("transfer-encoding: chunked") => {
            while !body.ends_with(b"\r\n0\r\n\r\n") && body != b"0\r\n\r\n" {
                let k = s.read(&mut chunk).ok().filter(|&k| k > 0).ok_or(3usize)?;
                body.extend_from_slice(&chunk[..k]);
            }
        }
        None => {}
    }
    buf.clear();
    Ok((status, keep))
}

fn client(
    addr: SocketAddr,
    seed: u64,
    stop: Arc<AtomicBool>,
    tally: Arc<Tally>,
    lat: Arc<Mutex<Vec<u32>>>,
    pace: Duration,
) {
    let mut rng = Rng(seed | 1);
    let mut conn: Option<TcpStream> = None;
    let mut buf = Vec::with_capacity(64 * 1024);
    let body = vec![b'p'; 16 * 1024];
    let mut mine = Vec::with_capacity(4096);
    while !stop.load(Ordering::Relaxed) {
        let s = match &mut conn {
            Some(s) => s,
            None => match TcpStream::connect(addr) {
                Ok(s) => {
                    let _ = s.set_nodelay(true);
                    let _ = s.set_read_timeout(Some(Duration::from_secs(30)));
                    conn.insert(s)
                }
                Err(e) => {
                    if tally.io_by_stage[0].load(Ordering::Relaxed) < 3 {
                        eprintln!("connect to the proxy failed: {e}");
                    }
                    tally.io_errors.fetch_add(1, Ordering::Relaxed);
                    tally.io_by_stage[0].fetch_add(1, Ordering::Relaxed);
                    std::thread::sleep(Duration::from_millis(50));
                    continue;
                }
            },
        };
        let pick = rng.below(1000);
        let (request, expect_refusal, abort): (Vec<u8>, bool, bool) = if pick < 700 {
            (
                b"GET /small HTTP/1.1\r\nHost: soak\r\n\r\n".to_vec(),
                false,
                false,
            )
        } else if pick < 850 {
            (
                b"GET /big HTTP/1.1\r\nHost: soak\r\n\r\n".to_vec(),
                false,
                false,
            )
        } else if pick < 994 {
            let len = rng.below(16 * 1024) as usize;
            let mut r =
                format!("POST /post HTTP/1.1\r\nHost: soak\r\nContent-Length: {len}\r\n\r\n")
                    .into_bytes();
            r.extend_from_slice(&body[..len]);
            (r, false, false)
        } else if pick < 999 {
            (
                b"GET /small/../etc HTTP/1.1\r\nHost: soak\r\n\r\n".to_vec(),
                true,
                false,
            )
        } else {
            (
                b"GET /big HTTP/1.1\r\nHost: soak\r\n\r\n".to_vec(),
                false,
                true,
            )
        };
        let started = Instant::now();
        if s.write_all(&request).is_err() {
            tally.io_errors.fetch_add(1, Ordering::Relaxed);
            tally.io_by_stage[1].fetch_add(1, Ordering::Relaxed);
            conn = None;
            continue;
        }
        if abort {
            let mut part = [0u8; 64];
            let _ = s.read(&mut part);
            tally.aborted.fetch_add(1, Ordering::Relaxed);
            conn = None;
            continue;
        }
        match response(s, &mut buf) {
            Ok((status, keep)) => {
                let t = &tally;
                match status {
                    200 if !expect_refusal => t.ok.fetch_add(1, Ordering::Relaxed),
                    400 if expect_refusal => t.refused_as_expected.fetch_add(1, Ordering::Relaxed),
                    502..=504 => t.upstream_errors.fetch_add(1, Ordering::Relaxed),
                    _ => t.unexpected.fetch_add(1, Ordering::Relaxed),
                };
                mine.push(u32::try_from(started.elapsed().as_micros()).unwrap_or(u32::MAX));
                if !keep {
                    conn = None;
                }
            }
            Err(stage) => {
                tally.io_errors.fetch_add(1, Ordering::Relaxed);
                tally.io_by_stage[stage].fetch_add(1, Ordering::Relaxed);
                conn = None;
            }
        }
        if mine.len() >= 256 {
            if let Ok(mut l) = lat.lock() {
                l.extend_from_slice(&mine);
            }
            mine.clear();
        }
        if !pace.is_zero() {
            std::thread::sleep(pace);
        }
    }
}

fn rss_kib() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    status
        .lines()
        .find_map(|l| l.strip_prefix("VmRSS:"))
        .and_then(|v| v.trim().trim_end_matches("kB").trim().parse().ok())
}

fn main() {
    let args: Vec<String> = std::env::args()
        .skip(1)
        .filter(|a| a != "--bench")
        .collect();
    let flag = |name: &str, default: u64| -> u64 {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .and_then(|v| v.parse().ok())
            .unwrap_or(default)
    };
    let minutes = flag("--minutes", 10);
    let connections = flag("--connections", 16) as usize;
    let pace = Duration::from_micros(flag("--pace-us", 2_000));

    let plain = origin(Kind::Plain, 1);
    let chunked = origin(Kind::Chunked, 2);
    let flaky = origin(Kind::Flaky, 3);
    let check = HealthCheck::http("/health").interval(Duration::from_secs(5));
    let builder = |with_flaky: bool| {
        let mut app = vec![plain.to_string(), chunked.to_string()];
        if with_flaky {
            app.push(flaky.to_string());
        }
        ProxyBuilder::new()
            .pool("app", Pool::new(app).health(check.clone()))
            .pool("big", Pool::new([chunked.to_string()]))
            .route(Route::any().path("/big").to("big"))
            .route(Route::any().to("app"))
    };
    let proxy = builder(true).build().expect("build");
    let handle = proxy.handle();
    let running = proxy
        .serve_with("127.0.0.1:0", Inbound::new().max_per_source(224))
        .expect("serve");
    let addr = running.local_addr().expect("addr");

    let stop = Arc::new(AtomicBool::new(false));
    let tally = Arc::new(Tally::default());
    let lat = Arc::new(Mutex::new(Vec::new()));
    let clients: Vec<_> = (0..connections)
        .map(|i| {
            let (stop, tally, lat) = (Arc::clone(&stop), Arc::clone(&tally), Arc::clone(&lat));
            std::thread::spawn(move || {
                client(
                    addr,
                    0x9e37_79b9 ^ ((i as u64 + 1) * 7919),
                    stop,
                    tally,
                    lat,
                    pace,
                )
            })
        })
        .collect();

    let started = Instant::now();
    let mut heap: Vec<(u64, i64)> = Vec::new();
    let mut last_ok = 0;
    println!(
        "minute,requests_ok,refused_400,upstream_5xx,unexpected,aborted,io_errors,p50_us,p99_us,live_heap_kib,rss_kib,exchanges,succeeded,failed,refused,upstream_open"
    );
    for minute in 1..=minutes {
        let until = started + Duration::from_secs(minute * 60);
        while Instant::now() < until {
            std::thread::sleep(Duration::from_millis(500));
            let secs = started.elapsed().as_secs();
            if secs % 120 == 60 && secs % 600 != 60 {
                // Replace the app pool's endpoints: without the flaky one, then with.
                let mut eps = vec![
                    Endpoint::parse(&plain.to_string()).expect("ep"),
                    Endpoint::parse(&chunked.to_string()).expect("ep"),
                ];
                if (secs / 120) % 2 == 0 {
                    eps.push(Endpoint::parse(&flaky.to_string()).expect("ep"));
                }
                let _ = handle.set_endpoints("app", &eps);
                std::thread::sleep(Duration::from_millis(600));
            } else if secs % 600 == 60 && secs > 60 {
                let _ = handle.reload(builder(true));
                std::thread::sleep(Duration::from_millis(600));
            }
        }
        let mut l = lat
            .lock()
            .map(|mut l| std::mem::take(&mut *l))
            .unwrap_or_default();
        l.sort_unstable();
        let at = |p: f64| {
            l.get(((l.len() as f64 - 1.0) * p) as usize)
                .copied()
                .unwrap_or(0)
        };
        let live = LIVE.load(Ordering::Relaxed) / 1024;
        heap.push((minute, live));
        let st = handle.stats();
        let open: u32 = handle
            .health()
            .pools
            .iter()
            .flat_map(|p| p.endpoints.iter())
            .map(|e| e.open)
            .sum();
        let ok = tally.ok.load(Ordering::Relaxed);
        println!(
            "{minute},{},{},{},{},{},{},{},{},{live},{},{},{},{},{},{open}",
            ok - last_ok,
            tally.refused_as_expected.load(Ordering::Relaxed),
            tally.upstream_errors.load(Ordering::Relaxed),
            tally.unexpected.load(Ordering::Relaxed),
            tally.aborted.load(Ordering::Relaxed),
            tally.io_errors.load(Ordering::Relaxed),
            at(0.5),
            at(0.99),
            rss_kib().map_or_else(|| "-".to_string(), |r| r.to_string()),
            st.exchanges,
            st.succeeded,
            st.failed,
            st.refused,
        );
        last_ok = ok;
    }
    stop.store(true, Ordering::Relaxed);
    for c in clients {
        let _ = c.join();
    }
    std::thread::sleep(Duration::from_secs(5));
    let st = handle.stats();
    let active: u32 = handle
        .health()
        .pools
        .iter()
        .flat_map(|p| p.endpoints.iter())
        .map(|e| e.active)
        .sum();
    let mut failed = Vec::new();
    if st.exchanges != st.succeeded + st.failed {
        failed.push(format!(
            "{} exchanges, {} succeeded, {} failed: some never ended",
            st.exchanges, st.succeeded, st.failed
        ));
    }
    if active != 0 {
        failed.push(format!(
            "{active} upstream exchanges still active after the load stopped"
        ));
    }
    if tally.unexpected.load(Ordering::Relaxed) != 0 {
        failed.push(format!(
            "{} unexpected statuses",
            tally.unexpected.load(Ordering::Relaxed)
        ));
    }
    // Growth of the live heap over the second half, from the minimum of
    // the third quarter to the maximum of the last.
    let n = heap.len();
    if n >= 8 {
        let third = heap[n / 2..3 * n / 4]
            .iter()
            .map(|h| h.1)
            .min()
            .unwrap_or(0);
        let last = heap[3 * n / 4..].iter().map(|h| h.1).max().unwrap_or(0);
        let growth = last - third;
        println!(
            "live heap: third quarter low {third} KiB, last quarter high {last} KiB, growth {growth} KiB"
        );
        if third > 0 && growth * 10 > third {
            failed.push(format!(
                "live heap grew {growth} KiB (over 10%) in the second half"
            ));
        }
    }
    println!(
        "summary: {} minutes, {} ok, {} refused as expected, {} upstream errors, {} aborted, {} io errors, {} exchanges",
        minutes,
        tally.ok.load(Ordering::Relaxed),
        tally.refused_as_expected.load(Ordering::Relaxed),
        tally.upstream_errors.load(Ordering::Relaxed),
        tally.aborted.load(Ordering::Relaxed),
        tally.io_errors.load(Ordering::Relaxed),
        st.exchanges
    );
    let stages: Vec<String> = STAGES
        .iter()
        .zip(&tally.io_by_stage)
        .map(|(name, n)| format!("{name} {}", n.load(Ordering::Relaxed)))
        .collect();
    println!("io errors by stage: {}", stages.join(", "));
    if failed.is_empty() {
        println!("SOAK PASSED");
    } else {
        for f in &failed {
            println!("SOAK FAILED: {f}");
        }
        drop(running);
        std::process::exit(1);
    }
    drop(running);
}
