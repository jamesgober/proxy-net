//! Attack tests at the core: each names the threat it stops. The same
//! threats get socket-level tests when forwarding arrives (0.4).

use proxy_net::core::{
    Action, Balancer, EndpointId, ErrorClass, Event, Exchange, FieldList, ForwardFacts,
    HealthTable, Outcome, RefusalKind, RetryBudget, Tick, check_request, request_fields,
};
use proxy_net::{
    ConfigReason, FORWARDING_SET, HeaderRule, Outlier, Pool, ProxyBuilder, Retry, Route,
};

fn single() -> proxy_net::Config {
    ProxyBuilder::new()
        .pool("p", Pool::new(["10.0.0.1:80", "10.0.0.2:80"]))
        .config()
        .expect("config")
}

fn forward(c: &proxy_net::Config, fields: &[(&str, &[u8])]) -> Result<FieldList, ErrorClass> {
    let e = &c.pools()[0].endpoints()[0];
    let facts =
        ForwardFacts::new(b"GET", b"/", b"example.com", e).client("198.51.100.7:1".parse().ok());
    let (mut out, mut target) = (FieldList::new(), Vec::new());
    request_fields(
        fields,
        &facts,
        c.routes().get(0).expect("route"),
        c,
        &mut out,
        &mut target,
    )?;
    Ok(out)
}

#[test]
fn test_t3_spoofed_forwarding_headers_are_rebuilt() {
    let c = single();
    let spoof: Vec<(&str, &[u8])> = FORWARDING_SET
        .iter()
        .map(|n| (*n, &b"127.0.0.1"[..]))
        .collect();
    let out = forward(&c, &spoof).expect("forward");
    assert!(
        out.iter()
            .all(|(_, v)| !v.windows(9).any(|w| w == b"127.0.0.1"))
    );
    assert_eq!(out.get("x-forwarded-for"), Some(&b"198.51.100.7"[..]));
    assert_eq!(out.get_all("x-forwarded-for").count(), 1);
}

#[test]
fn test_t4_body_on_get_is_refused() {
    let c = single();
    let route = c.routes().get(0).expect("route");
    for m in ["GET", "HEAD", "OPTIONS", "DELETE"] {
        assert_eq!(
            check_request(m.as_bytes(), b"/", true, route),
            Err(ErrorClass::Refused(RefusalKind::BodyNotAllowed)),
            "{m}"
        );
        assert_eq!(
            check_request(m.as_bytes(), b"/", false, route),
            Ok(()),
            "{m}"
        );
    }
    assert_eq!(
        check_request(b"TRACE", b"/", false, route),
        Err(ErrorClass::Refused(RefusalKind::Method))
    );
    assert_eq!(
        check_request(b"CONNECT", b"/", false, route),
        Err(ErrorClass::Refused(RefusalKind::Method))
    );
}

#[test]
fn test_t5_header_injection_through_rules_is_refused_at_build() {
    for value in ["a\r\nX-Injected: 1", "a\nb", "a\rb", "\0"] {
        let err = ProxyBuilder::new()
            .pool("p", Pool::new(["10.0.0.1:80"]))
            .route(
                Route::any()
                    .to("p")
                    .request_header(HeaderRule::set("x-a", value)),
            )
            .config()
            .expect_err("refused");
        assert_eq!(err.reason(), &ConfigReason::HeaderValue, "{value:?}");
    }
}

#[test]
fn test_t6_connection_cannot_strip_the_proxys_fields() {
    let c = single();
    let out = forward(
        &c,
        &[(
            "Connection",
            b"x-forwarded-for, forwarded, via, x-forwarded-proto",
        )],
    )
    .expect("forward");
    for name in ["x-forwarded-for", "forwarded", "via", "x-forwarded-proto"] {
        assert!(out.get(name).is_some(), "{name}");
    }
}

#[test]
fn test_t7_ambiguous_paths_are_refused() {
    let c = single();
    let route = c.routes().get(0).expect("route");
    for path in [
        "/a/../admin",
        "/a/%2e%2e/admin",
        "/a%2fadmin",
        "/a\\admin",
        "/app/..;/admin",
        "/a%00",
    ] {
        assert_eq!(
            check_request(b"GET", path.as_bytes(), false, route),
            Err(ErrorClass::Refused(RefusalKind::AmbiguousPath)),
            "{path}"
        );
    }
}

