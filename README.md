<h1 align="center">
    <img width="99" alt="Rust logo" src="https://raw.githubusercontent.com/jamesgober/rust-collection/72baabd71f00e14aa9184efcb16fa3deddda3a0a/assets/rust-logo.svg">
    <br>
    <b>proxy-net</b>
    <br>
    <sub>
        <sup>REVERSE PROXY AND LOAD BALANCER</sup>
    </sub>
</h1>

<p align="center">
    <a href="https://crates.io/crates/proxy-net"><img src="https://img.shields.io/crates/v/proxy-net.svg" alt="Crates.io"></a>
    <a href="https://crates.io/crates/proxy-net" alt="Download"><img alt="Crates.io Downloads" src="https://img.shields.io/crates/d/proxy-net?color=%230099ff"></a>
    <a href="https://docs.rs/proxy-net"><img src="https://docs.rs/proxy-net/badge.svg" alt="Documentation"></a>
    <a href="https://github.com/jamesgober/proxy-net/actions"><img alt="GitHub CI" src="https://github.com/jamesgober/proxy-net/actions/workflows/ci.yml/badge.svg"></a>
    <a href="https://github.com/rust-lang/rfcs/blob/master/text/2495-min-rust-version.md" title="MSRV"><img alt="MSRV" src="https://img.shields.io/badge/MSRV-1.85-blue"></a>
    <a href="#license"><img src="https://img.shields.io/badge/license-Apache--2.0%20OR%20MIT-blue.svg" alt="License"></a>
</p>

<p align="center">
    <b>A reverse proxy and load balancer for services on serve-net.</b>
    <br>
    <i>Every request and response is parsed and written again, never passed through as raw bytes.</i>
</p>

<br>

<p>
    <strong>proxy-net</strong> puts a service in front of other servers. It takes requests that serve-net has already admitted, with the connection gate and every inbound limit in place, picks an upstream endpoint from a pool, and forwards the request there and the response back. Pools have weights, priority tiers, active and passive health checks, and a choice of balancing policies.
</p>

<p>
    A proxy is where request smuggling happens: two programs read the same bytes and disagree about where a message ends. proxy-net parses every request and response strictly on arrival and writes it again on departure, so an upstream only ever sees bytes the proxy wrote. Client-sent forwarding headers are always removed and rebuilt from the real client address, and paths that a proxy and an origin could read differently are refused by default.
</p>

<p>
    The decisions (routing, balancing, health, header rewriting, retries) live in a sans-IO core that reads no clock and opens no socket, so the same core runs under serve-net, inside another engine, or in a test. It uses <code>#![forbid(unsafe_code)]</code>, builds without <code>std</code> (it needs only <code>alloc</code> and 64-bit atomics), and has no third-party runtime dependencies: everything it uses is first-party (<code>parse-net</code>, <code>http-net</code>, <code>net-io</code>, <code>serve-net</code>).
</p>

---

## Features

### **Routing**
- **Host patterns** - exact names, `*.example.com`, addresses, or every host; case, ports, and a trailing dot are ignored.
- **Path patterns** - a segment prefix (`/api` matches `/api/users`, never `/apix`), a prefix, or an exact path, matched as received and never decoded.
- **Checked at build** - a route that an earlier route shadows entirely is refused, since it could never match.
- **Rewrites** - prefix strip and replace, `Host` handling, request and response header rules.

### **Pools and balancing**
- **Endpoints** - by address or host name, with weights, priority tiers for failover, and connection caps.
- **Policies** - smooth weighted round robin, least connections, power of two choices, Maglev consistent hashing with a load bound, random, and first healthy.
- **Health** - active checks (HTTP or TCP, with jitter, rise and fall counts) and passive outlier detection with a panic threshold.
- **Slow start** - an endpoint that comes back, or that a live change adds, takes a growing share of traffic.

