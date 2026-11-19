//! The forwarding state machine of one request.
//!
//! `Exchange` owns no socket and reads no clock. The caller feeds it what
//! happened (`Event`) and the time, and does what it says (`Action`): pick an
//! endpoint, connect, send, wait, relay, ask the retry budget, back off,
//! finish, or fail. Every state has a deadline, so no exchange waits forever.

use core::time::Duration;

use super::budget::is_idempotent;
use super::class::{ErrorClass, TimeoutKind};
use super::health::EndpointId;
use super::tick::Tick;
use crate::pool::PoolConfig;

/// The most attempts an exchange makes (`Retry::attempts` is at most 5).
const MAX_ATTEMPTS: usize = 5;

/// What happened, for [`Exchange::on`].
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    /// Start the exchange.
    Start,
    /// The balancer picked this endpoint.
    Picked(EndpointId),
    /// The balancer had no endpoint.
    NoEndpoint,
    /// The connection to the endpoint is ready (new or reused).
    Connected,
    /// Connecting failed.
    ConnectFailed(ErrorClass),
    /// The request head and body were sent in full.
    Sent,
    /// Bytes moved (request body sent, or response body read): idle
    /// deadlines start again.
    Progress,
    /// The response head arrived with this status (1xx are handled by the
    /// caller and not reported).
    ResponseHead(u16),
    /// The response ended; `reusable` says the upstream connection can go
    /// back to the pool as far as the parser can tell.
    ResponseEnd {
        /// The connection's own state allows reuse.
        reusable: bool,
    },
    /// The upstream failed after connecting.
    UpstreamFailed(ErrorClass),
    /// The client went away.
    ClientGone,
    /// The retry budget granted the retry asked for.
    RetryGranted,
    /// The retry budget refused it.
    RetryDenied,
    /// Time passed; deadlines are checked.
    Tick,
}

/// What to do next.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Ask the balancer for an endpoint, excluding [`Exchange::tried`].
    Pick {
        /// The attempt number, from 1.
        attempt: u8,
    },
    /// Take a pooled connection to the endpoint or dial one, by `deadline`.
    Connect {
        /// The endpoint.
        endpoint: EndpointId,
        /// When connecting times out.
        deadline: Tick,
    },
    /// Send the request (head, then body); a write that makes no progress
    /// by `deadline` times out.
    Send {
        /// The write idle deadline.
        deadline: Tick,
    },
    /// Wait for the response head until `deadline`.
    Wait {
        /// The response head deadline.
        deadline: Tick,
    },
    /// Relay the response head (on the first `Relay`) and body to the client;
    /// a read that brings nothing by `deadline` times out.
    Relay {
        /// The read idle deadline.
        deadline: Tick,
    },
    /// Ask the retry budget; answer with `RetryGranted` or `RetryDenied`.
    /// Discard any response from the failed attempt and close its connection.
    Retry {
        /// Why the attempt failed (report it to health).
        class: ErrorClass,
    },
    /// Wait until `until` before connecting again to the same endpoint.
    Backoff {
        /// The endpoint.
        endpoint: EndpointId,
        /// When to connect.
        until: Tick,
    },
    /// The exchange succeeded.
    Finish {
        /// The endpoint that answered.
        endpoint: EndpointId,
        /// Whether its connection goes back to the pool.
        reuse: bool,
    },
    /// The exchange failed. With a `status`, answer the client with it; with
    /// none, a response was already under way (or the client is gone): close
    /// the client connection.
    Fail {
        /// Why.
        class: ErrorClass,
        /// The status to answer with, if any.
        status: Option<u16>,
    },
    /// Nothing to do (the event did not apply to this state, or no deadline
    /// passed).
    Nothing,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    New,
    Picking,
    Connecting,
    Sending,
    Awaiting,
    Relaying,
    RetryWait,
    Backoff,
    Done,
    Failed,
}

