//! Load tests for `docs/PERFORMANCE.md`: an origin, a closed-loop load
//! generator, and proxy-net, all on the standard library only.
//!
//! ```text
//! cargo bench --bench compare                         # direct vs proxy-net, in one process
//! cargo bench --bench compare -- origin 127.0.0.1:9000 [--body N]
//! cargo bench --bench compare -- proxy 127.0.0.1:8080 127.0.0.1:9000
//! cargo bench --bench compare -- load 127.0.0.1:8080 [--connections N] [--seconds S] [--label NAME]
//! ```
//!
//! The load generator keeps `connections` keep-alive connections, each
//! sending one `GET` and reading the whole response before the next (closed
//! loop), and reports requests per second and latency percentiles. Other
//! proxies are measured the same way: start the origin, put the proxy in
//! front of it, and point `load` at the proxy.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

fn main() {
    let args: Vec<String> = std::env::args()
        .skip(1)
        .filter(|a| a != "--bench")
        .collect();
    let flag = |name: &str| -> Option<String> {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1).cloned())
    };
    let number = |name: &str, default: usize| -> usize {
        flag(name).and_then(|v| v.parse().ok()).unwrap_or(default)
    };
    match args.first().map(String::as_str) {
        Some("origin") => {
            let addr = args.get(1).map_or("127.0.0.1:9000", String::as_str);
            let listener = TcpListener::bind(addr).expect("bind the origin");
            eprintln!("origin on {}", listener.local_addr().expect("addr"));
            serve_origin(listener, number("--body", 13));
        }
        Some("proxy") => {
            let listen = args.get(1).map_or("127.0.0.1:8080", String::as_str);
            let origin = args.get(2).map_or("127.0.0.1:9000", String::as_str);
            let running = proxy_net::ProxyBuilder::new()
                .pool("origin", proxy_net::Pool::new([origin]))
                .build()
                .and_then(|p| p.serve_with(listen, inbound()))
                .expect("start proxy-net");
            eprintln!("proxy-net on {:?} to {origin}", running.local_addr());
            loop {
                std::thread::sleep(Duration::from_secs(3600));
            }
        }
        Some("load") => {
            let target: SocketAddr = args.get(1).and_then(|a| a.parse().ok()).expect("load ADDR");
            let label = flag("--label").unwrap_or_else(|| target.to_string());
            let r = load(
                target,
                number("--connections", 64),
                Duration::from_secs(number("--seconds", 10) as u64),
            );
            println!("{}", r.row(&label));
        }
        _ => in_process(number("--seconds", 5) as u64),
    }
}

/// Direct and through proxy-net, at three concurrency levels.
fn in_process(seconds: u64) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let origin = listener.local_addr().expect("addr");
    std::thread::spawn(move || serve_origin(listener, 13));
    let running = proxy_net::ProxyBuilder::new()
        .pool("origin", proxy_net::Pool::new([origin.to_string()]))
        .build()
        .and_then(|p| p.serve_with("127.0.0.1:0", inbound()))
        .expect("proxy");
    let proxy = running.local_addr().expect("proxy addr");
    println!("{}", Report::header());
    for connections in [1, 16, 64] {
        for (label, target) in [("direct", origin), ("proxy-net", proxy)] {
            let r = load(target, connections, Duration::from_secs(seconds));
            println!("{}", r.row(&format!("{label}, {connections} conn")));
        }
    }
}

/// Every load connection comes from one address, so the per-source cap is
/// raised to the connection cap (other proxies have none by default).
fn inbound() -> proxy_net::Inbound {
    proxy_net::Inbound::new().max_per_source(224)
}

const REQUEST: &[u8] = b"GET /bench HTTP/1.1\r\nHost: bench.test\r\n\r\n";

