//! Arbitrary host patterns, path patterns, methods, and rewrites through the
//! configuration builder. Oracle: building never panics; an accepted route
//! matches the request made of its own literal host and path.

#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use proxy_net::core::RouteMatch;
use proxy_net::{Pool, ProxyBuilder, Route};

#[derive(Arbitrary, Debug)]
struct Input {
    host: String,
    path: String,
    exact: bool,
    methods: Option<Vec<String>>,
    strip: Option<String>,
}

fuzz_target!(|input: Input| {
    let mut route = Route::host(&input.host);
    route = if input.exact { route.exact(&input.path) } else { route.path(&input.path) };
    if let Some(methods) = &input.methods {
        let list: Vec<&str> = methods.iter().map(String::as_str).collect();
        route = route.methods(&list);
    }
    if let Some(prefix) = &input.strip {
        route = route.strip_prefix(prefix);
    }
    let Ok(config) = ProxyBuilder::new()
        .pool("p", Pool::new(["10.0.0.1:80"]))
        .route(route.to("p"))
        .config()
    else {
        return;
    };
    // A literal host (no wildcard) and the pattern's own path must match.
    if input.host.starts_with('*') {
        return;
    }
    let method = input
        .methods
        .as_ref()
        .and_then(|m| m.first().cloned())
        .unwrap_or_else(|| "GET".into());
    let found = config
        .routes()
        .find(input.host.as_bytes(), input.path.as_bytes(), method.as_bytes());
    assert!(
        matches!(found, RouteMatch::Route { index: 0, .. }),
        "{input:?} -> {found:?}"
    );
});