### **Forwarding**
- **Re-serialized** - requests and responses parsed strictly and written again.
- **Streamed** - bodies relayed with bounded buffers and backpressure both ways.
- **Retries** - only for idempotent requests whose body is held in full, before any response byte reached the client, under a retry budget.
- **Upstream protocols** - TLS with verified certificates and client certificates, the PROXY protocol (v1 and v2), and WebSocket bridged message by message.
- **Live changes** - reload the whole configuration, replace a pool's endpoints, or drain one endpoint, without dropping a request; discovery (static or DNS) behind an address allowlist.

### **Safe defaults**
- **Forwarding headers rebuilt** - `Forwarded`, `X-Forwarded-*`, `X-Real-IP` and similar fields from clients are removed; the proxy writes its own from the real client.
- **Ambiguous paths refused** - `..`, `%2e`, `%2f`, `\`, and `..;/` forms answer 400 unless a route allows them.
- **No body on `GET`** - a body on `GET`, `HEAD`, `OPTIONS`, `TRACE`, or `DELETE` is refused unless a route allows it.
- **Loops detected** - `Via` carries the proxy's pseudonym, and a request that comes back is refused.
- **WebSocket only after a real handshake** - the client gets its 101 only after the upstream's genuine 101 with the right accept key.

### **Reliability and safety**
- **No unsafe code** - `#![forbid(unsafe_code)]`.
- **Errors that say what to fix** - every refusal names the pool, endpoint, route, or setting.
- **Tested** - unit tests for every path, data tables, a reference matcher, and fuzz targets for every parser.

---

## **Why Choose proxy-net?**

### **Nothing passes through unread**
An upstream never receives a client's bytes, only a request the proxy built after strict parsing. That removes the whole class of attacks that depend on two hops framing one message differently.

### **Defaults that assume a hostile client**
The settings other proxies leave to the operator are on by default here: forwarding headers rebuilt, ambiguous paths refused, non-idempotent requests never retried or sent on a connection that may have gone stale.

### **A library, not a product**
It is a crate you embed: a serve-net service you mount next to your own handlers, configured in Rust and checked when it is built.

### **Developer Experience First**
One line for the common case, a builder for the rest, and errors that name the item and what to change.

### **Ready for many nodes**
Endpoints can be remote. Consistent hashing gives the same answer on every node with the same configuration, health is judged by each node on its own, and no clock is compared across machines.

---

<br>

## Installation

Add to your `Cargo.toml`:

```toml
[dependencies]
proxy-net = "0.7"
```

Without the standard library (the configuration types and the core only):

```toml
[dependencies]
proxy-net = { version = "0.7", default-features = false }
```

| Feature | Enables | Default |
|---|---|---|
| `serve` | The serve-net service, `Proxy::serve`, and `proxy_net::proxy`. Implies `client`. | yes |
| `client` | The upstream side over net-io and http-net: connections, the connection pool, the forwarder, the health checker. Implies `std`. | yes |
| `std` | Integration with the standard library. Without it the crate needs only `alloc` and 64-bit atomics. | yes |
| `tls` | TLS to upstreams through tls-net (aws-lc-rs underneath). Implies `client`. | no |
| `ws` | The WebSocket bridge through ws-net. Implies `serve`. | no |

**Feature Recommendations**:
- **A proxy in front of servers**: the defaults.
- **Another inbound engine driving the forwarder**: `default-features = false, features = ["client"]`.
- **Upstreams across a network you do not trust**: add `tls`.
- **WebSocket applications**: add `ws`, and `Route::websocket(true)` on their routes.
- **Embedding the core on `no_std`**: `default-features = false`.
- **Behind a load balancer**: start with `Proxy::serve_with` and raise `Inbound::max_per_source` to what each balancer address carries; the default of 16 connections per address is a defence for an open listener.

---

<br>

## Quick Start

One line forwards a listener to a set of servers:

```rust,no_run
let running = proxy_net::proxy("0.0.0.0:8080", ["10.0.0.1:80", "10.0.0.2:80"])?;
running.wait();
# Ok::<(), proxy_net::ProxyError>(())
```

