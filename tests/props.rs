//! Property tests of the core's invariants.

use proptest::prelude::*;
use proxy_net::core::{
    Action, EndpointId, ErrorClass, Event, Exchange, FieldList, ForwardFacts, LimitKind, Tick,
    TimeoutKind, check_path, request_fields,
};
use proxy_net::{PathCheck, PathPolicy, Pool, ProxyBuilder, Retry};

fn event() -> impl Strategy<Value = Event> {
    let class = prop_oneof![
        Just(ErrorClass::Connect),
        Just(ErrorClass::Tls),
        Just(ErrorClass::UpstreamReset),
        Just(ErrorClass::UpstreamProtocol),
        Just(ErrorClass::Timeout(TimeoutKind::Connect)),
        Just(ErrorClass::Limit(LimitKind::ResponseBody)),
    ];
    prop_oneof![
        Just(Event::Start),
        (0u16..3).prop_map(|i| Event::Picked(EndpointId(i))),
        Just(Event::NoEndpoint),
        Just(Event::Connected),
        class.clone().prop_map(Event::ConnectFailed),
        Just(Event::Sent),
        Just(Event::Progress),
        prop_oneof![Just(200u16), Just(404), Just(500), Just(503)].prop_map(Event::ResponseHead),
        any::<bool>().prop_map(|reusable| Event::ResponseEnd { reusable }),
        class.prop_map(Event::UpstreamFailed),
        Just(Event::ClientGone),
        Just(Event::RetryGranted),
        Just(Event::RetryDenied),
        Just(Event::Tick),
    ]
}

/// Percent-decodes `%XX`; an invalid escape is kept as it is.
fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        // Two hex digits exactly (`from_str_radix` alone would take "+0").
        if b[i] == b'%'
            && i + 3 <= b.len()
            && b[i + 1].is_ascii_hexdigit()
            && b[i + 2].is_ascii_hexdigit()
        {
            if let Some(v) = s
                .get(i + 1..i + 3)
                .and_then(|h| u8::from_str_radix(h, 16).ok())
            {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    /// No sequence of events gives two endings, a retry after a relayed
    /// response, a retry of a non-idempotent method or of a body not held, or
    /// a status after the response head went out.
    #[test]
    fn prop_exchange_invariants(
        method in prop_oneof![Just("GET"), Just("POST"), Just("PUT")],
        replayable in any::<bool>(),
        events in proptest::collection::vec((event(), 0u64..200_000_000_000), 0..60),
    ) {
        let c = ProxyBuilder::new()
            .pool("p", Pool::new(["10.0.0.1:80", "10.0.0.2:80", "10.0.0.3:80"])
                .retry(Retry::default().attempts(3).on_status(&[503])))
            .config()
            .expect("config");
        let mut ex = Exchange::new(method.as_bytes(), replayable, &c.pools()[0], Tick(0));
        let mut now = 0u64;
        let mut endings = 0;
        let mut relayed = false;
        let mut retries = 0;
        for (e, dt) in events {
            now = now.saturating_add(dt);
            let a = ex.on(e, Tick(now));
            match a {
                Action::Relay { .. } => relayed = true,
                Action::Retry { .. } => {
                    retries += 1;
                    prop_assert!(!relayed, "retry after relay");
                    prop_assert!(method != "POST", "POST retried");
                    prop_assert!(replayable, "unreplayable retried");
                }
                Action::Finish { .. } | Action::Fail { .. } => endings += 1,
                _ => {}
            }
            if let Action::Fail { status: Some(_), .. } = a {
                prop_assert!(!relayed, "a status after the head was relayed");
            }
        }
        prop_assert!(endings <= 1);
        prop_assert!(retries <= 2);
        prop_assert!(usize::from(ex.attempt()) <= 3);
    }

    /// A path accepted under STRICT holds none of the refused forms, by an
    /// independent decode-and-look check.
    #[test]
    fn prop_strict_paths_have_no_ambiguity(
        segments in proptest::collection::vec(
            prop_oneof![
                Just("a".to_string()), Just(".".to_string()), Just("..".to_string()),
                Just("%2e".to_string()), Just("%2E%2e".to_string()), Just("%2f".to_string()),
                Just("%5C".to_string()), Just("%00".to_string()), Just("x;y".to_string()),
                Just("\\".to_string()), Just("%41".to_string()), "[a-z.%0-9]{0,6}",
            ],
            1..8,
        ),
    ) {
        let path = format!("/{}", segments.join("/"));
        if check_path(path.as_bytes(), PathPolicy::STRICT, true).is_ok() {
            let decoded = decode(&path);
            prop_assert!(!decoded.contains('\\'), "{}", path);
            prop_assert!(!decoded.contains('\0'), "{}", path);
            prop_assert!(!path.contains(';'), "{}", path);
            for seg in path.split('/') {
                let d = decode(seg);
                prop_assert!(d != "." && d != "..", "{}", path);
                prop_assert!(!d.contains('/'), "{}", path);
            }
            // Allowing a check never makes a passing path fail.
            prop_assert!(
                check_path(path.as_bytes(), PathPolicy::STRICT.allow(PathCheck::Semicolon), true).is_ok()
            );
        }
    }

    /// Head rewriting never forwards a hop-by-hop field, a field the client
    /// named in `Connection` (other than the proxy's own), or a client's
    /// forwarding value, and writes one `Host`.
    #[test]
    fn prop_request_fields_invariants(
        fields in proptest::collection::vec(
            (
                prop_oneof![
                    Just("connection"), Just("keep-alive"), Just("te"), Just("upgrade"),
                    Just("x-forwarded-for"), Just("forwarded"), Just("x-a"), Just("x-b"),
                    Just("content-length"), Just("transfer-encoding"), Just("via"), Just("host"),
                ],
                prop_oneof![Just("x-a"), Just("x-b, x-a"), Just("close"), Just("1"), Just("1.1 other")],
            ),
            0..20,
        ),
    ) {
        let c = ProxyBuilder::new().pool("p", Pool::new(["10.0.0.1:80"])).config().expect("config");
        let e = &c.pools()[0].endpoints()[0];
        let facts = ForwardFacts::new(b"GET", b"/", b"h", e).client("192.0.2.1:1".parse().ok());
        let input: Vec<(&str, &[u8])> = fields.iter().map(|(n, v)| (*n, v.as_bytes())).collect();
        let (mut out, mut target) = (FieldList::new(), Vec::new());
        if request_fields(&input, &facts, c.routes().get(0).expect("route"), &c, &mut out, &mut target).is_ok() {
            let named: Vec<&str> = input
                .iter()
                .filter(|(n, _)| *n == "connection")
                .flat_map(|(_, v)| std::str::from_utf8(v).unwrap_or("").split(',').map(str::trim))
                .collect();
            for (n, v) in out.iter() {
                prop_assert!(
                    !["connection", "keep-alive", "te", "upgrade", "content-length", "transfer-encoding"].contains(&n),
                    "{}", n
                );
                prop_assert!(
                    !named.contains(&n) || ["via", "x-forwarded-for", "forwarded", "host", "x-forwarded-proto", "x-forwarded-host"].contains(&n),
                    "{}", n
                );
                if n == "x-forwarded-for" {
                    prop_assert_eq!(v, &b"192.0.2.1"[..]);
                }
            }
            prop_assert_eq!(out.get_all("host").count(), 1);
        }
    }
}
