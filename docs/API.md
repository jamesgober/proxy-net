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
> and examples. **Version `0.3.0`, the sans-IO core.** This file is the API
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
  - [`Tick`](#tick)
  - [`Balancer`](#balancer)
  - [`HealthTable`, `EndpointId`, `EndpointState`, `Transition`](#healthtable-endpointid-endpointstate-transition)
  - [`RetryBudget`, `is_idempotent`](#retrybudget-is_idempotent)
  - [`check_request`, `check_path`](#check_request-check_path)
  - [`request_fields`, `response_fields`, `ForwardFacts`, `FieldList`](#request_fields-response_fields-forwardfacts-fieldlist)
  - [`detect_loop`, `rewrite_target`](#detect_loop-rewrite_target)
  - [`Exchange`, `Event`, `Action`](#exchange-event-action)
  - [`ErrorClass`, `Outcome`, `ForwardError`](#errorclass-outcome-forwarderror)
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
| 3 | other engines, tests | `core`: the route table, balancers, health, the retry budget, request checks, head rewriting, and the forwarding state machine |

### Versions

| Version | What it adds |
|---|---|
| 0.2.0 | the configuration surface, built and checked (this file's *shipped* items) |
| 0.3.0 | the sans-IO core: balancing, health, retry accounting, head rewriting, path safety, loop detection, the exchange state machine (*shipped*) |
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
    pub fn frame_facts(self, on: bool) -> ProxyBuilder;   // 0.3.0
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
| `frame_facts` | add a `proxy-frame` field stating how the proxy framed each upstream request (`length=N`, `chunked`, or `none`, and the field count), so an upstream that reads it can detect a framing disagreement; off by default. A client's `proxy-frame` is always removed |
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
    pub fn frame_facts(&self) -> bool;
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
| `EncodedNul` | `%00`, or a raw NUL |
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

### `Tick`

*Shipped (0.3).* Time as the core sees it: nanoseconds from an origin the caller chooses. The core never reads a clock.

```rust,ignore
pub struct Tick(pub u64);
impl Tick {
    pub fn after(self, d: Duration) -> Tick;     // saturating
    pub fn since(self, earlier: Tick) -> Duration; // zero when earlier is later
    pub fn nanos(self) -> u64;
}
```

```rust
use std::time::Duration;
use proxy_net::core::Tick;
let start = Tick(0);
assert_eq!(start.after(Duration::from_millis(5)).since(start), Duration::from_millis(5));
```

### `Balancer`

*Shipped (0.3).* Balancing state for one pool. Picks read atomics only; the eligible set is recomputed by `refresh` after a health change.

```rust,ignore
impl Balancer {
    pub fn new(pool: &PoolConfig) -> Balancer;
    pub fn key(bytes: &[u8]) -> u64;                       // hash of a request key
    pub fn refresh(&self, health: &HealthTable) -> usize;  // returns the number eligible
    pub fn pick(&self, key: Option<u64>, exclude: &[EndpointId]) -> Option<EndpointId>;
    pub fn started(&self, id: EndpointId);
    pub fn finished(&self, id: EndpointId, latency_ns: u64);
    pub fn is_eligible(&self, id: EndpointId) -> bool;
    pub fn active(&self, id: EndpointId) -> u32;
    pub fn latency(&self, id: EndpointId) -> u64;
    pub fn eligible_count(&self) -> usize;
}
```

| Policy | How a pick works |
|---|---|
| `RoundRobin` | a schedule built once: each endpoint appears `weight` times per cycle, its turns spread evenly; picks walk it with one atomic counter |
| `LeastConnections` | the eligible endpoint with the fewest active exchanges for its weight, compared by cross-multiplying; ties rotate |
| `PowerOfTwo` | two picks at random by weight; fewer active exchanges wins, then the lower latency estimate |
| `ConsistentHash` | a Maglev table (at least 65,537 slots, a prime) built from the endpoint addresses, so every node with the same configuration builds the same table; an ineligible endpoint's keys move to the next slots, so the other endpoints keep theirs; with a load bound, an endpoint over `ceil(load_percent x (active + 1) / (100 x eligible))` passes the key on |
| `Random` | at random by weight |
| `First` | the first eligible endpoint in order |

The eligible set is the healthy endpoints of the lowest tier that has one. When the healthy share of the pool is below `Outlier::panic_below`, every endpoint that is not draining in the lowest tier is eligible instead. `exclude` (the endpoints a retry already tried) is honoured when another endpoint is eligible.

```rust
use proxy_net::core::{Balancer, EndpointId, HealthTable};
use proxy_net::{Balance, HashKey, Pool, ProxyBuilder};

let config = ProxyBuilder::new()
    .pool("app", Pool::new(["10.0.0.1:80", "10.0.0.2:80", "10.0.0.3:80"])
        .balance(Balance::consistent_hash(HashKey::Cookie("session".into()))))
    .config()?;
let pool = config.pool("app").expect("pool");
let (balancer, health) = (Balancer::new(pool), HealthTable::new(pool));
balancer.refresh(&health);
let key = Balancer::key(b"session-41");
let home = balancer.pick(Some(key), &[]).expect("pick");
assert_eq!(balancer.pick(Some(key), &[]), Some(home)); // the same key keeps its endpoint
// A retry avoids the endpoint that failed.
assert_ne!(balancer.pick(Some(key), &[home]), Some(home));
# Ok::<(), proxy_net::ConfigError>(())
```

### `HealthTable`, `EndpointId`, `EndpointState`, `Transition`

*Shipped (0.3).* The health of every endpoint of one pool. Each endpoint's state is one 64-bit word changed by compare-and-swap.

```rust,ignore
pub struct EndpointId(pub u16);
pub enum EndpointState { Unknown, Healthy, Unhealthy, Ejected { until: Tick }, Draining }
pub struct Transition { pub endpoint: EndpointId, pub from: EndpointState, pub to: EndpointState }
impl HealthTable {
    pub fn new(pool: &PoolConfig) -> HealthTable;
    pub fn record(&self, id: EndpointId, outcome: Outcome, now: Tick) -> Option<Transition>;   // passive
    pub fn check_result(&self, id: EndpointId, ok: bool, now: Tick) -> Option<Transition>;    // active
    pub fn expire(&self, now: Tick, changes: &mut Vec<Transition>);                           // ends ejections
    pub fn drain(&self, id: EndpointId) -> Option<Transition>;
    pub fn state(&self, id: EndpointId) -> EndpointState;
    pub fn is_healthy(&self, id: EndpointId) -> bool;
    pub fn healthy_count(&self) -> usize;
    pub fn ejected_count(&self) -> u32;
}
```

- **Passive.** A success resets the failure count. `Outlier::consecutive_failures` failures in a row eject an endpoint for `base_ejection` times its number of ejections, up to `max_ejection`, unless that would eject more than `max_ejected` percent of the pool. With the default 50%, a pool of one endpoint never ejects it.
- **Active.** `rise` successes in a row make an `Unknown` or `Unhealthy` endpoint healthy; `fall` failures make an `Unknown` or `Healthy` one unhealthy. In a pool with an active check, `Unknown` takes no traffic.
- **Time.** Ejections end when the caller runs `expire` (from 0.4, the health checker does). Every transition is returned, so the caller refreshes the balancer only when something changed.

```rust
use proxy_net::core::{EndpointId, EndpointState, HealthTable, Outcome, Tick};
use proxy_net::{Outlier, Pool, ProxyBuilder};

let config = ProxyBuilder::new()
    .pool("app", Pool::new(["10.0.0.1:80", "10.0.0.2:80"]).outlier(Outlier::default().consecutive_failures(2)))
    .config()?;
let health = HealthTable::new(config.pool("app").expect("pool"));
let a = EndpointId(0);
let _ = health.record(a, Outcome::Failure, Tick(0));
let t = health.record(a, Outcome::Failure, Tick(0)).expect("ejected");
assert!(matches!(t.to, EndpointState::Ejected { .. }));
let mut changes = Vec::new();
health.expire(Tick(u64::MAX), &mut changes);
assert_eq!(changes[0].to, EndpointState::Healthy);
# Ok::<(), proxy_net::ConfigError>(())
```

### `RetryBudget`, `is_idempotent`

*Shipped (0.3).* Retries in flight stay under `max(min, percent x active / 100)` per pool (`Retry::budget`, default 20% and 3), so a failing pool cannot multiply its own load. `is_idempotent` is true for `GET`, `HEAD`, `OPTIONS`, `TRACE`, `PUT`, and `DELETE` (RFC 9110 9.2.2), case-sensitive.

```rust
use proxy_net::core::{is_idempotent, RetryBudget};
assert!(is_idempotent(b"PUT") && !is_idempotent(b"POST"));
let budget = RetryBudget::new(&proxy_net::Retry::default());
for _ in 0..50 {
    budget.exchange_started();
}
let granted = (0..50).filter(|_| budget.try_retry()).count();
assert_eq!(granted, 10); // 20% of 50
```

### `check_request`, `check_path`

*Shipped (0.3).* The checks before a request is forwarded.

```rust,ignore
pub fn check_request(method: &[u8], path: &[u8], has_body: bool, route: &CompiledRoute) -> Result<(), ErrorClass>;
pub fn check_path(path: &[u8], policy: PathPolicy, prefix_route: bool) -> Result<(), PathCheck>;
```

`check_request` refuses `CONNECT`, `TRACE` unless the route lists it in its methods (405), a body on `GET`, `HEAD`, `OPTIONS`, `TRACE`, or `DELETE` unless the route allows it (400), and a path the route's policy refuses (400). `check_path` scans the path once (the query is ignored) for the forms in [`PathCheck`](#pathpolicy-pathcheck).

```rust
use proxy_net::core::{check_path, check_request, ErrorClass, RefusalKind};
use proxy_net::{PathCheck, PathPolicy, Pool, ProxyBuilder};

assert_eq!(check_path(b"/app/..;/admin", PathPolicy::STRICT, true), Err(PathCheck::Semicolon));
let config = ProxyBuilder::new().pool("app", Pool::new(["10.0.0.1:80"])).config()?;
let route = config.routes().get(0).expect("route");
assert_eq!(check_request(b"TRACE", b"/", false, route), Err(ErrorClass::Refused(RefusalKind::Method)));
assert_eq!(check_request(b"POST", b"/upload", true, route), Ok(()));
# Ok::<(), proxy_net::ConfigError>(())
```

### `request_fields`, `response_fields`, `ForwardFacts`, `FieldList`

*Shipped (0.3).* Head rewriting.

```rust,ignore
pub fn request_fields(fields: &[(&str, &[u8])], facts: &ForwardFacts<'_>, route: &CompiledRoute,
    config: &Config, out: &mut FieldList, target: &mut Vec<u8>) -> Result<(), ErrorClass>;
pub fn response_fields(fields: &[(&str, &[u8])], route: &CompiledRoute, config: &Config,
    out: &mut FieldList) -> Result<(), ErrorClass>;

impl<'a> ForwardFacts<'a> {
    pub fn new(method: &'a [u8], target: &'a [u8], host: &'a [u8], endpoint: &'a Endpoint) -> ForwardFacts<'a>;
    pub fn client(self, client: Option<SocketAddr>) -> ForwardFacts<'a>;   // resolved by serve-net
    pub fn https(self, https: bool) -> ForwardFacts<'a>;
    pub fn listener(self, listener: Option<SocketAddr>) -> ForwardFacts<'a>;
    pub fn upstream_tls(self, tls: bool) -> ForwardFacts<'a>;
    pub fn body(self, body: BodyFacts) -> ForwardFacts<'a>;              // None, Length(n), Chunked
}
impl FieldList { new, clear, push, remove, set, iter, get, get_all, len, is_empty }
```

The request side, in order:

1. refuse a request whose `Via` names this proxy (`Loop`, answered 508);
2. drop the hop-by-hop fields (`Connection` and every field it names, `Keep-Alive`, `Proxy-Connection`, `TE`, `Transfer-Encoding`, `Upgrade`, `Trailer`, `Proxy-Authenticate`, `Proxy-Authorization`), `Content-Length`, `Host`, `Expect: 100-continue`, the forwarding set, and any client `proxy-frame`;
3. write `Host` first (the route's `HostHeader`), then the kept fields in their order;
4. apply the route's request rules;
5. add `Forwarded` (written by parse-net) and `X-Forwarded-For`, `-Proto`, `-Host` in the configured style, then `Via: 1.1 <pseudonym>`, then `proxy-frame` when on;
6. write the target with the route's path rewrite and the query unchanged.

The proxy's own fields are added after the removals, so a `Connection` option cannot remove them. The response side drops hop-by-hop fields and the framing (the inbound engine frames the body), applies the response rules, and adds `Via`.

```rust
use proxy_net::core::{request_fields, FieldList, ForwardFacts};
use proxy_net::{Pool, ProxyBuilder, Route};

let config = ProxyBuilder::new()
    .pool("api", Pool::new(["10.0.0.1:8080"]))
    .route(Route::any().path("/v1").to("api").strip_prefix("/v1"))
    .config()?;
let route = config.routes().get(0).expect("route");
let endpoint = &config.pool("api").expect("pool").endpoints()[0];
let incoming: [(&str, &[u8]); 3] = [
    ("Host", b"api.example.com"),
    ("X-Forwarded-For", b"10.6.6.6"),
    ("Accept", b"application/json"),
];
let facts = ForwardFacts::new(b"GET", b"/v1/users?page=2", b"api.example.com", endpoint)
    .client("203.0.113.9:40000".parse().ok())
    .https(true);
let (mut fields, mut target) = (FieldList::new(), Vec::new());
request_fields(&incoming, &facts, route, &config, &mut fields, &mut target).expect("forward");
assert_eq!(target, b"/users?page=2");
assert_eq!(fields.get("x-forwarded-for"), Some(&b"203.0.113.9"[..]));
assert_eq!(fields.get("forwarded"), Some(&b"for=203.0.113.9;proto=https;host=api.example.com"[..]));
assert_eq!(fields.get("via"), Some(&b"1.1 proxy-net"[..]));
# Ok::<(), proxy_net::ConfigError>(())
```

Errors: `Loop`; `Refused(InvalidField)` when a request's `Connection` is not a list; `UpstreamProtocol` when a response's is not.

### `detect_loop`, `rewrite_target`

*Shipped (0.3).*

```rust,ignore
pub fn detect_loop<'v, I: IntoIterator<Item = &'v [u8]>>(via_values: I, pseudonym: &str) -> bool;
pub fn rewrite_target(target: &[u8], rewrite: &PathRewrite, out: &mut Vec<u8>);
```

```rust
use proxy_net::core::{detect_loop, rewrite_target, PathRewrite};
assert!(detect_loop([&b"1.0 fred, 1.1 Proxy-Net"[..]], "proxy-net"));
let mut out = Vec::new();
rewrite_target(b"/old/a?b", &PathRewrite::Replace("/old".into(), "/new".into()), &mut out);
assert_eq!(out, b"/new/a?b");
```

### `Exchange`, `Event`, `Action`

*Shipped (0.3).* The forwarding state machine of one request. It owns no socket and reads no clock: the caller feeds events and the time, and does what the returned action says.

```rust,ignore
impl Exchange {
    pub fn new(method: &[u8], replayable: bool, pool: &PoolConfig, now: Tick) -> Exchange;
    pub fn on(&mut self, event: Event, now: Tick) -> Action;
    pub fn deadline(&self) -> Option<Tick>;
    pub fn tried(&self) -> &[EndpointId];
    pub fn endpoint(&self) -> Option<EndpointId>;
    pub fn attempt(&self) -> u8;
    pub fn relayed(&self) -> bool;
    pub fn last_failure(&self) -> Option<ErrorClass>;
    pub fn is_over(&self) -> bool;
}
pub enum Event { Start, Picked(EndpointId), NoEndpoint, Connected, ConnectFailed(ErrorClass), Sent, Progress,
                 ResponseHead(u16), ResponseEnd { reusable: bool }, UpstreamFailed(ErrorClass), ClientGone,
                 RetryGranted, RetryDenied, Tick }
pub enum Action { Pick { attempt }, Connect { endpoint, deadline }, Send { deadline }, Wait { deadline },
                  Relay { deadline }, Retry { class }, Backoff { endpoint, until }, Finish { endpoint, reuse },
                  Fail { class, status }, Nothing }
```

Rules the machine holds (each covered by property tests and a fuzz target):

- a retry is offered only for an idempotent method whose body is held (`replayable`), before any response byte was relayed, after a retryable failure (connect, TLS, a reset before the response, a connect timeout, or a status in `Retry::on_status`), and within `Retry::attempts`; the caller asks the budget and answers `RetryGranted` or `RetryDenied`;
- a timeout after the request was sent is never retried: the upstream may still be working on it;
- a retry to an endpoint already tried waits `Retry::backoff` first;
- `Finish { reuse: true }` only after the whole response, with no response before the request was sent in full;
- every state has a deadline (connect, write idle, response head, read idle, and the total), and a `Tick` past it fails the attempt;
- `Fail` carries a status only while no response byte has gone to the client; after that the client connection is closed.

```rust
use proxy_net::core::{Action, EndpointId, ErrorClass, Event, Exchange, Tick};
use proxy_net::{Pool, ProxyBuilder};

let config = ProxyBuilder::new().pool("app", Pool::new(["10.0.0.1:80"])).config()?;
let mut ex = Exchange::new(b"POST", true, config.pool("app").expect("pool"), Tick(0));
let _ = ex.on(Event::Start, Tick(0));
let _ = ex.on(Event::Picked(EndpointId(0)), Tick(0));
// A POST is never sent twice: a failed connect ends the exchange with 502.
assert_eq!(
    ex.on(Event::ConnectFailed(ErrorClass::Connect), Tick(1)),
    Action::Fail { class: ErrorClass::Connect, status: Some(502) },
);
# Ok::<(), proxy_net::ConfigError>(())
```

### `ErrorClass`, `Outcome`, `ForwardError`

*Shipped (0.3).* What ended an exchange.

| Class | Status | Retryable | Counts against health |
|---|---|---|---|
| `Connect`, `Tls` | 502 | yes | yes |
| `UpstreamReset` | 502 | yes (before any response byte) | yes |
| `UpstreamStatus` (a status in `Retry::on_status`) | 502 | yes | yes |
| `UpstreamProtocol`, `Desync` | 502 | no | yes |
| `Timeout(Connect)` | 504 | yes | yes |
| `Timeout(ResponseHead / ReadIdle / WriteIdle / Total)` | 504 | no | yes |
| `Limit(RequestBody)` | 413 | no | no |
| `Limit(ResponseBody / ResponseHead)` | 502 | no | no |
| `NoEndpoint`, `Breaker` | 503 | no | no |
| `Loop` | 508 | no | no |
| `Refused(Method)` | 405 | no | no |
| `Refused(NoRoute / UnknownHost)` | 404 | no | no |
| `Refused(AmbiguousPath / BodyNotAllowed / InvalidField)` | 400 | no | no |
| `ClientGone` | none | no | no |

`Outcome::from(class)` gives `Failure` or `Neutral` for `HealthTable::record`; a response is `Success`. `ForwardError` carries the class, the endpoint of the last attempt, and the attempt number; its `Display` never names an address.

```rust
use proxy_net::core::{EndpointId, ErrorClass, TimeoutKind};
assert_eq!(ErrorClass::Timeout(TimeoutKind::ResponseHead).status(), Some(504));
let e = proxy_net::ForwardError::new(ErrorClass::NoEndpoint, None, 1);
assert_eq!(e.to_string(), "no upstream is available (attempt 1)");
# let _ = EndpointId(0);
```

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

The locked shape of what follows 0.3. Names and signatures here change only with a migration note.

### 0.4.0: forwarding

```rust,ignore
pub fn proxy(listen, upstreams) -> Result<Running, ProxyError>;
impl ProxyBuilder {
    pub fn build(self) -> Result<Proxy, ConfigError>;
    pub fn observer(self, o: impl Observer) -> ProxyBuilder;
    pub fn hash_key(self, key: [u8; 16]) -> ProxyBuilder;   // the consistent-hashing key; all zero by default so nodes agree
}
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
