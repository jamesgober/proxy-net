<h1 align="center">
        <img width="99" alt="Rust logo" src="https://raw.githubusercontent.com/jamesgober/rust-collection/72baabd71f00e14aa9184efcb16fa3deddda3a0a/assets/rust-logo.svg">
    <br><b>proxy-net</b><br>
    <sub><sup>API REFERENCE</sup></sub>
</h1>
<div align="center">
    <sup>
        <a href="../README.md" title="Project Home"><b>HOME</b></a>
        <span>&nbsp;│&nbsp;</span>
        <a href="./README.md" title="Documentation"><b>DOCS</b></a>
        <span>&nbsp;│&nbsp;</span>
        <span>API</span>
    </sup>
</div>
<br>

> Every public item of `proxy-net`: what it does, its parameters, its errors,
> and examples. **Version `0.2.0`, the design lock.** This file is the API
> contract: items marked *shipped* are in this build; items marked with a
> later version are the locked shape of what comes next, and their names and
> signatures change only with a migration note.

<sub>Copyright &copy; 2026 <strong>James Gober</strong>.</sub>

## Table of contents

- [Overview](#overview)
  - [Tiers](#tiers)
  - [Versions](#versions)
  - [Features](#features)
- [Tier 1](#tier-1)
  - [`proxy`](#proxy) (0.4)
- [Configuration](#configuration)
  - [`ProxyBuilder`](#proxybuilder)
  - [`Config`](#config)
  - [`Limits`](#limits)
  - [`Forwarded`, `ForwardedStyle`, `ByNode`](#forwarded-forwardedstyle-bynode)
  - [`UnknownHost`](#unknownhost)
- [Endpoints](#endpoints)
  - [`Endpoint`](#endpoint)
  - [`EndpointAddr`](#endpointaddr)
- [Routes](#routes)
  - [`Route`](#route)
  - [`HeaderRule`](#headerrule)
  - [`HostHeader`](#hostheader)
  - [`PathPolicy`, `PathCheck`](#pathpolicy-pathcheck)
- [Pools and policies](#pools-and-policies)
  - [`Pool`](#pool)
  - [`PoolConfig`](#poolconfig)
  - [`Balance`, `HashKey`](#balance-hashkey)
  - [`HealthCheck`, `CheckKind`](#healthcheck-checkkind)
  - [`Outlier`](#outlier)
  - [`Retry`](#retry)
  - [`Timeouts`](#timeouts)
  - [`PoolLimits`](#poollimits)
- [Errors](#errors)
  - [`ConfigError`](#configerror)
  - [`ConfigItem`](#configitem)
  - [`ConfigReason`](#configreason)
  - [`EndpointErrorKind`](#endpointerrorkind)
- [The sans-IO core (`proxy_net::core`)](#the-sans-io-core-proxy_netcore)
  - [`RouteTable`, `RouteMatch`](#routetable-routematch)
  - [`CompiledRoute`, `PathRewrite`](#compiledroute-pathrewrite)
- [Constants](#constants)
- [Planned items](#planned-items)

---

## Overview

proxy-net receives requests that serve-net has already admitted, picks an upstream endpoint from a pool, and forwards the request there and the response back. Requests and responses are parsed strictly on arrival and written again on departure (never passed through as raw bytes), and the forwarding headers are always removed and rebuilt from the client address serve-net resolved.

### Tiers

| Tier | For | Items |
|---|---|---|
| 1 | "put this in front of those servers" | `proxy` (0.4) |
| 2 | deployments with several pools, routes, and policies | `ProxyBuilder`, `Route`, `Pool`, `Endpoint`, the policy types, `Limits` |
| 3 | other engines, tests | `core`: the route table now; balancers, health, head rewriting, and the forwarding state machine in 0.3 |

### Versions

| Version | What it adds |
|---|---|
| 0.2.0 | the configuration surface, built and checked (this file's *shipped* items) |
| 0.3.0 | the sans-IO core: balancing, health, retry accounting, head rewriting, path safety, loop detection, the exchange state machine |
| 0.4.0 | forwarding over real sockets: the serve-net service, the upstream client and connection pool, active health checks, Tier 1 |
| 0.5.0 | WebSocket bridge, TLS and the PROXY protocol to upstreams, live reload, discovery |

### Features

| Feature | Default | What it adds |
|---|---|---|
| `std` | yes | `std` integration. Without it the crate needs only `alloc`. |
| `client` | yes, from 0.4 | the upstream side over net-io |
| `serve` | yes, from 0.4 | the serve-net service and Tier 1 |
| `tls` | no, from 0.5 | TLS to upstreams through tls-net |
| `ws` | no, from 0.5 | the WebSocket bridge through ws-net |

---

## Tier 1

### `proxy`

*0.4.0.* Listens on `listen` and forwards every request to the given endpoints: one pool, round robin, passive health, the default limits and timeouts, and forwarding headers in both styles.

```rust,ignore
pub fn proxy<A, I, S>(listen: A, upstreams: I) -> Result<Running, ProxyError>
where
    A: std::net::ToSocketAddrs,
    I: IntoIterator<Item = S>,
    S: AsRef<str>;
```

```rust,ignore
let running = proxy_net::proxy("0.0.0.0:8080", ["10.0.0.1:80", "10.0.0.2:80"])?;
running.wait();
```

Errors: `ProxyError::Config` (an endpoint does not parse), `ProxyError::Serve` (the listener could not be bound).

---

## Configuration

### `ProxyBuilder`

*Shipped.* Collects pools, routes, limits, and forwarding settings, and checks them all at once.

```rust,ignore
impl ProxyBuilder {
    pub fn new() -> ProxyBuilder;
    pub fn pool(self, name: &str, pool: Pool) -> ProxyBuilder;
    pub fn route(self, route: Route) -> ProxyBuilder;
    pub fn unknown_host(self, answer: UnknownHost) -> ProxyBuilder;
    pub fn limits(self, limits: Limits) -> ProxyBuilder;
    pub fn forwarded(self, forwarded: Forwarded) -> ProxyBuilder;
    pub fn strip_header(self, name: &str) -> ProxyBuilder;
    pub fn via(self, pseudonym: &str) -> ProxyBuilder;
    pub fn no_via(self) -> ProxyBuilder;
    pub fn check(&self) -> Result<(), ConfigError>;
    pub fn config(self) -> Result<Config, ConfigError>;
    // 0.4.0: pub fn build(self) -> Result<Proxy, ConfigError>;
    // 0.4.0: pub fn observer(self, observer: impl Observer) -> ProxyBuilder;
}
```

| Method | Behaviour |
|---|---|
| `pool` | adds a pool; the name is 1 to 64 ASCII letters, digits, `-`, `_`, `.` |
| `route` | adds a route; routes are tried in order and the first match wins |
| `unknown_host` | the answer when no route serves the host: 404 (default) or 421 |
| `limits` | size and count limits for the proxy |
| `forwarded` | which forwarding fields are written (default: both styles) |
| `strip_header` | adds a field to the set removed from every request (the forwarding set is always removed) |
| `via` | the pseudonym written in `Via` and used for loop detection (default `proxy-net`, a token of at most 64 bytes) |
| `no_via` | no `Via`, no loop detection |
| `check` | `config` without consuming the builder |
| `config` | checks everything and returns the validated [`Config`](#config) |

A builder with one pool and no routes routes every request to that pool. Several pools need routes.

```rust
use proxy_net::{Balance, Forwarded, HealthCheck, Pool, ProxyBuilder, Route};

let config = ProxyBuilder::new()
    .pool("api", Pool::new(["10.0.0.1:8080", "10.0.0.2:8080"]).balance(Balance::LeastConnections))
    .pool("web", Pool::new(["10.0.1.1:8080"]).health(HealthCheck::http("/health")))
    .route(Route::host("api.example.com").to("api"))
    .route(Route::any().to("web"))
    .forwarded(Forwarded::standard())
    .strip_header("CF-Connecting-IP")
    .via("edge-1")
    .config()?;
# Ok::<(), proxy_net::ConfigError>(())
```

```rust
// The simplest configuration: one pool, every request goes there.
let config = proxy_net::ProxyBuilder::new()
    .pool("app", proxy_net::Pool::new(["127.0.0.1:3000"]))
    .config()?;
assert_eq!(config.routes().len(), 1);
# Ok::<(), proxy_net::ConfigError>(())
```

Errors: the first problem found, as a [`ConfigError`](#configerror) naming the pool, endpoint, route, or the proxy.

### `Config`

*Shipped.* The validated configuration. Immutable; from 0.5 a reload replaces it whole.

```rust,ignore
impl Config {
    pub fn routes(&self) -> &RouteTable;
    pub fn pool(&self, name: &str) -> Option<&PoolConfig>;
    pub fn pool_at(&self, index: usize) -> Option<&PoolConfig>;
    pub fn pools(&self) -> &[PoolConfig];
    pub fn unknown_host(&self) -> UnknownHost;
    pub fn limits(&self) -> &Limits;
    pub fn forwarded(&self) -> &Forwarded;
    pub fn stripped_headers(&self) -> impl Iterator<Item = &str>;
    pub fn via(&self) -> Option<&str>;
}
```

```rust
let config = proxy_net::ProxyBuilder::new()
    .pool("app", proxy_net::Pool::new(["10.0.0.1:80"]))
    .config()?;
assert!(config.stripped_headers().any(|h| h == "x-forwarded-for"));
assert_eq!(config.via(), Some("proxy-net"));
assert_eq!(config.pool_at(0).map(|p| p.name()), Some("app"));
# Ok::<(), proxy_net::ConfigError>(())
```

### `Limits`

*Shipped.* Size and count limits. In-flight memory is bounded by `max_exchanges x (request_buffer + relay_buffer)` plus head buffers.

| Setting | Default | Range | Meaning |
|---|---|---|---|
| `max_request_body` | 100 MiB | any; 0 refuses bodies | largest request body forwarded |
| `max_response_body` | none | any | largest response body relayed |
| `request_buffer` | 64 KiB | 1 KiB to 16 MiB, at most `max_request_body` | body bytes held before an upstream connection is taken, and the most a retry replays |
| `relay_buffer` | 64 KiB | 1 KiB to 16 MiB | response bytes read per step |
| `max_exchanges` | 4,096 | at least 1 | exchanges in flight |
| `max_response_head` | 16 KiB | 1 KiB to 1 MiB | largest upstream response head |
| `max_response_fields` | 100 | 1 to 1,000 | most fields in an upstream response head |

Each setting has a setter of the same name and a getter with `_value` (`relay_buffer_value()`).

```rust
let limits = proxy_net::Limits::default()
    .max_request_body(10 * 1024 * 1024)
    .request_buffer(16 * 1024);
let config = proxy_net::ProxyBuilder::new()
    .pool("app", proxy_net::Pool::new(["10.0.0.1:80"]))
    .limits(limits)
    .config()?;
assert_eq!(config.limits().request_buffer_value(), 16 * 1024);
# Ok::<(), proxy_net::ConfigError>(())
```

Errors: `ConfigReason::Limit { field: "limits.<name>" }` on the proxy.

### `Forwarded`, `ForwardedStyle`, `ByNode`

*Shipped (settings); written from 0.3, `append_trusted` from 0.5.* Every field in [`FORWARDING_SET`](#constants) that a client sent is always removed; these settings say what the proxy writes instead.

```rust,ignore
impl Forwarded {
    pub fn both() -> Forwarded;          // Forwarded and X-Forwarded-* (the default)
    pub fn standard() -> Forwarded;      // Forwarded (RFC 7239) only
    pub fn x_forwarded() -> Forwarded;   // X-Forwarded-For, -Proto, -Host only
    pub fn off() -> Forwarded;           // nothing
    pub fn append_trusted(self) -> Forwarded;
    pub fn by(self, node: ByNode) -> Forwarded;
    pub fn style(&self) -> ForwardedStyle;
    pub fn appends_trusted(&self) -> bool;
    pub fn by_node(&self) -> &ByNode;
}
pub enum ByNode { Off, Listener, Obfuscated(Box<str>) }
impl ByNode { pub fn obfuscated(name: &str) -> ByNode; }
```

```rust
use proxy_net::{ByNode, Forwarded, Pool, ProxyBuilder};
let config = ProxyBuilder::new()
    .pool("app", Pool::new(["10.0.0.1:80"]))
    .forwarded(Forwarded::standard().by(ByNode::obfuscated("_edge1")))
    .config()?;
# let _ = config;
# Ok::<(), proxy_net::ConfigError>(())
```

Errors: `Inconsistent` when `append_trusted` or `by` is set with `off`, or `by` with `x_forwarded` (only `Forwarded` has `by=`); `Pseudonym` when an obfuscated name is not `_` followed by letters, digits, `.`, `_`, `-`.

### `UnknownHost`

*Shipped.* `NotFound` (404, default) or `Misdirected` (421); `status()` gives the code.

```rust
assert_eq!(proxy_net::UnknownHost::Misdirected.status(), 421);
```

---

## Endpoints

### `Endpoint`

*Shipped.* One upstream endpoint: its address, weight, tier, connection cap, and server name.

```rust,ignore
impl Endpoint {
    pub fn parse(text: &str) -> Result<Endpoint, ConfigError>;
    pub fn new(addr: EndpointAddr) -> Endpoint;
    pub fn weight(self, weight: u16) -> Endpoint;          // 1 to 1,000; default 1
    pub fn tier(self, tier: u8) -> Endpoint;               // 0 to 15; default 0
    pub fn max_connections(self, n: u32) -> Endpoint;      // 1 to 1,000,000; default 256
    pub fn server_name(self, name: &str) -> Result<Endpoint, ConfigError>;
    pub fn addr(&self) -> &EndpointAddr;
    pub fn weight_value(&self) -> u16;
    pub fn tier_value(&self) -> u8;
    pub fn max_connections_value(&self) -> u32;
    pub fn server_name_value(&self) -> Option<&str>;
}
impl FromStr for Endpoint { type Err = ConfigError; }
```

`parse` reads `host:port`, `ip:port`, and `[ipv6]:port`. Host names follow RFC 1123 (labels of 1 to 63 letters, digits, and hyphens, not starting or ending with a hyphen, at most 253 bytes, the last label not all digits) and are stored in lowercase without a trailing dot. Refused: IPv6 zone ids, unbracketed IPv6, userinfo, paths, queries, fragments, a port of 0 or above 65,535, a port with a sign or a leading zero, an IPv4 address with a leading zero, and any byte outside letters, digits, `.`, `-`, `:`, `[`, `]`.

The tier sets failover: tier 1 takes traffic only when tier 0 has no healthy endpoint (from 0.3).

```rust
use proxy_net::{Endpoint, Pool};
let primary = Endpoint::parse("10.0.0.1:443")?.weight(3).server_name("app.internal")?;
let standby = Endpoint::parse("10.0.9.1:443")?.tier(1);
let pool = Pool::new(Vec::<&str>::new()).endpoint(primary).endpoint(standby);
# let _ = pool;
# Ok::<(), proxy_net::ConfigError>(())
```

```rust
use proxy_net::{ConfigReason, Endpoint, EndpointErrorKind};
let err = Endpoint::parse("10.0.0.1:080").unwrap_err();
assert_eq!(err.reason(), &ConfigReason::EndpointSyntax(EndpointErrorKind::BadPort));
```

Errors: `EndpointSyntax(kind)`; `weight`, `tier`, and `max_connections` are checked when the configuration is built (`Weight`, `Limit`).

### `EndpointAddr`

*Shipped.*

```rust,ignore
pub enum EndpointAddr { Ip(SocketAddr), Name { host: Box<str>, port: u16 } }
impl EndpointAddr {
    pub fn port(&self) -> u16;
    pub fn host_name(&self) -> Option<&str>;
}
```

`Display` writes the form `Endpoint::parse` reads back.

```rust
let e = proxy_net::Endpoint::parse("[2001:db8::1]:8080")?;
assert_eq!(e.addr().to_string(), "[2001:db8::1]:8080");
assert_eq!(e.addr().port(), 8080);
# Ok::<(), proxy_net::ConfigError>(())
```

---

## Routes

### `Route`

*Shipped (matching and validation); rewrites applied from 0.3; `websocket` from 0.5.*

```rust,ignore
impl Route {
    pub fn any() -> Route;
    pub fn host(pattern: &str) -> Route;
    pub fn path(self, pattern: &str) -> Route;
    pub fn exact(self, pattern: &str) -> Route;
    pub fn methods(self, methods: &[&str]) -> Route;
    pub fn to(self, pool: &str) -> Route;
    pub fn strip_prefix(self, prefix: &str) -> Route;
    pub fn replace_prefix(self, from: &str, to: &str) -> Route;
    pub fn host_header(self, mode: HostHeader) -> Route;
    pub fn request_header(self, rule: HeaderRule) -> Route;
    pub fn response_header(self, rule: HeaderRule) -> Route;
    pub fn websocket(self, allow: bool) -> Route;
    pub fn path_policy(self, policy: PathPolicy) -> Route;
    pub fn allow_body_on(self, methods: &[&str]) -> Route;
}
```

| Pattern | Matches |
|---|---|
| `host("api.example.com")` | that host, any case, with or without a port or a trailing dot |
| `host("*.example.com")` | any name under `example.com` (one label or more); never `example.com` itself |
| `host("10.0.0.1")`, `host("[::1]")` | that address |
| `host("*")`, `any()` | every host |
| `path("/api")` | `/api` and everything below it (`/api/users`), never `/apix` |
| `path("/api/")` | everything below `/api/`, not `/api` |
| `path("/")` | every path |
| `exact("/health")` | only `/health` |

Paths are matched as received (percent-encoded, never decoded) and the query is ignored. A route that an earlier route shadows entirely is refused (`Shadowed { by }`), since it could never match.

Rewrite prefixes must start the route's own path and end at a segment boundary. Header rules cannot touch fields the proxy writes itself. `allow_body_on` lets `GET`, `HEAD`, `OPTIONS`, `TRACE`, or `DELETE` carry a body, which is refused by default because a body on those methods is a known desync vector.

```rust
use proxy_net::{HeaderRule, HostHeader, Pool, ProxyBuilder, Route};
let config = ProxyBuilder::new()
    .pool("api", Pool::new(["10.0.0.1:8080"]))
    .pool("web", Pool::new(["10.0.0.2:8080"]))
    .route(
        Route::host("example.com")
            .path("/api")
            .methods(&["GET", "POST"])
            .to("api")
            .strip_prefix("/api")
            .host_header(HostHeader::fixed("api.internal"))
            .request_header(HeaderRule::set("x-env", "production"))
            .response_header(HeaderRule::remove("server")),
    )
    .route(Route::any().to("web"))
    .config()?;
# let _ = config;
# Ok::<(), proxy_net::ConfigError>(())
```

Errors (as `ConfigError` on `Route(index)`): `HostPattern`, `PathPattern`, `Method`, `PathRewrite`, `HeaderName`, `HeaderValue`, `ReservedHeader`, `MissingPool`, `UnknownPool`, `Shadowed`.

### `HeaderRule`

*Shipped (validation); applied from 0.3.*

```rust,ignore
pub enum HeaderRule { Set(Box<str>, Box<str>), Add(Box<str>, Box<str>), Remove(Box<str>) }
impl HeaderRule {
    pub fn set(name: &str, value: &str) -> HeaderRule;
    pub fn add(name: &str, value: &str) -> HeaderRule;
    pub fn remove(name: &str) -> HeaderRule;
    pub fn name(&self) -> &str;          // lowercase
    pub fn value(&self) -> Option<&str>;
}
```

Names must be tokens; values must be field values (no CR, LF, NUL, or other control bytes, no leading or trailing whitespace). Rules on `Connection`, `Content-Length`, `Expect`, `Host`, `Keep-Alive`, `Proxy-Authenticate`, `Proxy-Authorization`, `Proxy-Connection`, `TE`, `Trailer`, `Transfer-Encoding`, `Upgrade`, `Via`, or the forwarding set are refused.

```rust
let rule = proxy_net::HeaderRule::add("Cache-Status", "edge; hit");
assert_eq!(rule.name(), "cache-status");
```

### `HostHeader`

*Shipped (validation); applied from 0.3.* `Preserve` (the client's `Host`, default), `Endpoint` (the endpoint's server name, or its address), or `Fixed(host)` via `HostHeader::fixed("app.internal:8080")`.

### `PathPolicy`, `PathCheck`

*Shipped (settings); checked on requests from 0.3.* Which request paths a route refuses with 400 because a proxy and an origin could read them differently.

| `PathCheck` | Refuses |
|---|---|
| `DotSegment` | `.` and `..` segments, raw or percent-encoded in any case |
| `EncodedSlash` | `%2f`, `%5c` |
| `Backslash` | a raw `\` |
| `EncodedNul` | `%00` |
| `Semicolon` | `;` inside a segment matched by a prefix route (`/app/..;/admin`) |

```rust
use proxy_net::{PathCheck, PathPolicy, Route};
let policy = PathPolicy::STRICT.allow(PathCheck::EncodedSlash);   // STRICT is the default
assert!(policy.refuses(PathCheck::DotSegment));
let route = Route::any().to("app").path_policy(policy);
# let _ = route;
```

---

## Pools and policies

### `Pool`

*Shipped (settings).*

```rust,ignore
impl Pool {
    pub fn new<I, S>(endpoints: I) -> Pool where I: IntoIterator<Item = S>, S: AsRef<str>;
    pub fn endpoint(self, endpoint: Endpoint) -> Pool;
    pub fn balance(self, policy: Balance) -> Pool;
    pub fn health(self, check: HealthCheck) -> Pool;
    pub fn outlier(self, outlier: Outlier) -> Pool;
    pub fn retry(self, retry: Retry) -> Pool;
    pub fn timeouts(self, timeouts: Timeouts) -> Pool;
    pub fn limits(self, limits: PoolLimits) -> Pool;
    // 0.5.0: proxy_protocol, tls, discovery
}
```

Endpoint text is parsed when the configuration is built, so an error names the pool and the endpoint's position. A pool holds 1 to 4,096 endpoints, each address once.

```rust
use std::time::Duration;
use proxy_net::{Balance, HashKey, HealthCheck, Pool, Retry};
let pool = Pool::new(["10.0.0.1:8080", "10.0.0.2:8080", "10.0.0.3:8080"])
    .balance(Balance::consistent_hash(HashKey::Cookie("session".into())))
    .health(HealthCheck::http("/ready").interval(Duration::from_secs(2)))
    .retry(Retry::default().on_status(&[503]));
# let _ = pool;
```

Errors (on `Pool(name)` or `Endpoint { pool, index }`): `PoolName`, `NoEndpoints`, `DuplicateEndpoint`, `EndpointSyntax`, `Weight`, `Limit`, `HeaderName`, `PathPattern`, `HeaderValue`.

### `PoolConfig`

*Shipped.* A checked pool: `name()`, `endpoints()`, `balance()`, `health()`, `outlier()`, `retry()`, `timeouts()`, `limits()`.

### `Balance`, `HashKey`

*Shipped (settings); behaviour from 0.3.*

| Policy | How it picks |
|---|---|
| `RoundRobin` (default) | smooth weighted round robin: each endpoint gets its weight's share, interleaved |
| `LeastConnections` | the fewest active requests for its weight |
| `PowerOfTwo` | two at random by weight; fewer outstanding requests wins, then lower latency |
| `ConsistentHash { key, load_percent }` | Maglev hashing on the key, so one key keeps one endpoint; an endpoint above `load_percent` of the mean load passes the key on |
| `Random` | at random by weight |
| `First` | the first healthy endpoint, in order (active and standby) |

`HashKey`: `ClientAddress`, `Header(name)`, `Cookie(name)`, `Path`, `PathAndQuery`. A missing header or cookie falls back to round robin. `Balance::consistent_hash(key)` uses a load bound of 125%; `.bounded_load(percent)` sets 100 to 1,000, or 0 for none.

```rust
use proxy_net::{Balance, HashKey};
let sticky = Balance::consistent_hash(HashKey::ClientAddress).bounded_load(150);
# let _ = sticky;
```

### `HealthCheck`, `CheckKind`

*Shipped (settings); the checker runs from 0.4.*

| Setting | Default | Range |
|---|---|---|
| `HealthCheck::http(path)` | `GET path`, healthy on 200 to 399 | path as a route path |
| `HealthCheck::tcp()` | a connect (and TLS when the pool uses it) | |
| `interval` | 5 s | 100 ms to 1 h |
| `timeout` | 2 s | above 0, below the interval |
| `jitter` | 10% | 0 to 50 |
| `rise` / `fall` | 2 / 3 | 1 to 100 |
| `expect_status` | 200..=399 | within 100..=599 |
| `expect_body` | none | 1 to 4,096 bytes, looked for in the first 4 KiB |
| `host` | the endpoint's server name | a host or host:port |

```rust
use std::time::Duration;
let check = proxy_net::HealthCheck::http("/ready")
    .timeout(Duration::from_millis(500))
    .expect_status(200..=204)
    .expect_body("ok");
# let _ = check;
```

### `Outlier`

*Shipped (settings); behaviour from 0.3.* Passive health from real traffic.

| Setting | Default | Meaning |
|---|---|---|
| `consecutive_failures` | 5 | connect failures or gateway errors in a row that eject an endpoint; 0 turns it off |
| `base_ejection` | 30 s | the first ejection; each later one lasts this times the ejection count |
| `max_ejection` | 300 s | the cap |
| `max_ejected` | 50% | most of a pool ejected at once |
| `panic_below` | 50% | below this share of healthy endpoints, balance over all of them; 0 turns it off |

### `Retry`

*Shipped (settings); behaviour from 0.4.* Only idempotent methods are retried (GET, HEAD, OPTIONS, TRACE, PUT, DELETE), only when the body is held in full, and only before a response byte reached the client.

| Setting | Default | Range |
|---|---|---|
| `attempts` | 2 (one retry) | 1 to 5; `Retry::off()` is 1 |
| `on_status` | none | statuses 500 to 599 retried like a failure |
| `backoff` | 25 ms with full jitter, only before a retry to the same endpoint | up to 10 s |
| `budget(percent, min)` | 20%, 3 | retries in flight stay under this share of active requests or `min` |

### `Timeouts`

*Shipped (settings); behaviour from 0.4.*

| Setting | Default |
|---|---|
| `connect` (TLS included) | 5 s |
| `response_head` (request sent to response head) | 60 s |
| `read_idle` (between reads) | 60 s |
| `write_idle` (a write making no progress) | 60 s |
| `total` | none; at least `connect` when set |
| `tunnel_idle` (bridged WebSocket) | 300 s |

### `PoolLimits`

*Shipped (settings); behaviour from 0.4.*

| Setting | Default | Meaning |
|---|---|---|
| `max_pending` | 1,024 | requests waiting for a connection when every endpoint is at its cap |
| `pending_timeout` | 1 s | how long one waits |
| `max_idle` | 32 | idle connections kept per endpoint; 0 turns reuse off |
| `idle_timeout` | 4 s | keep it below the endpoint's own keep-alive timeout |
| `max_exchanges_per_connection` | 1,000 | then the connection is closed |
| `fresh_for` | 1 s | a non-idempotent request only reuses a connection used this recently, so the keep-alive close race cannot lose it |

---

## Errors

### `ConfigError`

*Shipped.* `item()` says where, `reason()` says what; `Display` says both in one line.

```rust
let err = proxy_net::ProxyBuilder::new()
    .pool("db", proxy_net::Pool::new(["10.0.0.1"]))
    .config()
    .unwrap_err();
assert_eq!(err.to_string(), "pool \"db\" endpoint 0: the port is missing (write host:port)");
```

### `ConfigItem`

*Shipped.* `Endpoint { pool, index }`, `Pool(name)`, `Route(index)`, `Proxy`.

### `ConfigReason`

*Shipped.*

| Reason | When | What to do |
|---|---|---|
| `EndpointSyntax(kind)` | an endpoint does not parse | see [`EndpointErrorKind`](#endpointerrorkind) |
| `NoEndpoints` | a pool is empty | add an endpoint |
| `DuplicateEndpoint` | one address twice in a pool | remove one, or raise its weight |
| `DuplicatePool` | two pools share a name | rename one |
| `PoolName` | a bad pool name | 1 to 64 ASCII letters, digits, `-`, `_`, `.` |
| `UnknownPool` | a route names a missing pool | add the pool or fix the name |
| `MissingPool` | a route has no `.to(..)` | add it |
| `NoRoutes` | no pools, or several pools and no routes | add them |
| `HostPattern` | a bad host pattern | a host, an address, `*`, or `*.` and a host |
| `PathPattern` | a bad path (also for health check paths) | start with `/`; path bytes; no dot segment, query, or fragment |
| `PathRewrite` | a rewrite prefix not at the start of the route's path, or ending inside a segment | fix the prefix |
| `Shadowed { by }` | an earlier route takes every request this one would | reorder: narrower routes first |
| `Method` | a method that is not a token, or an empty list | fix it |
| `HeaderName` | a header or cookie name that is not a token | fix it |
| `HeaderValue` | a value with control bytes or edge whitespace, or a bad fixed host | fix it |
| `ReservedHeader` | a rule on a field the proxy writes | remove the rule |
| `Pseudonym` | a bad `Via` pseudonym or obfuscated node | a token; `_` and letters, digits, `.`, `_`, `-` |
| `Weight` | a weight outside 1 to 1,000 | fix it |
| `Limit { field }` | a setting out of range | see that setting's range |
| `Inconsistent { what }` | settings that contradict | as described |

### `EndpointErrorKind`

*Shipped.* `Empty`, `MissingPort`, `BadPort`, `BadHost`, `UserInfo`, `Path`, `TooLong`, `BadByte`; each `Display` says what to write instead.

---

## The sans-IO core (`proxy_net::core`)

Decisions without sockets, threads, or clocks; needs only `alloc`.

### `RouteTable`, `RouteMatch`

*Shipped.*

```rust,ignore
impl RouteTable {
    pub fn find(&self, host: &[u8], path: &[u8], method: &[u8]) -> RouteMatch<'_>;
    pub fn len(&self) -> usize;
    pub fn is_empty(&self) -> bool;
    pub fn get(&self, index: usize) -> Option<&CompiledRoute>;
    pub fn iter(&self) -> core::slice::Iter<'_, CompiledRoute>;
}
pub enum RouteMatch<'a> { Route { index: usize, route: &'a CompiledRoute }, NoRoute, UnknownHost }
```

`find` takes the `Host` value (case, a port, and a trailing dot are ignored), the path as received (anything from `?` on is ignored), and the method, and allocates nothing. `NoRoute` means a route serves the host but not the path or method (404); `UnknownHost` means none serves the host (404 or 421).

```rust
use proxy_net::core::RouteMatch;
use proxy_net::{Pool, ProxyBuilder, Route};
let config = ProxyBuilder::new()
    .pool("app", Pool::new(["10.0.0.1:80"]))
    .route(Route::host("example.com").path("/app").to("app"))
    .config()?;
let t = config.routes();
assert!(matches!(t.find(b"EXAMPLE.com:443", b"/app/x?y", b"GET"), RouteMatch::Route { index: 0, .. }));
assert_eq!(t.find(b"example.com", b"/other", b"GET"), RouteMatch::NoRoute);
assert_eq!(t.find(b"example.org", b"/app", b"GET"), RouteMatch::UnknownHost);
# Ok::<(), proxy_net::ConfigError>(())
```

### `CompiledRoute`, `PathRewrite`

*Shipped.* A checked route: `pool_index()`, `rewrite()`, `host_header()`, `request_rules()`, `response_rules()`, `websocket()`, `path_policy()`, `is_prefix()`, `allows_body_on(method)`. `PathRewrite` is `None`, `Strip(prefix)`, or `Replace(from, to)`.

---

## Constants

| Constant | Value |
|---|---|
| `VERSION` | the crate version |
| `FORWARDING_SET` | `forwarded`, `x-forwarded-for`, `x-forwarded-proto`, `x-forwarded-host`, `x-forwarded-port`, `x-forwarded-prefix`, `x-real-ip`, `x-client-ip`, `true-client-ip`, `x-original-forwarded-for` |
| `DEFAULT_PSEUDONYM` | `proxy-net` |
| `DEFAULT_MAX_CONNECTIONS` | 256 |
| `MAX_ENDPOINTS` | 4,096 per pool |
| `MAX_ROUTES` | 4,096 |

---

## Planned items

The locked shape of what follows 0.2. Names and signatures here change only with a migration note.

### 0.3.0: the core

```rust,ignore
pub struct Tick(pub u64);                         // nanoseconds from the caller's origin
pub struct Balancer;                              // pick(key, exclude), started(id), finished(id, outcome, latency)
pub struct HealthTable;                           // record(id, outcome, now), check_result(id, ok, now), state(id)
pub enum EndpointState { Unknown, Healthy, Unhealthy, Ejected { until: Tick }, Draining }
pub struct Exchange;                              // new(request, route, pool, now), on(event, now) -> Action, deadline()
pub fn request_fields(..) -> Result<(), RefusalKind>;   // hop-by-hop, forwarding set, rules, Host, forwarding fields, Via
pub fn response_fields(..) -> Result<(), RefusalKind>;
pub fn check_path(path: &[u8], policy: PathPolicy, prefix_matched: bool) -> Result<(), PathCheck>;
pub fn detect_loop(via_values: &[&[u8]], pseudonym: &str) -> bool;
pub enum ErrorClass { Connect, Tls, Timeout(TimeoutKind), UpstreamProtocol, UpstreamReset, ClientGone,
                      Limit(LimitKind), NoEndpoint, Breaker, Loop, Refused(RefusalKind) }
pub struct ForwardError;                          // class(), endpoint(), attempt()
```

### 0.4.0: forwarding

```rust,ignore
pub fn proxy(listen, upstreams) -> Result<Running, ProxyError>;
impl ProxyBuilder { pub fn build(self) -> Result<Proxy, ConfigError>; pub fn observer(self, o: impl Observer) -> ProxyBuilder; }
impl Proxy { pub fn handle(&self) -> ProxyHandle; pub fn service(&self) -> ProxyService; pub fn serve(self, listen) -> Result<Running, ProxyError>; }
pub struct Running;                               // handle(), stop(), drain(within), wait(), local_addr()
pub trait Observer: Send + Sync + 'static { /* one method per event, all with empty defaults */ }
pub trait Connector { type Conn: UpstreamIo; fn connect(&self, endpoint, prelude, deadline) -> Result<Self::Conn, ForwardError>; }
pub trait UpstreamIo { fn send(..); fn recv(..); fn is_reusable(&mut self) -> bool; fn close(self); }
pub struct TcpConnector;
impl ProxyHandle { pub fn health(&self) -> HealthSnapshot; pub fn stats(&self) -> ProxyStats; }
pub enum ProxyError { Config(ConfigError), Serve(serve_net::ServeError) }
```

Until serve-net can park a streamed reply, `ProxyService` needs serve-net's blocking workers; `Proxy::serve` and `proxy` set them.

### 0.5.0: feature complete

```rust,ignore
impl Pool {
    pub fn proxy_protocol(self, version: ProxyProtocol) -> Pool;   // Off, V1, V2 { authority, alpn, unique_id, crc }
    pub fn tls(self, tls: UpstreamTls) -> Pool;                    // roots, pinning, SNI, client certificates
    pub fn discovery(self, source: impl Discovery) -> Pool;
}
impl ProxyHandle {
    pub fn reload(&self, builder: ProxyBuilder) -> Result<(), ConfigError>;
    pub fn set_endpoints(&self, pool: &str, endpoints: &[Endpoint]) -> Result<(), ConfigError>;
    pub fn drain_endpoint(&self, pool: &str, endpoint: &EndpointAddr) -> bool;
}
pub trait Discovery: Send + Sync + 'static { fn endpoints(&self) -> Result<Vec<Endpoint>, DiscoveryError>; fn refresh_every(&self) -> Option<Duration>; }
pub struct StaticDiscovery; pub struct DnsDiscovery;
pub struct TlsConnector;
```
