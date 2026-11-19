//! The active health checker: one thread per proxy, sleeping until the next
//! check is due.

use std::boxed::Box;
use std::string::ToString;
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use std::vec::Vec;

use http_net::{ClientConn, ClientEvent, Request};

use super::runtime::{Runtime, Shared};
use crate::core::{EndpointId, TimeoutKind, Transition};
use crate::pool::CheckKind;

/// Stops the checker thread.
#[derive(Debug, Default)]
pub(crate) struct Stop {
    stopped: Mutex<bool>,
    wake: Condvar,
}

impl Stop {
    pub(crate) fn stop(&self) {
        *self.stopped.lock().unwrap_or_else(PoisonError::into_inner) = true;
        self.wake.notify_all();
    }

    /// Sleeps until `until` or a stop; `true` when stopped.
    fn sleep_until(&self, until: Instant) -> bool {
        let mut stopped = self.stopped.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            if *stopped {
                return true;
            }
            let now = Instant::now();
            if now >= until {
                return false;
            }
            stopped = self
                .wake
                .wait_timeout(stopped, until - now)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }
}

/// How often expired ejections and idle connections are looked at when no
/// active check is due sooner.
const HOUSEKEEPING: Duration = Duration::from_millis(500);

/// Starts the checker thread.
pub(crate) fn spawn(shared: Arc<Shared>, stop: Arc<Stop>) -> std::io::Result<JoinHandle<()>> {
    std::thread::Builder::new()
        .name("proxy-net-health".into())
        .spawn(move || run(&shared, &stop))
}

/// The next due time of every endpoint with an active check, spread over one
/// interval so the first checks do not all start together.
fn schedule(rt: &Runtime) -> Vec<(usize, EndpointId, Instant)> {
    let mut due = Vec::new();
    let start = Instant::now();
    for (p, pool) in rt.config.pools().iter().enumerate() {
        if let Some(check) = pool.health() {
            let n = pool.endpoints().len() as u32;
            for i in 0..pool.endpoints().len() {
                let spread = check.interval_value() * (i as u32) / n.max(1);
                due.push((p, EndpointId(i as u16), start + spread));
            }
        }
    }
    due
}

fn run(shared: &Shared, stop: &Stop) {
    let mut rt = shared.runtime();
    let mut due = schedule(&rt);
    // The next refresh of each pool with a refreshing discovery source.
    let mut refresh: Vec<(Box<str>, Instant)> = Vec::new();
    let mut seq: u64 = 0;
    let mut changes: Vec<Transition> = Vec::new();
    loop {
        let current = shared.runtime();
        if !Arc::ptr_eq(&current, &rt) {
            rt = current;
            due = schedule(&rt);
        }
        let now = Instant::now();
        // Ejections whose time has come, slow-start ramps, and idle
        // connections past their time.
        for (p, pool) in rt.pools.iter().enumerate() {
            changes.clear();
            pool.health.expire(rt.tick_at(now), &mut changes);
            for t in changes.drain(..) {
                rt.changed(p, t);
            }
            if let Some(cfg) = rt.config.pool_at(p) {
                if !cfg.slow_start().is_zero() {
                    let _ = pool
                        .balancer
                        .update_ramps(rt.tick_at(now), cfg.slow_start());
                }
                for conns in pool.conns.iter() {
                    conns.sweep(cfg.limits().idle_timeout_value());
                }
            }
        }
        for slot in &mut due {
            if slot.2 > now {
                continue;
            }
            let (p, id) = (slot.0, slot.1);
            let Some(check) = rt.config.pool_at(p).and_then(|c| c.health()) else {
                continue;
            };
            let ok = probe(&rt, p, id);
            if let Some(pool) = rt.pools.get(p) {
                if let Some(t) = pool.health.check_result(id, ok, rt.tick()) {
                    rt.changed(p, t);
                }
            }
            seq = seq.wrapping_add(1);
            slot.2 = Instant::now() + jittered(check.interval_value(), check.jitter_value(), seq);
        }
        refresh_discovery(shared, &mut refresh);
        let next = due
            .iter()
            .map(|d| d.2)
            .chain(refresh.iter().map(|r| r.1))
            .min()
            .unwrap_or(now + HOUSEKEEPING)
            .min(Instant::now() + HOUSEKEEPING);
        if stop.sleep_until(next) {
            return;
        }
    }
}

