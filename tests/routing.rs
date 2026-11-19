//! Route matching: host patterns (E-30 to E-38), path patterns (E-40 to E-48),
//! shadowing, and agreement with a naive reference matcher on generated
//! requests.

use proxy_net::core::{RouteMatch, RouteTable};
use proxy_net::{Config, ConfigReason, Pool, ProxyBuilder, Route, UnknownHost};

fn config(routes: Vec<Route>) -> Config {
    routes
        .into_iter()
        .fold(
            ProxyBuilder::new().pool("p", Pool::new(["10.0.0.1:80"])),
            |b, r| b.route(r.to("p")),
        )
        .config()
        .expect("config")
}

fn found(t: &RouteTable, host: &str, path: &str, method: &str) -> Option<usize> {
    match t.find(host.as_bytes(), path.as_bytes(), method.as_bytes()) {
        RouteMatch::Route { index, .. } => Some(index),
        _ => None,
    }
}

#[test]
fn test_host_cases() {
    let c = config(vec![
        Route::host("api.example.com"),
        Route::host("*.example.com"),
        Route::host("[::1]"),
        Route::host("192.0.2.1"),
    ]);
    let t = c.routes();
    let cases = [
        ("api.example.com", Some(0)),      // E-30
        ("API.EXAMPLE.COM", Some(0)),      // E-30
        ("api.example.com.", Some(0)),     // E-31
        ("api.example.com:8080", Some(0)), // E-32
        ("example.com", None),             // E-33
        ("a.example.com", Some(1)),        // E-33
        ("a.b.example.com", Some(1)),      // E-33
        ("[::1]", Some(2)),                // E-36
        ("[::1]:443", Some(2)),            // E-36
        ("192.0.2.1:80", Some(3)),
        ("", None), // E-37
        ("xexample.com", None),
        ("a.example.com.evil", None),
    ];
    for (host, want) in cases {
        assert_eq!(found(t, host, "/", "GET"), want, "{host:?}");
    }
    assert_eq!(
        t.find(b"example.com", b"/", b"GET"),
        RouteMatch::UnknownHost
    );
}

#[test]
fn test_bad_host_patterns_are_refused() {
    // E-34, E-35.
    for bad in [
        "a*.example.com",
        "*.*.example.com",
        "*example.com",
        "ex ample.com",
        "host:80",
    ] {
        let err = ProxyBuilder::new()
            .pool("p", Pool::new(["10.0.0.1:80"]))
            .route(Route::host(bad).to("p"))
            .config()
            .expect_err(bad);
        assert_eq!(err.reason(), &ConfigReason::HostPattern, "{bad}");
    }
}

#[test]
fn test_path_cases() {
    let c = config(vec![
        Route::any().exact("/health"),
        Route::any().path("/api"),
        Route::any().path("/static/"),
        Route::any().path("/").methods(&["GET"]),
    ]);
    let t = c.routes();
    let cases = [
        ("/", Some(3)),            // E-40
        ("/api", Some(1)),         // E-41
        ("/api/", Some(1)),        // E-41
        ("/api/x", Some(1)),       // E-41
        ("/apix", Some(3)),        // E-41
        ("/api?x=1", Some(1)),     // E-41, E-48
        ("/static", Some(3)),      // E-42
        ("/static/a", Some(2)),    // E-42
        ("/health", Some(0)),      // E-43
        ("/health/", Some(3)),     // E-43
        ("/%61pi", Some(3)),       // E-44
        ("/health?full", Some(0)), // E-48
    ];
    for (path, want) in cases {
        assert_eq!(found(t, "h", path, "GET"), want, "{path:?}");
    }
    // E-47: the last route only takes GET.
    assert_eq!(t.find(b"h", b"/other", b"POST"), RouteMatch::NoRoute);
}