### **Pools, routes, and health checks**

```rust,no_run
use std::time::Duration;
use proxy_net::{Balance, HealthCheck, Pool, ProxyBuilder, Route};

let running = ProxyBuilder::new()
    .pool("api", Pool::new(["10.0.0.1:8080", "10.0.0.2:8080"])
        .balance(Balance::LeastConnections)
        .health(HealthCheck::http("/health").interval(Duration::from_secs(2))))
    .pool("web", Pool::new(["10.0.1.1:8080"]))
    .route(Route::host("api.example.com").path("/v1").to("api").strip_prefix("/v1"))
    .route(Route::any().to("web"))
    .build()?
    .serve("0.0.0.0:8080")?;
for pool in running.handle().health().pools {
    println!("{}: {} endpoints", pool.name, pool.endpoints.len());
}
running.wait();
# Ok::<(), proxy_net::ProxyError>(())
```

### **Checking a configuration**

A configuration is built and checked without starting anything, and its route table answers which pool a request goes to:

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

### **One pool**

```rust
// One pool and no routes: every request goes to that pool.
let config = proxy_net::ProxyBuilder::new()
    .pool("app", proxy_net::Pool::new(["127.0.0.1:3000"]))
    .config()?;
assert_eq!(config.routes().len(), 1);
# Ok::<(), proxy_net::ConfigError>(())
```

### **Sticky sessions and failover**

```rust
use proxy_net::{Balance, Endpoint, HashKey, Pool};

let pool = Pool::new(["10.0.0.1:8080", "10.0.0.2:8080"])
    .endpoint(Endpoint::parse("10.0.9.1:8080")?.tier(1))   // used only when tier 0 is down
    .balance(Balance::consistent_hash(HashKey::Cookie("session".into())));
# let _ = pool;
# Ok::<(), proxy_net::ConfigError>(())
```

### **Errors name what to fix**

```rust
let err = proxy_net::ProxyBuilder::new()
    .pool("db", proxy_net::Pool::new(["10.0.0.1"]))
    .config()
    .unwrap_err();
assert_eq!(err.to_string(), "pool \"db\" endpoint 0: the port is missing (write host:port)");
```

---

## **Status: Feature complete**

**Current Version:** `0.7.0`. The public API is written in [`docs/API.md`](./docs/API.md), with the version each part ships in. Every feature planned for 1.0 is in: forwarding, TLS and the PROXY protocol to upstreams, the WebSocket bridge (`wss` included), live reload, draining, discovery, slow start, and request ids. The request path is measured against nginx, HAProxy, and Envoy in [`docs/PERFORMANCE.md`](./docs/PERFORMANCE.md). Every threat in the threat model has an attack test, listed in [`docs/SECURITY.md`](./docs/SECURITY.md) with the 24-hour fuzz and soak results. What remains is time in service.

**Target:** `1.0.0`, after the roadmap in [`dev/ROADMAP.md`](./dev/ROADMAP.md).

**What's complete**:
- **Endpoints** - parsing, weights, tiers, caps, server names.
- **Routes** - host, path, and method patterns, the compiled route table, rewrites, header rules, path policies.
- **Pools** - balancing, health checks, passive health, retries, timeouts, and connection limits as checked settings.
- **Proxy settings** - limits, forwarding headers, `Via`, and the errors for all of them.
- **Balancing** - round robin, least connections, power of two choices, Maglev consistent hashing with a load bound, random, first healthy, with tiers and a panic threshold.
- **Health** - passive ejection with growing back-off and a cap on how much of a pool is ejected, active check results, draining.
- **Forwarding decisions** - request checks, head rewriting with forwarding headers and `Via`, retries within a budget, and the exchange state machine with every timeout.
- **Forwarding** - the serve-net service, request bodies held when small and streamed when large, responses streamed with backpressure, pooled upstream connections checked before reuse, the active health checker, an observer, and health and counter snapshots.
- **Upstream protocols** - TLS, the PROXY protocol, and the WebSocket bridge, `wss` included.
- **Performance** - buffers kept per worker (10 allocations per proxied request), a connection pool with no system call under its lock, a host index for routes, and benchmarks against other proxies.
- **Live changes** - reload, endpoint replacement, draining, discovery with an allowlist, slow start.
- **Request ids and append mode** - a request id field kept or written, and forwarding chains from trusted proxies kept.
- **Hardening** - an attack test for every threat, chains and failover across several proxies, the upstream bytes checked against a second parser, 24-hour fuzzing and soaks, and the memory an exchange holds.