/// Asks the refreshing discovery sources whose time has come, and reloads a
/// pool whose endpoints changed.
fn refresh_discovery(shared: &Shared, refresh: &mut Vec<(Box<str>, Instant)>) {
    if !shared.discovers() {
        return;
    }
    let builder = shared.builder();
    let now = Instant::now();
    let mut changed: Vec<(Box<str>, Vec<crate::Endpoint>)> = Vec::new();
    let rt = shared.runtime();
    let _ = builder.clone().map_pools(|name, pool| {
        let Some(slot) = pool.discovery_slot().cloned() else {
            return pool;
        };
        let Some(every) = slot.0.refresh_every() else {
            return pool;
        };
        let next = match refresh.iter_mut().find(|r| &*r.0 == name) {
            Some(r) => r,
            None => {
                refresh.push((name.into(), now + every));
                return pool;
            }
        };
        if next.1 > now {
            return pool;
        }
        next.1 = now + every;
        match slot.0.endpoints() {
            Ok(found) => {
                let kept = super::discovery::allowed(found, pool.allowlist());
                if kept.is_empty() {
                    shared
                        .observer
                        .discovery_failed(name, "no endpoint inside the allowlist");
                } else {
                    let same = rt.config.pool(name).is_some_and(|p| {
                        p.endpoints().len() == kept.len()
                            && p.endpoints()
                                .iter()
                                .zip(&kept)
                                .all(|(a, b)| a.addr() == b.addr())
                    });
                    if !same {
                        changed.push((name.into(), kept));
                    }
                }
            }
            Err(e) => shared.observer.discovery_failed(name, e.message()),
        }
        pool
    });
    for (name, endpoints) in changed {
        let next = shared.builder().map_pools(|n, p| {
            if n == &*name {
                p.with_endpoints(&endpoints)
            } else {
                p
            }
        });
        if let Err(e) = shared.swap(next) {
            shared.observer.discovery_failed(&name, &e.to_string());
        }
    }
}

/// `interval` plus or minus up to `percent` of it.
fn jittered(interval: Duration, percent: u8, seq: u64) -> Duration {
    if percent == 0 {
        return interval;
    }
    let span = interval.as_nanos() as u64 * u64::from(percent) / 100;
    if span == 0 {
        return interval;
    }
    let r = crate::core::jitter(seq) % (2 * span + 1);
    let ns = (interval.as_nanos() as u64)
        .saturating_add(r)
        .saturating_sub(span);
    Duration::from_nanos(ns)
}

/// One check of endpoint `id`: a connect, and for HTTP checks a request
/// whose status is in the expected range (and whose body, when asked, holds
/// the expected text in its first 4 KiB).
fn probe(rt: &Runtime, p: usize, id: EndpointId) -> bool {
    let Some(pool) = rt.config.pool_at(p) else {
        return false;
    };
    let (Some(check), Some(endpoint)) = (pool.health(), pool.endpoints().get(id.index())) else {
        return false;
    };
    let deadline = Instant::now() + check.timeout_value();
    let Some(connector) = rt.pools.get(p).map(|r| Arc::clone(&r.connector)) else {
        return false;
    };
    let prelude = super::forward::prelude(pool.proxy_protocol(), None, None, b"");
    let Ok(mut io) = connector.connect(endpoint.addr(), &prelude, deadline) else {
        return false;
    };
    let ok = match check.kind() {
        CheckKind::Http {
            path,
            host,
            expect,
            body,
        } => {
            let host: String = match host {
                Some(h) => h.to_string(),
                None => endpoint
                    .server_name_value()
                    .map(str::to_string)
                    .unwrap_or_else(|| endpoint.addr().to_string()),
            };
            let request = Request::new("GET", path).header("host", host).close();
            let mut http = ClientConn::with_limits(http_net::Limits::default().with_max_body(4096));
            let mut out = Vec::new();
            if http.send_request(&request, &mut out).is_err() || io.send(&out, deadline).is_err() {
                false
            } else {
                read_check(&mut http, io.as_mut(), deadline, expect, body.as_deref())
            }
        }
        _ => true,
    };
    io.close();
    ok
}

fn read_check(
    http: &mut ClientConn,
    io: &mut dyn super::connector::UpstreamIo,
    deadline: Instant,
    expect: &core::ops::RangeInclusive<u16>,
    body: Option<&str>,
) -> bool {
    let mut status_ok = false;
    let mut seen: Vec<u8> = Vec::new();
    loop {
        match http.poll() {
            Ok(ClientEvent::Response(head)) => {
                status_ok = expect.contains(&head.status());
                if !status_ok {
                    return false;
                }
                if body.is_none() {
                    return true;
                }
            }
            Ok(ClientEvent::Body(b)) => {
                let room = 4096usize.saturating_sub(seen.len());
                seen.extend_from_slice(&b[..b.len().min(room)]);
            }
            Ok(ClientEvent::ResponseEnd) => break,
            Ok(ClientEvent::NeedInput) => {
                let buf = http.read_buf();
                match io.recv(buf, deadline, TimeoutKind::ResponseHead) {
                    Ok(0) => http.feed_eof(),
                    Ok(n) => http.commit(n),
                    Err(_) => return false,
                }
            }
            Ok(ClientEvent::Closed) | Err(_) => break,
            Ok(_) => {}
        }
        if seen.len() >= 4096 {
            break;
        }
    }
    status_ok
        && body.is_none_or(|text| {
            seen.windows(text.len().max(1))
                .any(|w| w == text.as_bytes())
        })
}
