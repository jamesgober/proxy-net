//! A fixed route table against arbitrary hosts, paths, and methods. Oracle: no
//! panic, and the same answer as a matcher written the obvious way on owned
//! lowercase strings.

#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use proxy_net::core::RouteMatch;
use proxy_net::{Config, Pool, ProxyBuilder, Route};
use std::sync::OnceLock;

const ROUTES: &[(&str, &str, bool, Option<&str>)] = &[
    ("api.example.com", "/v1", false, None),
    ("api.example.com", "/v2/", false, Some("GET")),
    ("*.example.com", "/admin/", false, None),
    ("example.com", "/health", true, None),
    ("*.example.com", "/", false, None),
    ("*", "/static", false, Some("GET")),
    ("*", "/files/", false, None),
];

fn table() -> &'static Config {
    static CONFIG: OnceLock<Config> = OnceLock::new();
    CONFIG.get_or_init(|| {
        ROUTES
            .iter()
            .fold(ProxyBuilder::new().pool("p", Pool::new(["10.0.0.1:80"])), |b, (h, p, exact, m)| {
                let mut r = Route::host(h);
                r = if *exact { r.exact(p) } else { r.path(p) };
                if let Some(m) = m {
                    r = r.methods(&[m]);
                }
                b.route(r.to("p"))
            })
            .config()
            .expect("the fixed table builds")
    })
}

fn reference(host: &[u8], path: &[u8], method: &[u8]) -> Option<usize> {
    // Host: lowercase, the port removed (not inside brackets), one trailing dot removed.
    let mut host: Vec<u8> = host.to_ascii_lowercase();
    if !host.starts_with(b"[") {
        if let Some(i) = host.iter().rposition(|&b| b == b':') {
            if host[i + 1..].iter().all(u8::is_ascii_digit) {
                host.truncate(i);
            }
        }
    }
    if host.ends_with(b".") {
        let _ = host.pop();
    }
    let path = match path.iter().position(|&b| b == b'?') {
        Some(q) => &path[..q],
        None => path,
    };
    ROUTES.iter().position(|(h, p, exact, m)| {
        let (h, p) = (h.as_bytes(), p.as_bytes());
        let host_ok = if h == b"*" {
            true
        } else if let Some(suffix) = h.strip_prefix(b"*") {
            host.len() > suffix.len()
                && host.ends_with(suffix)
                && !host[..host.len() - suffix.len()].ends_with(b".")
        } else {
            host == h
        };
        let path_ok = if *exact {
            path == p
        } else if p == b"/" {
            true
        } else if p.ends_with(b"/") {
            path.starts_with(p)
        } else {
            path == p || (path.starts_with(p) && path.get(p.len()) == Some(&b'/'))
        };
        let method_ok = m.is_none_or(|m| m.as_bytes() == method);
        host_ok && path_ok && method_ok
    })
}

#[derive(Arbitrary, Debug)]
struct Input<'a> {
    host: &'a [u8],
    path: &'a [u8],
    method: &'a [u8],
}

fuzz_target!(|input: Input<'_>| {
    // IP literals take another path in both matchers; the fixed table has
    // none, so only names are compared.
    if input.host.contains(&b'[') || input.host.iter().all(|b| b.is_ascii_digit() || *b == b'.' || *b == b':') {
        let _ = table().routes().find(input.host, input.path, input.method);
        return;
    }
    let got = match table().routes().find(input.host, input.path, input.method) {
        RouteMatch::Route { index, .. } => Some(index),
        _ => None,
    };
    assert_eq!(got, reference(input.host, input.path, input.method), "{input:?}");
});