/// The forwarding state machine of one request.
///
/// ```
/// use proxy_net::core::{Action, EndpointId, Event, Exchange, Tick};
/// use proxy_net::{Pool, ProxyBuilder};
///
/// let config = ProxyBuilder::new().pool("app", Pool::new(["10.0.0.1:80", "10.0.0.2:80"])).config()?;
/// let pool = config.pool("app").expect("pool");
/// let mut ex = Exchange::new(b"GET", true, pool, Tick(0));
/// assert_eq!(ex.on(Event::Start, Tick(0)), Action::Pick { attempt: 1 });
/// assert!(matches!(ex.on(Event::Picked(EndpointId(0)), Tick(0)), Action::Connect { .. }));
/// // The connection is refused: an idempotent request may retry.
/// let a = ex.on(Event::ConnectFailed(proxy_net::core::ErrorClass::Connect), Tick(1));
/// assert!(matches!(a, Action::Retry { .. }));
/// assert_eq!(ex.on(Event::RetryGranted, Tick(1)), Action::Pick { attempt: 2 });
/// assert_eq!(ex.tried(), &[EndpointId(0)]);
/// assert!(matches!(ex.on(Event::Picked(EndpointId(1)), Tick(1)), Action::Connect { .. }));
/// assert!(matches!(ex.on(Event::Connected, Tick(2)), Action::Send { .. }));
/// assert!(matches!(ex.on(Event::Sent, Tick(3)), Action::Wait { .. }));
/// assert!(matches!(ex.on(Event::ResponseHead(200), Tick(4)), Action::Relay { .. }));
/// assert_eq!(
///     ex.on(Event::ResponseEnd { reusable: true }, Tick(5)),
///     Action::Finish { endpoint: EndpointId(1), reuse: true },
/// );
/// # Ok::<(), proxy_net::ConfigError>(())
/// ```
#[derive(Clone, Debug)]
pub struct Exchange {
    state: State,
    idempotent: bool,
    replayable: bool,
    attempt: u8,
    max_attempts: u8,
    tried: [EndpointId; MAX_ATTEMPTS],
    tried_len: usize,
    current: Option<EndpointId>,
    relayed: bool,
    early: bool,
    last: Option<ErrorClass>,
    deadline: Option<(Tick, TimeoutKind)>,
    total: Option<Tick>,
    backoff_until: Tick,
    retry_on: u128,
    connect: Duration,
    response_head: Duration,
    read_idle: Duration,
    write_idle: Duration,
    backoff: Duration,
}

impl Exchange {
    /// An exchange for a request with `method` in `pool`, starting at `now`.
    /// `replayable` says the whole body (or no body) is held, so a retry can
    /// send it again.
    pub fn new(method: &[u8], replayable: bool, pool: &PoolConfig, now: Tick) -> Exchange {
        let retry = pool.retry();
        let t = pool.timeouts();
        let retry_on = retry
            .on_status_value()
            .iter()
            .filter(|s| (500..600).contains(*s))
            .fold(0u128, |m, s| m | (1u128 << (s - 500)));
        Exchange {
            state: State::New,
            idempotent: is_idempotent(method),
            replayable,
            attempt: 0,
            max_attempts: retry.attempts_value().clamp(1, MAX_ATTEMPTS as u8),
            tried: [EndpointId(0); MAX_ATTEMPTS],
            tried_len: 0,
            current: None,
            relayed: false,
            early: false,
            last: None,
            deadline: None,
            total: t.total_value().map(|d| now.after(d)),
            backoff_until: Tick(0),
            retry_on,
            connect: t.connect_value(),
            response_head: t.response_head_value(),
            read_idle: t.read_idle_value(),
            write_idle: t.write_idle_value(),
            backoff: retry.backoff_value(),
        }
    }

    /// The endpoints tried by earlier attempts, for the balancer to avoid.
    pub fn tried(&self) -> &[EndpointId] {
        &self.tried[..self.tried_len]
    }

    /// The endpoint of the current attempt.
    pub fn endpoint(&self) -> Option<EndpointId> {
        self.current
    }

    /// The attempt number, from 1 (0 before `Start`).
    pub fn attempt(&self) -> u8 {
        self.attempt
    }

    /// The last failure, if any.
    pub fn last_failure(&self) -> Option<ErrorClass> {
        self.last
    }

    /// True once the response head has been handed to the client.
    pub fn relayed(&self) -> bool {
        self.relayed
    }

    /// True when the exchange finished or failed.
    pub fn is_over(&self) -> bool {
        matches!(self.state, State::Done | State::Failed)
    }

