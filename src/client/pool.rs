//! Upstream connections: one idle list per endpoint, connection caps, and
//! the reuse rules.

use std::boxed::Box;
use std::sync::{Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};
use std::vec::Vec;

use http_net::ClientConn;

use super::connector::UpstreamIo;
use crate::core::ErrorClass;
use crate::pool::PoolLimits;

/// One connection to an endpoint, with its HTTP/1.1 client state.
pub(crate) struct UpstreamConn {
    pub io: Box<dyn UpstreamIo>,
    pub http: ClientConn,
    pub exchanges: u32,
    pub reused: bool,
}

struct Idle {
    conn: UpstreamConn,
    since: Instant,
}

#[derive(Default)]
struct Slots {
    idle: Vec<Idle>,
    open: u32,
    pending: u32,
}

/// The connections of one endpoint.
pub(crate) struct EndpointConns {
    slots: Mutex<Slots>,
    freed: Condvar,
    max_open: u32,
}

/// What `take` found.
#[allow(
    clippy::large_enum_variant,
    reason = "moved once per exchange; boxing it allocated on every reuse"
)]
pub(crate) enum Taken {
    /// A pooled connection that passed every reuse check.
    Pooled(UpstreamConn),
    /// A place for a new connection: dial it, then `opened` or `dial_failed`.
    Dial,
}

impl EndpointConns {
    pub(crate) fn new(max_open: u32) -> EndpointConns {
        EndpointConns {
            slots: Mutex::new(Slots::default()),
            freed: Condvar::new(),
            max_open,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Slots> {
        self.slots.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A pooled connection, or a place to dial one. A pooled connection is
    /// reused only when it has been idle less than `idle_timeout` (and less
    /// than `fresh_for` for a non-idempotent request), has carried fewer
    /// than the most exchanges per connection, and its probe says it is open
    /// with nothing waiting. When every place is taken, waits up to
    /// `pending_timeout` for one (at most `max_pending` waiters).
    ///
    /// The lock covers only the list and the counts: the probe and any close
    /// are system calls, made after it is released, so threads taking from
    /// one endpoint do not queue behind each other's system calls.
    pub(crate) fn take(
        &self,
        limits: &PoolLimits,
        idempotent: bool,
        deadline: Instant,
    ) -> Result<Taken, ErrorClass> {
        let wait_until = (Instant::now() + limits.pending_timeout_value()).min(deadline);
        let mut slots = self.lock();
        loop {
            let now = Instant::now();
            // LIFO: the most recently used connection first.
            if let Some(idle) = slots.idle.pop() {
                // Still counted in `open` while it is checked outside the lock.
                drop(slots);
                let age = now.saturating_duration_since(idle.since);
                let fresh = idempotent || age < limits.fresh_for_value();
                let usable = age < limits.idle_timeout_value()
                    && idle.conn.exchanges < limits.max_exchanges_per_connection_value()
                    && fresh;
                let mut conn = idle.conn;
                if usable && conn.io.is_reusable() {
                    conn.reused = true;
                    return Ok(Taken::Pooled(conn));
                }
                conn.io.close();
                slots = self.lock();
                slots.open = slots.open.saturating_sub(1);
                continue;
            }
            if slots.open < self.max_open {
                slots.open += 1;
                return Ok(Taken::Dial);
            }
            if slots.pending >= limits.max_pending_value() || now >= wait_until {
                return Err(ErrorClass::Breaker);
            }
            slots.pending += 1;
            let (guard, _) = self
                .freed
                .wait_timeout(slots, wait_until.saturating_duration_since(now))
                .unwrap_or_else(PoisonError::into_inner);
            slots = guard;
            slots.pending -= 1;
        }
    }

    /// Wakes one waiter when there is one. `slots` is released first.
    fn freed_one(&self, slots: std::sync::MutexGuard<'_, Slots>) {
        let waiting = slots.pending > 0;
        drop(slots);
        if waiting {
            self.freed.notify_one();
        }
    }

    /// A dial for a place from `take` failed: the place is free again.
    pub(crate) fn dial_failed(&self) {
        let mut slots = self.lock();
        slots.open = slots.open.saturating_sub(1);
        self.freed_one(slots);
    }

    /// The exchange is over: the connection goes back to the pool when
    /// `reuse` holds and the pool has room, and is closed otherwise. A close
    /// happens before its place is freed, so the endpoint never has more
    /// sockets open than its cap.
    pub(crate) fn release(&self, mut conn: UpstreamConn, reuse: bool, max_idle: u32) {
        if reuse {
            let _ = conn.http.shrink_idle();
            let mut slots = self.lock();
            if (slots.idle.len() as u64) < u64::from(max_idle) {
                slots.idle.push(Idle {
                    conn,
                    since: Instant::now(),
                });
                self.freed_one(slots);
                return;
            }
        }
        conn.io.close();
        let mut slots = self.lock();
        slots.open = slots.open.saturating_sub(1);
        self.freed_one(slots);
    }

    /// Closes idle connections older than `idle_timeout` (the health checker
    /// runs it on its schedule, so nothing polls for it). The closes happen
    /// after the lock is released.
    pub(crate) fn sweep(&self, idle_timeout: Duration) {
        let now = Instant::now();
        let mut slots = self.lock();
        if slots
            .idle
            .iter()
            .all(|i| now.saturating_duration_since(i.since) < idle_timeout)
        {
            return;
        }
        let mut stale = Vec::new();
        let mut kept = Vec::with_capacity(slots.idle.len());
        for idle in slots.idle.drain(..) {
            if now.saturating_duration_since(idle.since) < idle_timeout {
                kept.push(idle);
            } else {
                stale.push(idle);
            }
        }
        slots.idle = kept;
        drop(slots);
        let closed = stale.len();
        for mut idle in stale {
            idle.conn.io.close();
        }
        let mut slots = self.lock();
        slots.open = slots.open.saturating_sub(closed as u32);
        drop(slots);
        self.freed.notify_all();
    }

    /// Open connections (in use and idle) and idle ones.
    pub(crate) fn counts(&self) -> (u32, u32) {
        let slots = self.lock();
        (slots.open, slots.idle.len() as u32)
    }

    /// Closes every idle connection (a drain, or the proxy stopping).
    pub(crate) fn close_idle(&self) {
        self.sweep(Duration::ZERO);
    }
}
