//! Configuration validation as data: endpoints (edge cases E-1 to E-25 of the
//! specification), header rules, limits, and builder errors.

use std::time::Duration;

use proxy_net::{
    ConfigItem, ConfigReason, Endpoint, EndpointAddr, EndpointErrorKind, HeaderRule, HostHeader,
    Limits, Pool, PoolLimits, ProxyBuilder, Retry, Route, Timeouts,
};

fn kind(text: &str) -> Option<EndpointErrorKind> {
    match Endpoint::parse(text) {
        Ok(_) => None,
        Err(e) => match e.reason() {
            ConfigReason::EndpointSyntax(k) => Some(*k),
            other => panic!("{text}: {other:?}"),
        },
    }
}

#[test]
fn test_endpoint_table_accepts_and_refuses() {
    use EndpointErrorKind::*;
    let label63 = format!("{}:80", "a".repeat(63));
    let label64 = format!("{}:80", "a".repeat(64));
    let name253 = format!("{}b:80", "a.".repeat(126));
    let name254 = format!("{}bc:80", "a.".repeat(126));
    let cases: Vec<(&str, Option<EndpointErrorKind>)> = vec![
        ("", Some(Empty)),                    // E-1
        (":80", Some(BadHost)),               // E-2
        ("host", Some(MissingPort)),          // E-3
        ("host:", Some(MissingPort)),         // E-4
        ("host:0", Some(BadPort)),            // E-5
        ("host:65535", None),                 // E-6
        ("host:65536", Some(BadPort)),        // E-7
        ("host:080", Some(BadPort)),          // E-8
        ("host:+80", Some(BadByte)),          // E-9
        ("1.2.3.4:80", None),                 // E-10
        ("1.2.3:80", Some(BadHost)),          // E-11
        ("[::1]:80", None),                   // E-12
        ("::1:80", Some(BadHost)),            // E-13
        ("[::1]", Some(MissingPort)),         // E-14
        ("[fe80::1%eth0]:80", Some(BadByte)), // E-15
        ("user@host:80", Some(UserInfo)),     // E-16
        ("host:80/path", Some(Path)),         // E-17
        (&label63, None),                     // E-18
        (&label64, Some(BadHost)),            // E-18
        (&name253, None),                     // E-19
        (&name254, Some(BadHost)),            // E-19
        ("-host:80", Some(BadHost)),          // E-20
        ("host-:80", Some(BadHost)),          // E-20
        ("HOST.Example:80", None),            // E-21
        ("host.:80", None),                   // E-22
        ("host..x:80", Some(BadHost)),        // E-23
        ("h\u{f6}st:80", Some(BadByte)),      // E-24
        (" host:80", Some(BadByte)),          // E-25
        ("host :80", Some(BadByte)),          // E-25
        ("host:80 ", Some(BadByte)),          // E-25
        ("010.0.0.1:80", Some(BadHost)),
        ("[::1]:080", Some(BadPort)),
        ("[1.2.3.4]:80", Some(BadHost)),
        ("host:80#x", Some(Path)),
    ];
    for (text, want) in cases {
        assert_eq!(kind(text), want, "{text:?}");
    }
}

#[test]
fn test_endpoint_names_are_normalized() {
    let e = Endpoint::parse("HOST.Example.:80").expect("parse");
    assert_eq!(
        e.addr(),
        &EndpointAddr::Name {
            host: "host.example".into(),
            port: 80
        }
    );
    assert_eq!(e.addr().to_string(), "host.example:80");
}