    /// The next deadline, for the caller's timer.
    pub fn deadline(&self) -> Option<Tick> {
        if self.is_over() {
            return None;
        }
        let step = match self.state {
            State::Backoff => Some(self.backoff_until),
            _ => self.deadline.map(|d| d.0),
        };
        match (step, self.total) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    fn arm(&mut self, now: Tick, d: Duration, kind: TimeoutKind) -> Tick {
        let at = now.after(d);
        self.deadline = Some((at, kind));
        at
    }

    fn fail(&mut self, class: ErrorClass) -> Action {
        self.state = State::Failed;
        self.last = Some(class);
        self.deadline = None;
        let status = if self.relayed { None } else { class.status() };
        Action::Fail { class, status }
    }

    /// True when the request may be sent again after `class`. A failure
    /// while connecting (the request never left the proxy) may be retried
    /// for any method; after that, only for an idempotent one.
    fn may_retry(&self, class: ErrorClass) -> bool {
        let before_send = self.state == State::Connecting;
        usize::from(self.attempt) < usize::from(self.max_attempts)
            && (self.idempotent || before_send)
            && self.replayable
            && !self.relayed
            && class.is_retryable()
    }

    fn failed_attempt(&mut self, class: ErrorClass) -> Action {
        self.last = Some(class);
        if self.may_retry(class) {
            self.state = State::RetryWait;
            self.deadline = None;
            Action::Retry { class }
        } else {
            self.fail(class)
        }
    }

    /// Feeds one event at `now` and returns what to do.
    pub fn on(&mut self, event: Event, now: Tick) -> Action {
        if self.is_over() {
            return Action::Nothing;
        }
        if event == Event::ClientGone {
            return self.fail(ErrorClass::ClientGone);
        }
        if let Some(total) = self.total {
            if now >= total {
                return self.fail(ErrorClass::Timeout(TimeoutKind::Total));
            }
        }
        match (self.state, event) {
            (State::New, Event::Start) => {
                self.attempt = 1;
                self.state = State::Picking;
                Action::Pick { attempt: 1 }
            }
            (State::Picking, Event::NoEndpoint) => self.fail(ErrorClass::NoEndpoint),
            (State::Picking, Event::Picked(id)) => {
                let again = self.tried().contains(&id);
                self.current = Some(id);
                if again && !self.backoff.is_zero() {
                    self.state = State::Backoff;
                    self.backoff_until = now.after(self.backoff);
                    Action::Backoff {
                        endpoint: id,
                        until: self.backoff_until,
                    }
                } else {
                    self.connect_now(id, now)
                }
            }
            (State::Backoff, Event::Tick) if now >= self.backoff_until => match self.current {
                Some(id) => self.connect_now(id, now),
                None => self.fail(ErrorClass::NoEndpoint),
            },
            (State::Connecting, Event::Connected) => {
                self.state = State::Sending;
                let deadline = self.arm(now, self.write_idle, TimeoutKind::WriteIdle);
                Action::Send { deadline }
            }
            (State::Connecting, Event::ConnectFailed(class)) => self.failed_attempt(class),
            (State::Sending, Event::Progress) => {
                let deadline = self.arm(now, self.write_idle, TimeoutKind::WriteIdle);
                Action::Send { deadline }
            }
            (State::Sending, Event::Sent) => {
                self.state = State::Awaiting;
                let deadline = self.arm(now, self.response_head, TimeoutKind::ResponseHead);
                Action::Wait { deadline }
            }
            (State::Sending | State::Awaiting, Event::ResponseHead(status)) => {
                if self.retries_status(status) && self.may_retry(ErrorClass::UpstreamStatus) {
                    return self.failed_attempt(ErrorClass::UpstreamStatus);
                }
                // A response before the request was sent in full ends the
                // request; the connection is not reused.
                self.early = self.state == State::Sending;
                self.state = State::Relaying;
                self.relayed = true;
                let deadline = self.arm(now, self.read_idle, TimeoutKind::ReadIdle);
                Action::Relay { deadline }
            }
            (State::Relaying, Event::Progress) => {
                let deadline = self.arm(now, self.read_idle, TimeoutKind::ReadIdle);
                Action::Relay { deadline }
            }
            (State::Relaying, Event::ResponseEnd { reusable }) => match self.current {
                Some(endpoint) => {
                    self.state = State::Done;
                    self.deadline = None;
                    Action::Finish {
                        endpoint,
                        reuse: reusable && !self.early,
                    }
                }
                None => self.fail(ErrorClass::UpstreamProtocol),
            },
            (State::Sending | State::Awaiting | State::Relaying, Event::UpstreamFailed(class)) => {
                self.failed_attempt(class)
            }
            (State::RetryWait, Event::RetryGranted) => {
                if let Some(id) = self.current.take() {
                    if self.tried_len < MAX_ATTEMPTS && !self.tried().contains(&id) {
                        self.tried[self.tried_len] = id;
                        self.tried_len += 1;
                    }
                }
                self.attempt += 1;
                self.state = State::Picking;
                Action::Pick {
                    attempt: self.attempt,
                }
            }
            (State::RetryWait, Event::RetryDenied) => {
                let class = self.last.unwrap_or(ErrorClass::NoEndpoint);
                self.fail(class)
            }
            (_, Event::Tick) => self.check_deadline(now),
            _ => Action::Nothing,
        }
    }

    fn connect_now(&mut self, id: EndpointId, now: Tick) -> Action {
        self.state = State::Connecting;
        let deadline = self.arm(now, self.connect, TimeoutKind::Connect);
        Action::Connect {
            endpoint: id,
            deadline,
        }
    }

    fn retries_status(&self, status: u16) -> bool {
        (500..600).contains(&status) && self.retry_on & (1u128 << (status - 500)) != 0
    }

    fn check_deadline(&mut self, now: Tick) -> Action {
        match self.deadline {
            Some((at, kind)) if now >= at => self.failed_attempt(ErrorClass::Timeout(kind)),
            _ => Action::Nothing,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{ErrorClass, LimitKind};
    use crate::{Pool, PoolConfig, ProxyBuilder, Retry, Timeouts};

    const MS: u64 = 1_000_000;

    fn pool(p: Pool) -> PoolConfig {
        ProxyBuilder::new()
            .pool("p", p)
            .config()
            .expect("config")
            .pool("p")
            .expect("pool")
            .clone()
    }

    fn two() -> PoolConfig {
        pool(Pool::new(["10.0.0.1:80", "10.0.0.2:80"]))
    }

    fn connected(ex: &mut Exchange) {
        assert_eq!(ex.on(Event::Start, Tick(0)), Action::Pick { attempt: 1 });
        assert!(matches!(
            ex.on(Event::Picked(EndpointId(0)), Tick(0)),
            Action::Connect { .. }
        ));
        assert!(matches!(
            ex.on(Event::Connected, Tick(0)),
            Action::Send { .. }
        ));
    }

    #[test]
    fn test_non_idempotent_retries_only_before_sending() {
        // The request never left the proxy: a connect failure is retried for
        // a POST too.
        let mut ex = Exchange::new(b"POST", true, &two(), Tick(0));
        let _ = ex.on(Event::Start, Tick(0));
        let _ = ex.on(Event::Picked(EndpointId(0)), Tick(0));
        assert_eq!(
            ex.on(Event::ConnectFailed(ErrorClass::Connect), Tick(0)),
            Action::Retry {
                class: ErrorClass::Connect
            }
        );
        // A POST whose body is not held is not, even then.
        let mut ex = Exchange::new(b"POST", false, &two(), Tick(0));
        let _ = ex.on(Event::Start, Tick(0));
        let _ = ex.on(Event::Picked(EndpointId(0)), Tick(0));
        assert!(matches!(
            ex.on(Event::ConnectFailed(ErrorClass::Connect), Tick(0)),
            Action::Fail { .. }
        ));
    }

    #[test]
    fn test_non_idempotent_is_never_retried() {
        // T-14, E-122.
        let mut ex = Exchange::new(b"POST", true, &two(), Tick(0));
        connected(&mut ex);
        let _ = ex.on(Event::Sent, Tick(1));
        assert_eq!(
            ex.on(Event::UpstreamFailed(ErrorClass::UpstreamReset), Tick(2)),
            Action::Fail {
                class: ErrorClass::UpstreamReset,
                status: Some(502)
            }
        );
        assert!(ex.is_over());
        assert_eq!(ex.on(Event::RetryGranted, Tick(3)), Action::Nothing);
    }

    #[test]
    fn test_idempotent_reset_before_response_retries() {
        let mut ex = Exchange::new(b"GET", true, &two(), Tick(0));
        connected(&mut ex);
        let _ = ex.on(Event::Sent, Tick(1));
        assert_eq!(
            ex.on(Event::UpstreamFailed(ErrorClass::UpstreamReset), Tick(2)),
            Action::Retry {
                class: ErrorClass::UpstreamReset
            }
        );
        assert_eq!(
            ex.on(Event::RetryGranted, Tick(2)),
            Action::Pick { attempt: 2 }
        );
        // The only other attempt: the budget allowed 2 in total.
        assert!(matches!(
            ex.on(Event::Picked(EndpointId(1)), Tick(2)),
            Action::Connect { .. }
        ));
        assert!(matches!(
            ex.on(Event::ConnectFailed(ErrorClass::Connect), Tick(3)),
            Action::Fail {
                status: Some(502),
                ..
            }
        ));
    }

    #[test]
    fn test_no_retry_once_relayed_or_unreplayable() {
        // E-123: reset mid-response.
        let mut ex = Exchange::new(b"GET", true, &two(), Tick(0));
        connected(&mut ex);
        let _ = ex.on(Event::Sent, Tick(1));
        assert!(matches!(
            ex.on(Event::ResponseHead(200), Tick(2)),
            Action::Relay { .. }
        ));
        assert!(ex.relayed());
        assert_eq!(
            ex.on(Event::UpstreamFailed(ErrorClass::UpstreamReset), Tick(3)),
            Action::Fail {
                class: ErrorClass::UpstreamReset,
                status: None
            }
        );
        // E-132: a PUT whose body is not held.
        let mut ex = Exchange::new(b"PUT", false, &two(), Tick(0));
        connected(&mut ex);
        assert!(matches!(
            ex.on(Event::UpstreamFailed(ErrorClass::UpstreamReset), Tick(1)),
            Action::Fail { .. }
        ));
    }

    #[test]
    fn test_retry_denied_by_budget() {
        // E-131.
        let mut ex = Exchange::new(b"GET", true, &two(), Tick(0));
        let _ = ex.on(Event::Start, Tick(0));
        let _ = ex.on(Event::Picked(EndpointId(0)), Tick(0));
        let _ = ex.on(Event::ConnectFailed(ErrorClass::Tls), Tick(0));
        assert_eq!(
            ex.on(Event::RetryDenied, Tick(0)),
            Action::Fail {
                class: ErrorClass::Tls,
                status: Some(502)
            }
        );
    }

    #[test]
    fn test_retry_on_status_and_backoff_on_same_endpoint() {
        // E-130, E-133.
        let p = pool(
            Pool::new(["10.0.0.1:80"]).retry(
                Retry::default()
                    .on_status(&[503])
                    .backoff(Duration::from_millis(25)),
            ),
        );
        let mut ex = Exchange::new(b"GET", true, &p, Tick(0));
        connected(&mut ex);
        let _ = ex.on(Event::Sent, Tick(0));
        assert_eq!(
            ex.on(Event::ResponseHead(503), Tick(MS)),
            Action::Retry {
                class: ErrorClass::UpstreamStatus
            }
        );
        assert!(!ex.relayed());
        let _ = ex.on(Event::RetryGranted, Tick(MS));
        assert_eq!(
            ex.on(Event::Picked(EndpointId(0)), Tick(MS)),
            Action::Backoff {
                endpoint: EndpointId(0),
                until: Tick(26 * MS)
            }
        );
        assert_eq!(ex.deadline(), Some(Tick(26 * MS)));
        assert_eq!(ex.on(Event::Tick, Tick(25 * MS)), Action::Nothing);
        assert!(matches!(
            ex.on(Event::Tick, Tick(26 * MS)),
            Action::Connect { .. }
        ));
        let _ = ex.on(Event::Connected, Tick(27 * MS));
        let _ = ex.on(Event::Sent, Tick(27 * MS));
        // The last attempt relays whatever comes, 503 included.
        assert!(matches!(
            ex.on(Event::ResponseHead(503), Tick(28 * MS)),
            Action::Relay { .. }
        ));
        // A 500 not in the list is relayed at once.
        let mut ex = Exchange::new(b"GET", true, &p, Tick(0));
        connected(&mut ex);
        assert!(matches!(
            ex.on(Event::ResponseHead(500), Tick(1)),
            Action::Relay { .. }
        ));
    }

    #[test]
    fn test_timeouts() {
        let p = pool(
            Pool::new(["10.0.0.1:80"]).timeouts(
                Timeouts::default()
                    .connect(Duration::from_millis(10))
                    .response_head(Duration::from_millis(20))
                    .read_idle(Duration::from_millis(30))
                    .total(Duration::from_millis(100)),
            ),
        );
        // E-121: a connect timeout is retryable (another attempt, if any).
        let mut ex = Exchange::new(b"GET", true, &p, Tick(0));
        let _ = ex.on(Event::Start, Tick(0));
        let _ = ex.on(Event::Picked(EndpointId(0)), Tick(0));
        assert_eq!(ex.deadline(), Some(Tick(10 * MS)));
        assert_eq!(ex.on(Event::Tick, Tick(9 * MS)), Action::Nothing);
        assert_eq!(
            ex.on(Event::Tick, Tick(10 * MS)),
            Action::Retry {
                class: ErrorClass::Timeout(TimeoutKind::Connect)
            }
        );
        // E-124: a response head timeout is 504 and not retried.
        let mut ex = Exchange::new(b"GET", true, &p, Tick(0));
        connected(&mut ex);
        let _ = ex.on(Event::Sent, Tick(0));
        assert_eq!(
            ex.on(Event::Tick, Tick(20 * MS)),
            Action::Fail {
                class: ErrorClass::Timeout(TimeoutKind::ResponseHead),
                status: Some(504)
            }
        );
        // E-125: read idle mid-body; progress restarts it.
        let mut ex = Exchange::new(b"GET", true, &p, Tick(0));
        connected(&mut ex);
        let _ = ex.on(Event::Sent, Tick(0));
        let _ = ex.on(Event::ResponseHead(200), Tick(MS));
        assert!(
            matches!(ex.on(Event::Progress, Tick(20 * MS)), Action::Relay { deadline } if deadline == Tick(50 * MS))
        );
        assert_eq!(
            ex.on(Event::Tick, Tick(50 * MS)),
            Action::Fail {
                class: ErrorClass::Timeout(TimeoutKind::ReadIdle),
                status: None
            }
        );
        // The total deadline wins whatever the state.
        let mut ex = Exchange::new(b"GET", true, &p, Tick(0));
        connected(&mut ex);
        let _ = ex.on(Event::Sent, Tick(0));
        let _ = ex.on(Event::ResponseHead(200), Tick(MS));
        for i in 2..10 {
            let _ = ex.on(Event::Progress, Tick(i * 10 * MS));
        }
        assert_eq!(ex.deadline(), Some(Tick(100 * MS)));
        assert!(matches!(
            ex.on(Event::Progress, Tick(100 * MS)),
            Action::Fail {
                class: ErrorClass::Timeout(TimeoutKind::Total),
                status: None
            }
        ));
        assert_eq!(ex.deadline(), None);
    }

    #[test]
    fn test_early_response_is_not_reused() {
        let mut ex = Exchange::new(b"POST", true, &two(), Tick(0));
        connected(&mut ex);
        assert!(matches!(
            ex.on(Event::Progress, Tick(1)),
            Action::Send { .. }
        ));
        // 413 from the upstream before the body was sent.
        assert!(matches!(
            ex.on(Event::ResponseHead(413), Tick(2)),
            Action::Relay { .. }
        ));
        assert_eq!(
            ex.on(Event::ResponseEnd { reusable: true }, Tick(3)),
            Action::Finish {
                endpoint: EndpointId(0),
                reuse: false
            }
        );
    }

    #[test]
    fn test_client_gone_no_endpoint_and_stray_events() {
        let mut ex = Exchange::new(b"GET", true, &two(), Tick(0));
        assert_eq!(ex.on(Event::Sent, Tick(0)), Action::Nothing);
        let _ = ex.on(Event::Start, Tick(0));
        assert_eq!(
            ex.on(Event::NoEndpoint, Tick(0)),
            Action::Fail {
                class: ErrorClass::NoEndpoint,
                status: Some(503)
            }
        );
        let mut ex = Exchange::new(b"GET", true, &two(), Tick(0));
        connected(&mut ex);
        assert_eq!(
            ex.on(Event::ClientGone, Tick(1)),
            Action::Fail {
                class: ErrorClass::ClientGone,
                status: None
            }
        );
        assert_eq!(ex.last_failure(), Some(ErrorClass::ClientGone));
        assert_eq!(ex.on(Event::Tick, Tick(u64::MAX)), Action::Nothing);
        // A limit after relaying closes the client connection.
        let mut ex = Exchange::new(b"GET", true, &two(), Tick(0));
        connected(&mut ex);
        let _ = ex.on(Event::Sent, Tick(0));
        let _ = ex.on(Event::ResponseHead(200), Tick(0));
        assert_eq!(ex.on(Event::ResponseHead(200), Tick(0)), Action::Nothing);
        assert_eq!(
            ex.on(
                Event::UpstreamFailed(ErrorClass::Limit(LimitKind::ResponseBody)),
                Tick(0)
            ),
            Action::Fail {
                class: ErrorClass::Limit(LimitKind::ResponseBody),
                status: None
            }
        );
        assert_eq!(ex.endpoint(), Some(EndpointId(0)));
        assert_eq!(ex.attempt(), 1);
    }
}
