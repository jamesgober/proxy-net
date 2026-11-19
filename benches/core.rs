//! Timings of the sans-IO core's hot paths for `docs/PERFORMANCE.md`:
//! route matching, an endpoint pick, and rewriting a request head.
//!
//! ```text
//! cargo bench --bench core
//! ```
//!
//! Each line is the median of 15 runs of a timed loop, in nanoseconds per
//! operation.

use std::hint::black_box;
use std::time::Instant;

use proxy_net::core::{Balancer, FieldList, ForwardFacts, HealthTable, RouteMatch, request_fields};
use proxy_net::{Balance, Pool, ProxyBuilder, Route};

fn median_ns(iters: u32, mut f: impl FnMut()) -> f64 {
    let mut runs: Vec<f64> = (0..15)
        .map(|_| {
            let start = Instant::now();
            for _ in 0..iters {
                f();
            }
            start.elapsed().as_nanos() as f64 / f64::from(iters)
        })
        .collect();
    runs.sort_by(f64::total_cmp);
    runs[runs.len() / 2]
}

fn main() {
    // 32 routes: 8 hosts with 4 paths each; the request matches the last.
    let mut builder = ProxyBuilder::new().pool("p", Pool::new(["10.0.0.1:80", "10.0.0.2:80"]));
    for h in 0..8 {
        for (i, path) in ["/api", "/static", "/v1/users", "/"].iter().enumerate() {
            builder = builder.route(
                Route::host(&format!("h{h}.example.com"))
                    .path(path)
                    .methods(if i == 0 { &["GET", "POST"] } else { &["GET"] })
                    .to("p"),
            );
        }
    }
    let config = builder.config().expect("config");
    let routes = config.routes();
    let found = matches!(
        routes.find(b"h7.example.com", b"/v1/users/42", b"GET"),
        RouteMatch::Route { .. }
    );
    assert!(found);
    let route_ns = median_ns(1_000_000, || {
        let m = routes.find(
            black_box(b"h7.example.com"),
            black_box(b"/v1/users/42"),
            black_box(b"GET"),
        );
        let _ = black_box(m);
    });
    println!("route match, 32 routes: {route_ns:.1} ns");

    for (name, balance) in [
        ("round robin", Balance::RoundRobin),
        ("least connections", Balance::LeastConnections),
        ("power of two choices", Balance::PowerOfTwo),
    ] {
        let pool = ProxyBuilder::new()
            .pool(
                "p",
                Pool::new(["10.0.0.1:80", "10.0.0.2:80", "10.0.0.3:80", "10.0.0.4:80"])
                    .balance(balance),
            )
            .config()
            .expect("config");
        let p = pool.pool("p").expect("pool");
        let balancer = Balancer::new(p);
        let _ = balancer.refresh(&HealthTable::new(p));
        assert!(balancer.pick(None, &[]).is_some());
        let ns = median_ns(1_000_000, || {
            let _ = black_box(balancer.pick(None, &[]));
        });
        println!("pick, {name}, 4 endpoints: {ns:.1} ns");
    }

    let route = routes.get(0).expect("route");
    let endpoint = &config.pool("p").expect("pool").endpoints()[0];
    let fields: Vec<(&str, &[u8])> = vec![
        ("Host", b"h0.example.com"),
        ("User-Agent", b"bench/1.0"),
        ("Accept", b"application/json"),
        ("Accept-Encoding", b"gzip, br"),
        ("Cookie", b"session=0123456789abcdef"),
        ("X-Forwarded-For", b"203.0.113.9"),
    ];
    let client = Some("198.51.100.7:50000".parse().expect("addr"));
    let (mut out, mut target) = (FieldList::new(), Vec::new());
    let ns = median_ns(200_000, || {
        let facts = ForwardFacts::new(b"GET", b"/api/items?page=2", b"h0.example.com", endpoint)
            .client(client);
        request_fields(&fields, &facts, route, &config, &mut out, &mut target).expect("fields");
        let _ = black_box(out.len());
    });
    println!("rewrite a request head, 6 fields: {ns:.1} ns");
}