#[test]
fn test_shadowed_routes_are_refused() {
    // E-45, E-46.
    let shadowed = |routes: Vec<Route>| {
        routes
            .into_iter()
            .fold(
                ProxyBuilder::new().pool("p", Pool::new(["10.0.0.1:80"])),
                |b, r| b.route(r.to("p")),
            )
            .config()
            .err()
            .map(|e| e.reason().clone())
    };
    assert_eq!(
        shadowed(vec![Route::any().exact("/a"), Route::any().exact("/a")]),
        Some(ConfigReason::Shadowed { by: 0 })
    );
    assert_eq!(
        shadowed(vec![Route::any().path("/a"), Route::any().path("/a/b")]),
        Some(ConfigReason::Shadowed { by: 0 })
    );
    assert_eq!(
        shadowed(vec![
            Route::host("x.com"),
            Route::host("y.com"),
            Route::host("x.com").path("/a")
        ]),
        Some(ConfigReason::Shadowed { by: 0 })
    );
    assert_eq!(
        shadowed(vec![Route::any().path("/a/b"), Route::any().path("/a")]),
        None
    );
}

#[test]
fn test_unknown_host_answer() {
    let c = ProxyBuilder::new()
        .pool("p", Pool::new(["10.0.0.1:80"]))
        .route(Route::host("a.com").to("p"))
        .unknown_host(UnknownHost::Misdirected)
        .config()
        .expect("config");
    assert_eq!(
        c.routes().find(b"b.com", b"/", b"GET"),
        RouteMatch::UnknownHost
    );
    assert_eq!(c.unknown_host().status(), 421);
}

/// A matcher written the obvious way, on owned lowercase strings.
fn reference(routes: &[(&str, &str, bool)], host: &str, path: &str) -> Option<usize> {
    let mut host = host.to_ascii_lowercase();
    if let Some(i) = host.rfind(':') {
        if host[i + 1..].chars().all(|c| c.is_ascii_digit()) && !host.starts_with('[') {
            host.truncate(i);
        }
    }
    if host.ends_with('.') {
        host.pop();
    }
    let path = path.split('?').next().unwrap_or("");
    routes.iter().position(|(h, p, exact)| {
        let host_ok = match *h {
            "*" => true,
            h if h.starts_with("*.") => {
                let suffix = &h[1..];
                host.len() > suffix.len()
                    && host.ends_with(suffix)
                    && !host[..host.len() - suffix.len()].ends_with('.')
            }
            h => host == h,
        };
        let path_ok = if *exact {
            path == *p
        } else if *p == "/" {
            true
        } else if p.ends_with('/') {
            path.starts_with(p)
        } else {
            path == *p || path.starts_with(&format!("{p}/"))
        };
        host_ok && path_ok
    })
}

#[test]
fn test_agrees_with_reference_on_generated_requests() {
    let routes: &[(&str, &str, bool)] = &[
        ("api.example.com", "/v1", false),
        ("api.example.com", "/v1/", false),
        ("*.example.com", "/admin/", false),
        ("example.com", "/health", true),
        ("*.example.com", "/", false),
        ("*", "/static", false),
    ];
    // "/v1/" after "/v1" is shadowed, so build the table without it and drop
    // it from the reference too.
    let routes: Vec<_> = routes.iter().copied().filter(|r| r.1 != "/v1/").collect();
    let c = config(
        routes
            .iter()
            .map(|(h, p, exact)| {
                let r = Route::host(h);
                if *exact { r.exact(p) } else { r.path(p) }
            })
            .collect(),
    );
    let hosts = [
        "api.example.com",
        "API.example.com:443",
        "x.example.com",
        "a.b.example.com.",
        "example.com",
        "other.org",
        "",
        "api.example.com.",
        ".example.com",
    ];
    let paths = [
        "/",
        "/v1",
        "/v1/",
        "/v1/x",
        "/v1x",
        "/admin",
        "/admin/",
        "/admin/a",
        "/health",
        "/health/",
        "/static",
        "/static/x",
        "/staticx",
        "/v1?q",
        "/%76%31",
    ];
    let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    for _ in 0..20_000 {
        let host = hosts[(next() % hosts.len() as u64) as usize];
        let path = paths[(next() % paths.len() as u64) as usize];
        assert_eq!(
            found(c.routes(), host, path, "GET"),
            reference(&routes, host, path),
            "{host:?} {path:?}"
        );
    }
}