#[test]
fn test_t10_h2c_upgrade_is_not_forwarded() {
    let c = single();
    let out = forward(
        &c,
        &[
            ("Upgrade", b"h2c"),
            ("HTTP2-Settings", b"AAMAAABkAARAAAAAAAIAAAAA"),
            ("Connection", b"Upgrade, HTTP2-Settings"),
        ],
    )
    .expect("forward");
    assert_eq!(out.get("upgrade"), None);
    assert_eq!(out.get("http2-settings"), None);
    assert_eq!(out.get("connection"), None);
}

#[test]
fn test_t14_post_is_never_sent_twice() {
    let c = single();
    let pool = &c.pools()[0];
    // Once the request may have reached the upstream, a POST is not retried.
    for failure in [ErrorClass::UpstreamReset, ErrorClass::UpstreamStatus] {
        let mut ex = Exchange::new(b"POST", true, pool, Tick(0));
        let _ = ex.on(Event::Start, Tick(0));
        let _ = ex.on(Event::Picked(EndpointId(0)), Tick(0));
        let _ = ex.on(Event::Connected, Tick(0));
        let _ = ex.on(Event::Sent, Tick(0));
        let action = ex.on(Event::UpstreamFailed(failure), Tick(0));
        assert!(
            matches!(action, Action::Fail { .. }),
            "{failure:?}: {action:?}"
        );
    }
    // A failure while connecting is retried: the request never left.
    for failure in [ErrorClass::Connect, ErrorClass::Tls] {
        let mut ex = Exchange::new(b"POST", true, pool, Tick(0));
        let _ = ex.on(Event::Start, Tick(0));
        let _ = ex.on(Event::Picked(EndpointId(0)), Tick(0));
        let action = ex.on(Event::ConnectFailed(failure), Tick(0));
        assert!(
            matches!(action, Action::Retry { .. }),
            "{failure:?}: {action:?}"
        );
    }
}

#[test]
fn test_t15_retries_stay_within_the_budget() {
    let p = ProxyBuilder::new()
        .pool(
            "p",
            Pool::new(["10.0.0.1:80", "10.0.0.2:80"]).retry(Retry::default().attempts(5)),
        )
        .config()
        .expect("config");
    let pool = &p.pools()[0];
    let budget = RetryBudget::new(pool.retry());
    let mut exchanges: Vec<Exchange> = (0..100)
        .map(|_| Exchange::new(b"GET", true, pool, Tick(0)))
        .collect();
    for _ in &exchanges {
        budget.exchange_started();
    }
    let mut granted = 0;
    for ex in &mut exchanges {
        let _ = ex.on(Event::Start, Tick(0));
        let _ = ex.on(Event::Picked(EndpointId(0)), Tick(0));
        if let Action::Retry { .. } = ex.on(Event::ConnectFailed(ErrorClass::Connect), Tick(0)) {
            if budget.try_retry() {
                granted += 1;
                let _ = ex.on(Event::RetryGranted, Tick(0));
            } else {
                let _ = ex.on(Event::RetryDenied, Tick(0));
            }
        }
    }
    // A pool failing everything: 20% of 100 active exchanges retry, no more.
    assert_eq!(granted, 20);
    assert_eq!(budget.retries(), 20);
}

#[test]
fn test_t15_failing_endpoints_are_ejected_and_avoided() {
    let p = ProxyBuilder::new()
        .pool(
            "p",
            Pool::new(["10.0.0.1:80", "10.0.0.2:80", "10.0.0.3:80"])
                .outlier(Outlier::default().consecutive_failures(3)),
        )
        .config()
        .expect("config");
    let pool = &p.pools()[0];
    let (health, balancer) = (HealthTable::new(pool), Balancer::new(pool));
    let _ = balancer.refresh(&health);
    for _ in 0..3 {
        if health
            .record(EndpointId(1), Outcome::Failure, Tick(0))
            .is_some()
        {
            let _ = balancer.refresh(&health);
        }
    }
    for _ in 0..30 {
        assert_ne!(balancer.pick(None, &[]), Some(EndpointId(1)));
    }
}

#[test]
fn test_t17_a_loop_is_refused() {
    let c = ProxyBuilder::new()
        .pool("p", Pool::new(["10.0.0.1:80"]))
        .via("edge-7")
        .config()
        .expect("config");
    assert_eq!(
        forward(&c, &[("Via", b"1.1 cdn, 1.1 edge-7")]).err(),
        Some(ErrorClass::Loop)
    );
    assert_eq!(ErrorClass::Loop.status(), Some(508));
}

#[test]
fn test_frame_field_from_a_client_is_never_forwarded() {
    let c = single();
    let out = forward(&c, &[("Proxy-Frame", b"length=0; fields=1")]).expect("forward");
    assert_eq!(out.get("proxy-frame"), None);
}
