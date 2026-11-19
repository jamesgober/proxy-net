<h1 align="center">
    <img width="99" alt="Rust logo" src="https://raw.githubusercontent.com/jamesgober/rust-collection/72baabd71f00e14aa9184efcb16fa3deddda3a0a/assets/rust-logo.svg">
    <br>
    <b>proxy-net</b>
    <br>
    <sub><sup>REVERSE PROXY</sup></sub>
</h1>

<div align="center">
    <a href="https://crates.io/crates/proxy-net"><img alt="Crates.io" src="https://img.shields.io/crates/v/proxy-net"></a>
    <a href="https://crates.io/crates/proxy-net"><img alt="Downloads" src="https://img.shields.io/crates/d/proxy-net?color=%230099ff"></a>
    <a href="https://docs.rs/proxy-net"><img alt="docs.rs" src="https://img.shields.io/docsrs/proxy-net"></a>
    <a href="https://github.com/jamesgober/proxy-net/actions"><img alt="CI" src="https://github.com/jamesgober/proxy-net/actions/workflows/ci.yml/badge.svg"></a>
    <a href="https://github.com/rust-lang/rfcs/blob/master/text/2495-min-rust-version.md"><img alt="MSRV" src="https://img.shields.io/badge/MSRV-1.85%2B-blue"></a>
</div>

<br>

<div align="left">
    <p>
        <strong>proxy-net</strong> puts a service in front of other servers. It takes requests that serve-net has already admitted, with the connection gate and every limit in place, picks an upstream endpoint from a pool, and forwards the request there and the response back.
    </p>
    <p>
        Every request and response is parsed strictly on arrival and written again on departure, so an upstream only ever sees bytes the proxy wrote. Forwarding headers sent by clients are always removed and rebuilt from the real client address.
    </p>
    <br>
    <hr>
    <p>
        <strong>MSRV is 1.85+</strong> (Rust 2024 edition). <code>#![forbid(unsafe_code)]</code>. No third-party runtime dependencies.
    </p>
    <blockquote>
        <strong>Status: <code>0.2.0</code>, design lock.</strong> The configuration surface is built and checked: endpoints, routes and the compiled route table, pools and their policies, header rules, path safety, forwarding headers, and limits. Nothing forwards traffic yet. The sans-IO core arrives in <code>0.3.0</code> and forwarding over real sockets in <code>0.4.0</code>. The full API, with the version each part ships in, is in <a href="./docs/API.md"><code>docs/API.md</code></a>.
    </blockquote>
</div>

<hr>
<br>

## Installation

```toml
[dependencies]
proxy-net = "0.2"
```

## Quick start

From `0.4.0`, one line forwards a listener to a set of servers:

```rust,ignore
let running = proxy_net::proxy("0.0.0.0:8080", ["10.0.0.1:80", "10.0.0.2:80"])?;
running.wait();
```

In `0.2.0`, the configuration is built and checked, and the route table answers which pool a request goes to:

```rust
use proxy_net::core::RouteMatch;
use proxy_net::{Balance, HealthCheck, Pool, ProxyBuilder, Route};

let config = ProxyBuilder::new()
    .pool("api", Pool::new(["10.0.0.1:8080", "10.0.0.2:8080"]).balance(Balance::LeastConnections))
    .pool("web", Pool::new(["10.0.1.1:8080"]).health(HealthCheck::http("/health")))
    .route(Route::host("api.example.com").path("/v1").to("api").strip_prefix("/v1"))
    .route(Route::any().to("web"))
    .config()?;

if let RouteMatch::Route { route, .. } = config.routes().find(b"api.example.com", b"/v1/users", b"GET") {
    assert_eq!(config.pool_at(route.pool_index()).map(|p| p.name()), Some("api"));
}
# Ok::<(), proxy_net::ConfigError>(())
```

A mistake is an error that names the pool, endpoint, or route and what to change:

```text
pool "db" endpoint 0: the port is missing (write host:port)
route 2: route 0 already matches every request this route would
```

## Features

- **Upstream pools**: endpoints by address or host name, weights, priority tiers for failover, per-endpoint connection caps.
- **Balancing**: smooth weighted round robin, least connections, power of two choices, Maglev consistent hashing with bounded load, random, first healthy.
- **Health**: active checks (HTTP or TCP, intervals with jitter, rise and fall counts) and passive outlier detection with a panic threshold.
- **Forwarding** (0.4): re-serialized requests and responses, streamed bodies with backpressure, connection reuse checked before every use, timeouts, retries only for idempotent requests with a retry budget.
- **Safe defaults**: client-sent forwarding headers removed and rebuilt, `Via` with loop detection, ambiguous paths (`..`, `%2f`, `..;/`) refused, bodies on `GET` refused, non-idempotent requests never sent on a connection that may have gone stale.
- **Later**: WebSocket bridging, TLS and the PROXY protocol to upstreams, live reload, service discovery (0.5).

<hr>
<br>

## Documentation

- [`docs/API.md`](./docs/API.md): every public item, with examples and the version it ships in.
- [`CHANGELOG.md`](./CHANGELOG.md) and [`docs/release/`](./docs/release/): what changed in each version.
- [`dev/PLAN.md`](./dev/PLAN.md) and [`dev/ROADMAP.md`](./dev/ROADMAP.md): the design, the decision log, and the schedule.

<hr>
<br>

## Contributing

See <a href="./dev/DIRECTIVES.md"><code>dev/DIRECTIVES.md</code></a> for engineering standards and the definition of done. Before a PR: `cargo fmt --all`, `cargo clippy --all-targets --all-features -- -D warnings`, and `cargo test --all-features` must be clean.

<br>

<div id="license">
    <h2>License</h2>
    <p>Licensed under either of</p>
    <ul>
        <li><b>Apache License, Version 2.0</b> - <a href="./LICENSE-APACHE">LICENSE-APACHE</a></li>
        <li><b>MIT License</b> - <a href="./LICENSE-MIT">LICENSE-MIT</a></li>
    </ul>
    <p>at your option.</p>
</div>

<div align="center">
  <h2></h2>
  <sup>COPYRIGHT <small>&copy;</small> 2026 <strong>James Gober.</strong></sup>
</div>