/// A keep-alive origin answering every request head with a fixed response.
fn serve_origin(listener: TcpListener, body: usize) {
    let mut reply = format!("HTTP/1.1 200 OK\r\nContent-Length: {body}\r\n\r\n").into_bytes();
    reply.extend((0..body).map(|i| b'a' + (i % 26) as u8));
    let reply: Arc<[u8]> = reply.into();
    for stream in listener.incoming() {
        let Ok(mut s) = stream else { continue };
        let reply = Arc::clone(&reply);
        std::thread::spawn(move || {
            let _ = s.set_nodelay(true);
            let mut buf = vec![0u8; 16 * 1024];
            let mut filled = 0;
            loop {
                let n = match s.read(&mut buf[filled..]) {
                    Ok(0) | Err(_) => return,
                    Ok(n) => n,
                };
                filled += n;
                while let Some(end) = head_end(&buf[..filled]) {
                    if s.write_all(&reply).is_err() {
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
}

fn head_end(b: &[u8]) -> Option<usize> {
    b.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4)
}

/// One response read off a keep-alive connection.
struct Reader {
    buf: Vec<u8>,
    filled: usize,
}

impl Reader {
    fn new() -> Reader {
        Reader {
            buf: vec![0u8; 64 * 1024],
            filled: 0,
        }
    }

    fn more(&mut self, s: &mut TcpStream) -> Option<()> {
        if self.filled == self.buf.len() {
            self.buf.resize(self.buf.len() * 2, 0);
        }
        match s.read(&mut self.buf[self.filled..]) {
            Ok(0) | Err(_) => None,
            Ok(n) => {
                self.filled += n;
                Some(())
            }
        }
    }

    fn consume(&mut self, n: usize) {
        self.buf.copy_within(n..self.filled, 0);
        self.filled -= n;
    }

    /// Reads one whole response; `Some(true)` when the connection stays open.
    fn response(&mut self, s: &mut TcpStream) -> Option<bool> {
        let end = loop {
            if let Some(e) = head_end(&self.buf[..self.filled]) {
                break e;
            }
            self.more(s)?;
        };
        let head = String::from_utf8_lossy(&self.buf[..end]).to_ascii_lowercase();
        if !head.starts_with("http/1.1 200") {
            return None;
        }
        let keep = !head.contains("\r\nconnection: close");
        let length = head
            .split("\r\n")
            .find_map(|l| l.strip_prefix("content-length:"))
            .and_then(|v| v.trim().parse::<usize>().ok());
        self.consume(end);
        match length {
            Some(n) => {
                while self.filled < n {
                    self.more(s)?;
                }
                self.consume(n);
            }
            None => loop {
                // Chunked: sizes, data, then the last chunk and an empty line.
                let line = loop {
                    if let Some(p) = self.buf[..self.filled]
                        .windows(2)
                        .position(|w| w == b"\r\n")
                    {
                        break p;
                    }
                    self.more(s)?;
                };
                let size = usize::from_str_radix(
                    String::from_utf8_lossy(&self.buf[..line])
                        .split(';')
                        .next()?
                        .trim(),
                    16,
                )
                .ok()?;
                let need = line + 2 + size + 2;
                while self.filled < need {
                    self.more(s)?;
                }
                self.consume(need);
                if size == 0 {
                    break;
                }
            },
        }
        Some(keep)
    }
}

struct Report {
    requests: u64,
    errors: u64,
    took: Duration,
    latencies: Vec<u32>,
}

impl Report {
    fn header() -> &'static str {
        "| target | requests/s | p50 | p99 | p99.9 | errors |\n|---|---:|---:|---:|---:|---:|"
    }

    fn percentile(&self, p: f64) -> String {
        if self.latencies.is_empty() {
            return "-".into();
        }
        let i = ((self.latencies.len() as f64 - 1.0) * p).round() as usize;
        let us = self.latencies[i];
        if us >= 1000 {
            format!("{:.2} ms", f64::from(us) / 1000.0)
        } else {
            format!("{us} us")
        }
    }

    fn row(&self, label: &str) -> String {
        format!(
            "| {label} | {:.0} | {} | {} | {} | {} |",
            self.requests as f64 / self.took.as_secs_f64(),
            self.percentile(0.5),
            self.percentile(0.99),
            self.percentile(0.999),
            self.errors
        )
    }
}

/// `connections` closed-loop clients against `target` for `run`, after a
/// one-second warm-up that is not counted.
fn load(target: SocketAddr, connections: usize, run: Duration) -> Report {
    let measuring = Arc::new(AtomicBool::new(false));
    let stop = Arc::new(AtomicBool::new(false));
    let workers: Vec<_> = (0..connections)
        .map(|_| {
            let (measuring, stop) = (Arc::clone(&measuring), Arc::clone(&stop));
            std::thread::spawn(move || {
                let mut latencies = Vec::with_capacity(1 << 16);
                let (mut requests, mut errors) = (0u64, 0u64);
                let mut conn: Option<(TcpStream, Reader)> = None;
                while !stop.load(Ordering::Relaxed) {
                    let (s, r) = match &mut conn {
                        Some(c) => c,
                        None => match TcpStream::connect(target) {
                            Ok(s) => {
                                let _ = s.set_nodelay(true);
                                let _ = s.set_read_timeout(Some(Duration::from_secs(10)));
                                conn.insert((s, Reader::new()))
                            }
                            Err(_) => {
                                errors += 1;
                                std::thread::sleep(Duration::from_millis(10));
                                continue;
                            }
                        },
                    };
                    let start = Instant::now();
                    let outcome = s.write_all(REQUEST).ok().and_then(|()| r.response(s));
                    let took = start.elapsed();
                    let counted = measuring.load(Ordering::Relaxed);
                    match outcome {
                        Some(keep) => {
                            if counted {
                                requests += 1;
                                latencies.push(u32::try_from(took.as_micros()).unwrap_or(u32::MAX));
                            }
                            if !keep {
                                conn = None;
                            }
                        }
                        None => {
                            if counted {
                                errors += 1;
                            }
                            conn = None;
                        }
                    }
                }
                (requests, errors, latencies)
            })
        })
        .collect();
    std::thread::sleep(Duration::from_secs(1));
    measuring.store(true, Ordering::Relaxed);
    let started = Instant::now();
    std::thread::sleep(run);
    measuring.store(false, Ordering::Relaxed);
    let took = started.elapsed();
    stop.store(true, Ordering::Relaxed);
    let mut report = Report {
        requests: 0,
        errors: 0,
        took,
        latencies: Vec::new(),
    };
    for w in workers {
        if let Ok((r, e, l)) = w.join() {
            report.requests += r;
            report.errors += e;
            report.latencies.extend(l);
        }
    }
    report.latencies.sort_unstable();
    report
}
