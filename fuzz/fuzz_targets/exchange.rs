//! The exchange state machine on arbitrary event sequences and times.
//! Oracle: it never panics; at most one ending; no retry after the response
//! head was relayed, of a non-idempotent method, or of a body not held; no
//! status after the head was relayed; no more attempts than configured.

#![no_main]

use std::sync::OnceLock;

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use proxy_net::core::{Action, EndpointId, ErrorClass, Event, Exchange, LimitKind, Tick, TimeoutKind};
use proxy_net::{Config, Pool, ProxyBuilder, Retry, Timeouts};

fn config() -> &'static Config {
    static C: OnceLock<Config> = OnceLock::new();
    C.get_or_init(|| {
        ProxyBuilder::new()
            .pool(
                "p",
                Pool::new(["10.0.0.1:80", "10.0.0.2:80"])
                    .retry(Retry::default().attempts(4).on_status(&[502, 503]))
                    .timeouts(Timeouts::default().total(std::time::Duration::from_secs(30))),
            )
            .config()
            .expect("the fixed configuration builds")
    })
}

#[derive(Arbitrary, Debug, Clone, Copy)]
enum Class {
    Connect,
    Tls,
    Reset,
    Protocol,
    ConnectTimeout,
    Limit,
    Desync,
}

impl Class {
    fn get(self) -> ErrorClass {
        match self {
            Class::Connect => ErrorClass::Connect,
            Class::Tls => ErrorClass::Tls,
            Class::Reset => ErrorClass::UpstreamReset,
            Class::Protocol => ErrorClass::UpstreamProtocol,
            Class::ConnectTimeout => ErrorClass::Timeout(TimeoutKind::Connect),
            Class::Limit => ErrorClass::Limit(LimitKind::ResponseBody),
            Class::Desync => ErrorClass::Desync,
        }
    }
}

#[derive(Arbitrary, Debug, Clone, Copy)]
enum Ev {
    Start,
    Picked(u8),
    NoEndpoint,
    Connected,
    ConnectFailed(Class),
    Sent,
    Progress,
    ResponseHead(u16),
    ResponseEnd(bool),
    UpstreamFailed(Class),
    ClientGone,
    RetryGranted,
    RetryDenied,
    Tick,
}

impl Ev {
    fn get(self) -> Event {
        match self {
            Ev::Start => Event::Start,
            Ev::Picked(i) => Event::Picked(EndpointId(u16::from(i % 4))),
            Ev::NoEndpoint => Event::NoEndpoint,
            Ev::Connected => Event::Connected,
            Ev::ConnectFailed(c) => Event::ConnectFailed(c.get()),
            Ev::Sent => Event::Sent,
            Ev::Progress => Event::Progress,
            Ev::ResponseHead(s) => Event::ResponseHead(s),
            Ev::ResponseEnd(r) => Event::ResponseEnd { reusable: r },
            Ev::UpstreamFailed(c) => Event::UpstreamFailed(c.get()),
            Ev::ClientGone => Event::ClientGone,
            Ev::RetryGranted => Event::RetryGranted,
            Ev::RetryDenied => Event::RetryDenied,
            Ev::Tick => Event::Tick,
        }
    }
}

#[derive(Arbitrary, Debug)]
struct Input {
    method: u8,
    replayable: bool,
    events: Vec<(Ev, u32)>,
}

fuzz_target!(|input: Input| {
    let method: &[u8] = match input.method % 4 {
        0 => b"GET",
        1 => b"POST",
        2 => b"PUT",
        _ => b"PATCH",
    };
    let mut ex = Exchange::new(method, input.replayable, &config().pools()[0], Tick(0));
    let (mut now, mut endings, mut relayed) = (0u64, 0, false);
    for (e, dt) in input.events {
        now = now.saturating_add(u64::from(dt) * 1_000_000);
        let a = ex.on(e.get(), Tick(now));
        match a {
            Action::Relay { .. } => relayed = true,
            Action::Retry { .. } => {
                assert!(!relayed);
                assert!(method == b"GET" || method == b"PUT");
                assert!(input.replayable);
            }
            Action::Finish { .. } => endings += 1,
            Action::Fail { status, .. } => {
                endings += 1;
                assert!(status.is_none() || !relayed);
            }
            _ => {}
        }
        assert!(ex.attempt() <= 4);
        if let Some(d) = ex.deadline() {
            assert!(!ex.is_over() || d == Tick(0));
        }
    }
    assert!(endings <= 1);
});