**What's planned**:
- `0.8.0`: a gateway deployment using proxy-net end to end, and the operations and platform guides.

**Performance targets** for 1.0.0, measured in [`docs/PERFORMANCE.md`](./docs/PERFORMANCE.md):
- **Route match** under 50 ns for 32 routes, with no allocation: 39 ns, none.
- **Added latency** under 30 µs at p50 for a small request over a kept-alive connection: 25 µs on a desktop, 42 µs on a 4-CPU Linux runner (nginx adds 39 µs there).
- **Memory** under 20 KiB per active exchange: 15.3 KiB with 4 KiB client reads (`Inbound::read_buffer`), 27.3 KiB with the default 16 KiB.
- **Throughput** under concurrency is about two thirds of nginx's on a 4-CPU runner, held back by one blocking thread per connection until the reactor path lands.

**Note on API stability**: the API in `docs/API.md` is locked from 0.2. Names and signatures change only with a migration note. The `1.0.0` release will commit to the stability contract in `docs/STABILITY-1.0.md`.

---

## **Documentation & Resources**

### **Documentation**
- **[Documentation Index](docs/README.md)** - the documentation hub
- **[API Reference](docs/API.md)** - every public item, with examples and the version it ships in
- **[Release Notes](docs/release/)** - one file per release
- **[Changelog](CHANGELOG.md)** - what changed in each version
- **[Plan](dev/PLAN.md)** - the design, the threats, and the decision log
- **[Roadmap](dev/ROADMAP.md)** - what each version delivers