#[test]
fn test_header_rule_table() {
    let check = |rule: HeaderRule| -> Result<(), ConfigReason> {
        ProxyBuilder::new()
            .pool("app", Pool::new(["10.0.0.1:80"]))
            .route(Route::any().to("app").request_header(rule))
            .config()
            .map(|_| ())
            .map_err(|e| e.reason().clone())
    };
    // E-70: fields the proxy owns.
    for name in [
        "Content-Length",
        "Transfer-Encoding",
        "Connection",
        "Host",
        "Via",
        "X-Forwarded-For",
        "Forwarded",
        "Keep-Alive",
        "TE",
        "Trailer",
        "Upgrade",
        "Proxy-Authorization",
        "Expect",
    ] {
        assert_eq!(
            check(HeaderRule::set(name, "x")),
            Err(ConfigReason::ReservedHeader),
            "{name}"
        );
    }
    // E-71: values with control bytes.
    for value in ["a\rb", "a\nb", "a\0b", "a\x7fb", "\x01"] {
        assert_eq!(
            check(HeaderRule::add("x-a", value)),
            Err(ConfigReason::HeaderValue),
            "{value:?}"
        );
    }
    // E-72 to E-75.
    assert_eq!(check(HeaderRule::set("x-a", "")), Ok(()));
    assert_eq!(check(HeaderRule::remove("x-absent")), Ok(()));
    assert_eq!(check(HeaderRule::add("x-a", "1")), Ok(()));
    assert_eq!(
        check(HeaderRule::set("x a", "1")),
        Err(ConfigReason::HeaderName)
    );
}

#[test]
fn test_route_rule_order_is_kept() {
    let config = ProxyBuilder::new()
        .pool("app", Pool::new(["10.0.0.1:80"]))
        .route(
            Route::any()
                .to("app")
                .request_header(HeaderRule::set("x-a", "1"))
                .request_header(HeaderRule::remove("x-a"))
                .response_header(HeaderRule::remove("server"))
                .host_header(HostHeader::Endpoint),
        )
        .config()
        .expect("config");
    let route = config.routes().get(0).expect("route");
    let names: Vec<&str> = route.request_rules().iter().map(HeaderRule::name).collect();
    assert_eq!(names, ["x-a", "x-a"]);
    assert!(matches!(route.request_rules()[1], HeaderRule::Remove(_)));
    assert_eq!(route.response_rules().len(), 1);
    assert_eq!(route.host_header(), &HostHeader::Endpoint);
}

#[test]
fn test_rewrites_are_checked_against_the_route() {
    let build = |route: Route| {
        ProxyBuilder::new()
            .pool("app", Pool::new(["10.0.0.1:80"]))
            .route(route.to("app"))
            .config()
            .map(|_| ())
            .map_err(|e| e.reason().clone())
    };
    assert_eq!(build(Route::any().path("/v1").strip_prefix("/v1")), Ok(()));
    // E-76: a prefix equal to the path leaves "/".
    assert_eq!(build(Route::any().exact("/v1").strip_prefix("/v1")), Ok(()));
    // E-77: a prefix that ends inside a segment.
    assert_eq!(
        build(Route::any().path("/v10").strip_prefix("/v1")),
        Err(ConfigReason::PathRewrite)
    );
    assert_eq!(
        build(Route::any().path("/a").replace_prefix("/a", "/b/../c")),
        Err(ConfigReason::PathRewrite)
    );
}

#[test]
fn test_pool_errors_carry_the_pool() {
    let err = ProxyBuilder::new()
        .pool(
            "db",
            Pool::new(["10.0.0.1:5432"]).timeouts(Timeouts::default().read_idle(Duration::ZERO)),
        )
        .config()
        .expect_err("refused");
    assert_eq!(err.item(), &ConfigItem::Pool("db".into()));
    assert_eq!(
        err.reason(),
        &ConfigReason::Limit {
            field: "timeouts.read_idle"
        }
    );
    assert_eq!(
        err.to_string(),
        "pool \"db\": timeouts.read_idle is out of range"
    );
    let err = ProxyBuilder::new()
        .pool(
            "p",
            Pool::new(["10.0.0.1:80"]).retry(Retry::default().attempts(9)),
        )
        .config()
        .expect_err("refused");
    assert_eq!(
        err.reason(),
        &ConfigReason::Limit {
            field: "retry.attempts"
        }
    );
    assert!(
        ProxyBuilder::new()
            .pool(
                "p",
                Pool::new(["10.0.0.1:80"]).limits(PoolLimits::default().max_idle(0))
            )
            .config()
            .is_ok()
    );
}

#[test]
fn test_limits_errors_are_proxy_wide() {
    let err = ProxyBuilder::new()
        .pool("p", Pool::new(["10.0.0.1:80"]))
        .limits(Limits::default().relay_buffer(10))
        .config()
        .expect_err("refused");
    assert_eq!(err.item(), &ConfigItem::Proxy);
}