### **External Resources**
- **[API Documentation](https://docs.rs/proxy-net)** - rustdoc on docs.rs
- **[Crate Registry](https://crates.io/crates/proxy-net)** - crates.io
- **[serve-net](https://github.com/jamesgober/serve-net)** - the server engine proxy-net runs on
- **[http-net](https://github.com/jamesgober/http-net)** - the HTTP/1.1 engine that parses and writes every message

### **Troubleshooting forwarding**

<details>
  <summary><b>A mounted <code>ProxyService</code> stalls other connections?</b></summary>
    <p>The service waits on the upstream inside serve-net's call, so it needs blocking workers (<code>Workers::blocking</code>) until serve-net can park a streamed reply. <code>Proxy::serve</code> and <code>proxy_net::proxy</code> set them for you.</p>
</details>

<details>
  <summary><b>Uploads over 1 MiB are refused in my own serve-net server?</b></summary>
    <p>serve-net's default body limit is 1 MiB. Raise it to at least <code>Limits::max_request_body</code> (<code>serve_net::Limits::default().max_body(..)</code>), as <code>Proxy::serve</code> does.</p>
</details>

### **Common Use Cases**
- **One binary in front of several servers**: one pool, round robin, passive health
- **Splitting traffic by host or path**: several pools and routes
- **Sticky sessions**: consistent hashing on a cookie or the client address
- **Active and standby**: priority tiers, or `Balance::First`

---

### **Troubleshooting**

<details>
  <summary><b>"route 1: route 0 already matches every request this route would"?</b></summary>
    <p>Routes are tried in order and the first match wins, so a broad route before a narrow one hides it. Put the narrower route first, for example <code>Route::any().path("/api")</code> before <code>Route::any()</code>.</p>
</details>

<details>
  <summary><b>"add a pool, and a route for each pool when there are several"?</b></summary>
    <p>One pool needs no routes: every request goes there. With two or more pools, add routes so each request has a pool.</p>
</details>

<details>
  <summary><b>A header rule is refused as reserved?</b></summary>
    <p>The proxy writes framing, hop-by-hop, <code>Host</code>, <code>Via</code>, and the forwarding fields itself. Use <code>Route::host_header</code> for <code>Host</code>, <code>ProxyBuilder::via</code> for <code>Via</code>, and <code>ProxyBuilder::forwarded</code> for the forwarding fields.</p>
</details>

<details>
  <summary><b>An endpoint like <code>::1:80</code> or <code>host:080</code> does not parse?</b></summary>
    <p>Write IPv6 addresses in brackets (<code>[::1]:80</code>) and ports without a leading zero. Every accepted endpoint has exactly one reading.</p>
</details>

<details>
  <summary><b>My application needs <code>%2f</code> or <code>..</code> in paths?</b></summary>
    <p>Allow that one check on that route: <code>Route::path_policy(PathPolicy::STRICT.allow(PathCheck::EncodedSlash))</code>. The rest stay refused.</p>
</details>

---

### **Version Compatibility**
- **Rust**: 1.85+ (edition 2024), checked in CI on stable and 1.85
- **Edition**: 2024
- **`no_std`**: the configuration types and the core need only `alloc` and 64-bit atomics; CI builds the crate for `aarch64-unknown-none`
- **API Stability**: pre-1.0. The API is locked from 0.2; changes come with a migration note
- **Dependencies**: first-party only: `parse-net`, and with the default features `http-net`, `net-io`, and `serve-net`; `tls` adds `tls-net` (which uses the audited aws-lc-rs) and `ws` adds `ws-net`
- **Feature Flags**: `serve`, `client`, `std` (all default); `tls`, `ws`

### **Development Setup**
```bash
git clone https://github.com/jamesgober/proxy-net.git
cd proxy-net
cargo test --all-features                                 # unit, integration and doc tests
cargo clippy --all-targets --all-features -- -D warnings  # lints (should be clean)
cargo +nightly fuzz run route_match                       # one of the fuzz targets, from fuzz/
```

## **Contributing**

See [`dev/DIRECTIVES.md`](./dev/DIRECTIVES.md) for the engineering standards and the definition of done. Before a PR, `cargo fmt --all`, `cargo clippy --all-targets --all-features -- -D warnings` and `cargo test --all-features` must be clean.

<hr>

<!-- LICENSE
############################################# -->
<div id="license">
    <h2>License</h2>
    <p><b>proxy-net</b> is dual-licensed under either of:</p>
    <ul>
        <li><b>Apache License, Version 2.0</b> - see <a href="./LICENSE-APACHE" title="Apache-2.0 License">LICENSE-APACHE</a> or <a href="http://www.apache.org/licenses/LICENSE-2.0" title="Apache-2.0 License" target="_blank">apache.org/licenses/LICENSE-2.0</a></li>
        <li><b>MIT License</b> - see <a href="./LICENSE-MIT" title="MIT License">LICENSE-MIT</a> or <a href="https://opensource.org/licenses/MIT" title="MIT License" target="_blank">opensource.org/licenses/MIT</a></li>
    </ul>
    <p>at your option.</p>
    <h3>Contribution</h3>
    <p>Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in <b>proxy-net</b> by you, as defined in the Apache-2.0 license, shall be dual-licensed as above, without any additional terms or conditions.</p>
    <p>Unless required by applicable law or agreed to in writing, software distributed under the Licenses is distributed on an <b>"AS IS" BASIS, WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND</b>, either express or implied.</p>
</div>


<!-- FOOT COPYRIGHT
################################################# -->
<div align="center">
  <h2></h2>
  <sup>COPYRIGHT <small>&copy;</small> 2026 <strong>JAMES GOBER.</strong></sup>
</div>
