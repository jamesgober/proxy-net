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
> and examples. **Version `0.5.0`, feature complete.** This file is the API
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
  - [`proxy`](#proxy)
- [The running proxy](#the-running-proxy)
  - [`ProxyBuilder::build`, `observer`, `connector`](#proxybuilderbuild-observer-connector)
  - [`Proxy`](#proxy-1)
  - [`Running`](#running)
  - [`ProxyHandle`, `HealthSnapshot`, `ProxyStats`](#proxyhandle-healthsnapshot-proxystats)
  - [`Observer`, `NoObserver`, `Finished`](#observer-noobserver-finished)
  - [`ProxyService`](#proxyservice)
  - [`ProxyError`](#proxyerror)
- [Live changes](#live-changes)
  - [`reload`, `set_endpoints`, `drain_endpoint`](#reload-set_endpoints-drain_endpoint)
  - [`Discovery`, `StaticDiscovery`, `DnsDiscovery`](#discovery-staticdiscovery-dnsdiscovery)
- [Upstream protocols](#upstream-protocols)
  - [`ProxyProtocol`](#proxyprotocol)
  - [`UpstreamTls`, `UpstreamRoots`, `TlsConnector`](#upstreamtls-upstreamroots-tlsconnector)
  - [The WebSocket bridge](#the-websocket-bridge)
- [The upstream side (`proxy_net::client`)](#the-upstream-side-proxy_netclient)
  - [`Connector`, `UpstreamIo`, `TcpConnector`](#connector-upstreamio-tcpconnector)
  - [`Forward`, `RequestParts`, `ResponseStart`, `Failure`](#forward-requestparts-responsestart-failure)
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
  - [`Cidr`](#cidr)
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
- [Changes from the planned 0.5 shape](#changes-from-the-planned-05-shape)

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
| 0.4.0 | forwarding over real sockets: the serve-net service, the upstream client and connection pool, active health checks, Tier 1 (*shipped*) |
| 0.5.0 | WebSocket bridge, TLS and the PROXY protocol to upstreams, live reload, draining, discovery, slow start, request ids (*shipped*) |

### Features

| Feature | Default | What it adds |
|---|---|---|
| `std` | yes | `std` integration. Without it the crate needs only `alloc`. |
| `client` | yes | the upstream side over net-io and http-net: connections, the pool, the forwarder, the health checker |
| `serve` | yes | the serve-net service, `Proxy::serve`, and Tier 1 |
| `tls` | no (0.5) | TLS to upstreams through tls-net (aws-lc-rs as the crypto provider, Mozilla roots built in) |
| `ws` | no (0.5) | the WebSocket bridge through ws-net; turns on `serve` |

---

## Tier 1

### `proxy`

*Shipped (0.4, feature `serve`).* Listens on `listen` and forwards every request to the given endpoints: one pool, round robin, passive health, the default limits and timeouts, and forwarding headers in both styles.

```rust,ignore
pub fn proxy<A, I, S>(listen: A, upstreams: I) -> Result<Running, ProxyError>
where
    A: std::net::ToSocketAddrs,
    I: IntoIterator<Item = S>,
    S: AsRef<str>;
```

```rust,no_run
let running = proxy_net::proxy("0.0.0.0:8080", ["10.0.0.1:80", "10.0.0.2:80"])?;
running.wait();
# Ok::<(), proxy_net::ProxyError>(())
```

Errors: `ProxyError::Config` (an endpoint does not parse), `ProxyError::Serve` (the listener could not be bound).

---

## The running proxy

### `ProxyBuilder::build`, `observer`, `connector`

*Shipped (0.4, feature `client`).*

```rust,ignore
impl ProxyBuilder {
    pub fn build(self) -> Result<Proxy, ProxyError>;
    pub fn observer(self, observer: impl Observer) -> ProxyBuilder;
    pub fn connector(self, connector: impl Connector) -> ProxyBuilder;   // default TcpConnector
}
```

`build` checks the configuration as `config` does, then starts the running state: a balancer, a health table, a retry budget, and a connection pool per pool, and one health checker thread. The checker runs the active checks, ends ejections whose time has come, and closes idle connections past `PoolLimits::idle_timeout`; it sleeps until the next one is due.

```rust,no_run
let proxy = proxy_net::ProxyBuilder::new()
    .pool("app", proxy_net::Pool::new(["127.0.0.1:3000", "127.0.0.1:3001"]))
    .build()?;
let running = proxy.serve("127.0.0.1:8080")?;
running.wait();
# Ok::<(), proxy_net::ProxyError>(())
```

Errors: `ProxyError::Config`; `ProxyError::Io` when the checker thread cannot start.

### `Proxy`

*Shipped (0.4, feature `client`; `service` and `serve` with `serve`).*

```rust,ignore
impl Proxy {
    pub fn handle(&self) -> ProxyHandle;
    pub fn config(&self) -> Config;                                         // the current one (0.5: owned)
    pub fn service(&self) -> ProxyService;                                  // feature `serve`
    pub fn serve<A: ToSocketAddrs>(self, listen: A) -> Result<Running, ProxyError>; // feature `serve`
    pub fn forward(&self, route: usize, parts: RequestParts) -> Option<Forward>;
    pub fn hash(&self, pool: usize, bytes: &[u8]) -> Option<u64>;
}
```

- `serve` starts a serve-net server on `listen` with 256 blocking workers and the body limit raised to `Limits::max_request_body`, and mounts the proxy's service.
- `service` gives the serve-net `HttpService` to mount in a server of your own. Give that server blocking workers (`Workers::blocking`) and a body limit at least `Limits::max_request_body`: until serve-net can park a streamed reply, the proxy waits on the upstream inside the service.
- `forward` starts one exchange for another inbound engine; see [`Forward`](#forward-requestparts-responsestart-failure).
- Dropping the `Proxy` stops the checker and closes the idle connections.

### `Running`

*Shipped (0.4, feature `serve`).* The proxy serving a listener.

```rust,ignore
impl Running {
    pub fn local_addr(&self) -> Option<SocketAddr>;
    pub fn handle(&self) -> ProxyHandle;
    pub fn drain(self, within: Duration) -> serve_net::DrainReport;  // stop accepting, finish exchanges, stop
    pub fn wait(self) -> serve_net::DrainReport;                    // until a signal or a drain from elsewhere
}
```

### `ProxyHandle`, `HealthSnapshot`, `ProxyStats`

*Shipped (0.4, feature `client`; `is_eligible` and `ramp` 0.5).* Snapshots, read without locks except the connection counts (one short lock per endpoint). The live changes are in [Live changes](#live-changes).

```rust,ignore
impl ProxyHandle {
    pub fn health(&self) -> HealthSnapshot;
    pub fn stats(&self) -> ProxyStats;
    pub fn is_eligible(&self, pool: usize, id: EndpointId) -> bool;   // replaces 0.4's balancer()
}
pub struct HealthSnapshot { pub pools: Vec<PoolHealth> }
pub struct PoolHealth { pub name: Box<str>, pub endpoints: Vec<EndpointHealth> }
pub struct EndpointHealth { pub addr: EndpointAddr, pub state: EndpointState, pub eligible: bool,
                            pub active: u32, pub open: u32, pub idle: u32, pub latency_ns: u64,
                            pub ramp: u32 }   // slow-start share in percent, 100 when not ramping
pub struct ProxyStats { pub exchanges: u64, pub succeeded: u64, pub failed: u64, pub retries: u64,
                        pub refused: u64, pub desyncs: u64 }
```

```rust,no_run
let running = proxy_net::proxy("127.0.0.1:8080", ["127.0.0.1:3000"])?;
for pool in running.handle().health().pools {
    for e in pool.endpoints {
        println!("{} {}: {:?}, {} active, {} idle", pool.name, e.addr, e.state, e.active, e.idle);
    }
}
# Ok::<(), proxy_net::ProxyError>(())
```

### `Observer`, `NoObserver`, `Finished`

*Shipped (0.4, feature `client`).* One method per event, each with an empty default; methods run on the exchange's thread, so keep them short.

| Method | When |
|---|---|
| `route_matched(route, pool)` | a request matched a route |
| `endpoint_picked(pool, endpoint, attempt)` | the balancer picked an endpoint |
| `connected(endpoint, took, reused)` | a connection is ready, pooled or new |
| `retry(endpoint, class, attempt)` | an attempt failed and the exchange retries |
| `response_head(endpoint, status, waited)` | the response head arrived |
| `finished(&Finished)` | an exchange ended (each exactly once) |
| `health_changed(pool, endpoint, from, to)` | an endpoint's health changed |
| `refused(class)` | a request was refused before forwarding |
| `desync(endpoint, leftover)` | an upstream left bytes after a complete response |
| `response_outlier(route, bytes, mean)` | 0.5: a response far larger than the route's usual (see below) |
| `discovery_failed(pool, why)` | 0.5: a discovery source failed, or gave nothing inside the allowlist; the pool keeps its endpoints |

`response_outlier` reports, never blocks. Each route keeps a moving mean of its response sizes (weight 1/32); once a route has 100 responses, one over 8 times the mean and over 64 KiB is reported. It costs three relaxed atomic operations per exchange.

`Finished` carries the route, pool, endpoint, status, failure class, attempts, bytes up and down, and the time it took.

```rust
use std::sync::atomic::{AtomicU64, Ordering};
use proxy_net::{EndpointAddr, Observer};

#[derive(Default)]
struct Retries(AtomicU64);
impl Observer for Retries {
    fn retry(&self, _e: &EndpointAddr, _c: proxy_net::core::ErrorClass, _attempt: u8) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
}
let builder = proxy_net::ProxyBuilder::new().observer(Retries::default());
# let _ = builder;
```

### `ProxyService`

*Shipped (0.4, feature `serve`).* The serve-net `HttpService`.

- `head`: matches the route (404, or `UnknownHost`'s status), refuses a loop (508), runs `check_request` (400 or 405), and asks serve-net to stream the body.
- `call`: holds the first `Limits::request_buffer` bytes of the body, then forwards; when the body is complete, waits for the response head (with retries), then relays the body in parts of up to `Limits::relay_buffer`, called again only once the client took the last part.
- A failure before any response byte answers with its status (`Retry-After: 1` with 503); after that, the client connection is closed.
- The upstream's `Date` is dropped (serve-net writes its own), and a response with more than 64 fields is answered 502 (serve-net 0.8 writes at most 64).
- 0.5: a request whose head arrived before a live change is routed again under the new configuration when its body is forwarded, with the route, loop, and request checks run again, so one request is never judged by two configurations.
- 0.5, feature `ws`: a WebSocket upgrade on a route with `websocket(true)` goes to [the bridge](#the-websocket-bridge); on any other route an upgrade is refused.

### `ProxyError`

*Shipped (0.4).* `Config(ConfigError)`, `Serve(serve_net::ServeError)` (feature `serve`), `Io(std::io::Error)`.

---

## Live changes

### `reload`, `set_endpoints`, `drain_endpoint`

*Shipped (0.5, feature `client`).*

```rust,ignore
impl ProxyHandle {
    pub fn reload(&self, builder: ProxyBuilder) -> Result<(), ConfigError>;
    pub fn set_endpoints(&self, pool: &str, endpoints: &[Endpoint]) -> Result<(), ConfigError>;
    pub fn drain_endpoint(&self, pool: &str, endpoint: &EndpointAddr) -> bool;
}
```

- `reload` checks the new configuration (asking discovery sources again), builds its running state beside the current one, and swaps it in. An endpoint in both (same pool name and address) keeps its health, its pooled connections, and its slow-start ramp; one that is gone takes nothing new and finishes what it has. On an error the running configuration is unchanged.
- `set_endpoints` replaces one pool's endpoints the same way, without asking discovery. Errors: `UnknownPool`, or the new configuration's error.
- `drain_endpoint` takes an endpoint out of rotation: it takes no new exchanges, finishes the ones in flight, and its idle connections close. It stays draining across reloads. `false` when there is no such endpoint.
- The observer, the connector, and the counters stay across changes. A request in flight finishes on the configuration it started with.

```rust,no_run
let running = proxy_net::proxy("127.0.0.1:8080", ["127.0.0.1:3000"])?;
let handle = running.handle();
handle.set_endpoints("default", &[proxy_net::Endpoint::parse("127.0.0.1:3001")?])?;
handle.reload(proxy_net::ProxyBuilder::new().pool("default", proxy_net::Pool::new(["127.0.0.1:3002"])))?;
# Ok::<(), Box<dyn std::error::Error>>(())
```

### `Discovery`, `StaticDiscovery`, `DnsDiscovery`

*Shipped (0.5, feature `client`).*

```rust,ignore
pub trait Discovery: Send + Sync + 'static {
    fn endpoints(&self) -> Result<Vec<Endpoint>, DiscoveryError>;
    fn refresh_every(&self) -> Option<Duration> { None }   // None: asked at build and reload only
}
pub struct StaticDiscovery;   // StaticDiscovery::new(Vec<Endpoint>)
pub struct DnsDiscovery;      // DnsDiscovery::new(host, port, refresh): every address the name resolves to
pub struct DiscoveryError;    // DiscoveryError::new(message), message()
impl Pool {
    pub fn discovery(self, source: impl Discovery) -> Pool;
    pub fn discovery_allow(self, ranges: &[&str]) -> Pool;   // CIDR ranges
}
```

- A pool with a discovery source may list no endpoints of its own. Its endpoints are what the source gives at build, and at each refresh from the health checker thread. A refresh that changes nothing changes nothing.
- The allowlist (T-20): with ranges set, only addresses inside them are kept, and no host names (a name cannot be checked). Without ranges, every address is kept except loopback, link-local (where cloud metadata services live), unspecified, multicast, and broadcast; name those ranges to allow them.
- A source that fails, or gives nothing inside the allowlist, leaves the pool as it was and is reported through `Observer::discovery_failed`. A pool never empties itself on a bad answer.

```rust
use std::time::Duration;
use proxy_net::client::DnsDiscovery;
let pool = proxy_net::Pool::new(Vec::<&str>::new())
    .discovery(DnsDiscovery::new("app.internal", 8080, Duration::from_secs(10)))
    .discovery_allow(&["10.0.0.0/8"]);
# let _ = pool;
```

Errors: a range that does not parse refuses the build (`Inconsistent`).

---

## Upstream protocols

### `ProxyProtocol`

*Shipped (0.5).*

```rust,ignore
pub enum ProxyProtocol { Off, V1, V2 { authority: bool, crc: bool } }
impl ProxyProtocol { pub fn v2() -> ProxyProtocol; pub fn is_on(self) -> bool; }
impl Pool { pub fn proxy_protocol(self, version: ProxyProtocol) -> Pool; }
```

The header goes at the start of each upstream connection, written by net-io's encoders: the client's address and the listener's. `v2()` adds the request's `Host` as the authority TLV and a CRC32C. Health checks send a `LOCAL` (v2) or `UNKNOWN` (v1) header. With the header on, a connection is never reused, since it names one client.

```rust
use proxy_net::{Pool, ProxyProtocol};
let pool = Pool::new(["10.0.0.1:80"]).proxy_protocol(ProxyProtocol::v2());
# let _ = pool;
```

### `UpstreamTls`, `UpstreamRoots`, `TlsConnector`

*Shipped (0.5; connecting needs feature `tls`).*

```rust,ignore
impl UpstreamTls {
    pub fn new() -> UpstreamTls;                                        // public roots, SNI from each endpoint
    pub fn roots_pem(self, pem: &[u8]) -> UpstreamTls;                  // trust only these roots
    pub fn server_name(self, name: &str) -> UpstreamTls;                // one SNI and name for every endpoint
    pub fn client_certificate(self, chain_pem: &[u8], key_pem: &[u8]) -> UpstreamTls;
    pub fn roots(&self) -> &UpstreamRoots;
    pub fn server_name_value(&self) -> Option<&str>;
    pub fn client_certificate_value(&self) -> Option<(&[u8], &[u8])>;
}
pub enum UpstreamRoots { Public, Pem(Box<[u8]>) }
impl Pool { pub fn tls(self, tls: UpstreamTls) -> Pool; }
// proxy_net::client, feature `tls`
impl TlsConnector { pub fn new(tls: &UpstreamTls) -> Result<TlsConnector, ConfigReason>; }
```

- The certificate is always verified: against the Mozilla roots, or only the PEM roots given. To pin, trust a private CA that signs nothing else; tls-net has no SPKI pins yet.
- The name checked is `server_name` when set, otherwise the endpoint's host name, otherwise its address. ALPN offers `http/1.1`.
- A pooled TLS connection is reused only when its probe says it is open with nothing waiting.
- A pool with `tls` and no `tls` feature, roots that do not parse, or a client certificate that does not load refuse the build.

```rust
use proxy_net::{Pool, UpstreamTls};
let pool = Pool::new(["api.internal:443"]).tls(UpstreamTls::new().server_name("api.internal"));
# let _ = pool;
```

### The WebSocket bridge

*Shipped (0.5, feature `ws`).* Turned on per route with `Route::websocket(true)`.

- serve-net terminates the client's WebSocket; ws-net opens a new one to the upstream, offering the client's subprotocols and no extensions. Every message is parsed on one side and written again on the other, so no frame passes through as raw bytes.
- An upgrade gets the same request checks as any request (path policy and methods) before anything is opened.
- The client gets its 101 only after the upstream's genuine 101 with the accept key for the key the proxy sent (T-11). Anything else answers 502 and no tunnel opens.
- The upstream request's fields are rewritten as for any request (forwarding headers, rules, `Host`), and the PROXY header is sent when the pool has one.
- The subprotocol the upstream chose goes to the client. serve-net keeps these names for the life of the process, so the bridge accepts at most 64 distinct names; past that an upgrade answers 502.
- A pool with `tls` is not bridged in 0.5 (502): the bridge reads and writes the upstream from two threads, and a TLS stream cannot be split.
- serve-net cannot yet close a client's WebSocket from outside a callback, so an upstream close reaches the client at its next message, or when the tunnel idle timeout ends the connection.

---

## The upstream side (`proxy_net::client`)

Feature `client`.

### `Connector`, `UpstreamIo`, `TcpConnector`

*Shipped (0.4).* The transport seam.

```rust,ignore
pub trait Connector: Send + Sync + 'static {
    fn connect(&self, endpoint: &EndpointAddr, prelude: &[u8], deadline: Instant)
        -> Result<Box<dyn UpstreamIo>, ErrorClass>;
}
pub trait UpstreamIo: Send + 'static {
    fn send(&mut self, bytes: &[u8], deadline: Instant) -> Result<(), ErrorClass>;
    fn recv(&mut self, buf: &mut [u8], deadline: Instant, kind: TimeoutKind) -> Result<usize, ErrorClass>;
    fn is_reusable(&mut self) -> bool;   // probe before reuse
    fn close(&mut self);
}
```

`TcpConnector` connects through net-io (the system resolver for host names, happy eyeballs when a name has several addresses) with net-io's own phase timeouts out of the way, so every call's deadline decides; the minimum transfer rate is off, since an upstream may trickle a long response. `prelude` is sent first (the PROXY header, 0.5).

```rust
use std::time::Instant;
use proxy_net::client::{Connector, UpstreamIo};
use proxy_net::core::{ErrorClass, TimeoutKind};
use proxy_net::EndpointAddr;

/// Answers every request with a fixed response (for tests).
struct Fixed;
struct FixedConn(Vec<u8>);
impl UpstreamIo for FixedConn {
    fn send(&mut self, _b: &[u8], _d: Instant) -> Result<(), ErrorClass> { Ok(()) }
    fn recv(&mut self, buf: &mut [u8], _d: Instant, _k: TimeoutKind) -> Result<usize, ErrorClass> {
        let n = self.0.len().min(buf.len());
        buf[..n].copy_from_slice(&self.0[..n]);
        self.0.drain(..n);
        Ok(n)
    }
    fn is_reusable(&mut self) -> bool { false }
    fn close(&mut self) {}
}
impl Connector for Fixed {
    fn connect(&self, _e: &EndpointAddr, _p: &[u8], _d: Instant) -> Result<Box<dyn UpstreamIo>, ErrorClass> {
        Ok(Box::new(FixedConn(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok".to_vec())))
    }
}
let proxy = proxy_net::ProxyBuilder::new()
    .pool("app", proxy_net::Pool::new(["10.0.0.1:80"]))
    .connector(Fixed)
    .build()?;
let mut forward = proxy
    .forward(0, proxy_net::client::RequestParts { method: "GET".into(), target: "/".into(), ..Default::default() })
    .expect("route 0");
forward.push_body(b"", true).expect("body");
let head = forward.response().expect("response");
assert_eq!(head.status, 200);
let mut body = Vec::new();
while !forward.read_body(&mut body, 4096).expect("body") {}
assert_eq!(body, b"ok");
# Ok::<(), proxy_net::ProxyError>(())
```

### `Forward`, `RequestParts`, `ResponseStart`, `Failure`

*Shipped (0.4).* One request being forwarded, driven by the caller's thread (it blocks on the upstream).

```rust,ignore
pub struct RequestParts { pub method: Box<str>, pub target: Box<str>, pub host: Box<[u8]>,
                          pub fields: Vec<(Box<str>, Box<[u8]>)>, pub client: Option<SocketAddr>,
                          pub https: bool, pub body: BodyFacts, pub key: Option<u64>,
                          pub peer: Option<SocketAddr>, pub hops: u8,          // 0.5, append mode
                          pub request_id: Option<Box<[u8]>> }                  // 0.5
impl Forward {
    pub fn push_body(&mut self, bytes: &[u8], last: bool) -> Result<(), Failure>;
    pub fn response(&mut self) -> Result<ResponseStart, Failure>;
    pub fn read_body(&mut self, out: &mut Vec<u8>, max: usize) -> Result<bool, Failure>; // true: ended
    pub fn abandon(&mut self);   // the client went away
}
pub struct ResponseStart { pub status: u16, pub fields: FieldList, pub has_body: bool }
pub struct Failure { pub class: ErrorClass, pub status: Option<u16> }
```

- The body: the first `Limits::request_buffer` bytes are held before any upstream connection is taken. A body that ends within that is replayable, so the exchange may retry; a larger one goes upstream at once and streams, and is not retried.
- Connections: a pooled connection is reused only after its probe says it is open with nothing waiting, while it is younger than `PoolLimits::idle_timeout` and has carried fewer than `max_exchanges_per_connection`. A non-idempotent request takes only a connection used within `fresh_for`, so the keep-alive close race cannot lose it.
- When every connection place of an endpoint is taken, a request waits up to `pending_timeout` (at most `max_pending` waiters), then fails with `Breaker` (503).
- Bytes left after a complete response close the connection, count as a desync, and count against the endpoint's health.
- Dropping a `Forward` that did not finish abandons it: its connection is closed, never reused.

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
    pub fn hash_key(self, key: [u8; 16]) -> ProxyBuilder; // 0.4.0
    pub fn request_id(self, name: &str) -> ProxyBuilder;  // 0.5.0
    pub fn check(&self) -> Result<(), ConfigError>;
    pub fn config(self) -> Result<Config, ConfigError>;
    // 0.4.0, feature `client`: build, observer, connector (see The running proxy)
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
| `hash_key` | the key consistent hashing uses for request keys; all zero by default, so every node agrees with no setting; set the same secret on every node to make the mapping unpredictable from outside |
| `frame_facts` | add a `proxy-frame` field stating how the proxy framed each upstream request (`length=N`, `chunked`, or `none`, and the field count), so an upstream that reads it can detect a framing disagreement; off by default. A client's `proxy-frame` is always removed |
| `request_id` | 0.5: the field that carries a request id. A client's value is kept when it is a token of 1 to 64 bytes; otherwise the proxy writes 32 hex characters, unique per process (not secret). Off by default |
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

*Shipped.* The validated configuration. Immutable; a reload (0.5) replaces it whole.

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
    pub fn hash_key(&self) -> [u8; 16];
    pub fn request_id(&self) -> Option<&str>;   // 0.5
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

`append_trusted` (0.5): when the inbound side walked trusted proxies to find the client, the last that many elements of the incoming `Forwarded` and `X-Forwarded-For` are kept and the socket peer is added; nothing a client wrote before them survives. A chain that does not parse, or a direct client, is replaced as in the default mode.

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

*Shipped (matching and validation); rewrites applied from 0.3; `websocket` bridged from 0.5.*

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
    pub fn proxy_protocol(self, version: ProxyProtocol) -> Pool;   // 0.5.0
    pub fn tls(self, tls: UpstreamTls) -> Pool;                    // 0.5.0
    pub fn slow_start(self, ramp: Duration) -> Pool;               // 0.5.0
    pub fn discovery(self, source: impl Discovery) -> Pool;        // 0.5.0, feature `client`
    pub fn discovery_allow(self, ranges: &[&str]) -> Pool;         // 0.5.0
}
```

`slow_start` (0.5): an endpoint that comes back from ejection or an unhealthy state, or that a live change adds to a running pool, takes a growing share of its turns over `ramp`: a tenth at first, all of them at the end. Round robin, least connections, and power of two choices honour it; consistent hashing does not (it would move keys). At most one hour; 0 (the default) is off.

```rust,ignore
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

Errors (on `Pool(name)` or `Endpoint { pool, index }`): `PoolName`, `NoEndpoints` (not raised for a pool with discovery), `DuplicateEndpoint`, `EndpointSyntax`, `Weight`, `Limit` (also `slow_start` over an hour), `HeaderName`, `PathPattern`, `HeaderValue`, `Inconsistent` (TLS settings, an allowlist range).

### `PoolConfig`

*Shipped.* A checked pool: `name()`, `endpoints()`, `balance()`, `health()`, `outlier()`, `retry()`, `timeouts()`, `limits()`; 0.5: `proxy_protocol()`, `tls()`, `slow_start()`, `discovery_allow()`.

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

### `Cidr`

*Shipped (0.5).* An address range, parsed from `10.0.0.0/8` or `2001:db8::/32` (a bare address is one host), with `contains(ip)`. Bits past the prefix must be zero.

```rust
let range: proxy_net::Cidr = "10.0.0.0/8".parse()?;
assert!(range.contains("10.1.2.3".parse()?));
# Ok::<(), Box<dyn std::error::Error>>(())
```

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
    pub fn with_key(pool: &PoolConfig, key: [u8; 16]) -> Balancer;
    pub fn key(bytes: &[u8]) -> u64;                       // hash of a request key, default key
    pub fn hash(&self, bytes: &[u8]) -> u64;               // hash of a request key, this balancer's key
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

- a retry is offered only when the body is held (`replayable`), before any response byte was relayed, after a retryable failure (connect, TLS, a reset before the response, a connect timeout, or a status in `Retry::on_status`), and within `Retry::attempts`; the caller asks the budget and answers `RetryGranted` or `RetryDenied`;
- a failure while connecting is retried for any method, since the request never left the proxy (0.4); once the request may have reached the upstream, only idempotent methods are retried;
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
let _ = ex.on(Event::Connected, Tick(0));
let _ = ex.on(Event::Sent, Tick(0));
// A POST is never sent twice: a reset after it went out ends the exchange with 502.
assert_eq!(
    ex.on(Event::UpstreamFailed(ErrorClass::UpstreamReset), Tick(1)),
    Action::Fail { class: ErrorClass::UpstreamReset, status: Some(502) },
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

## Changes from the planned 0.5 shape

0.5 ships every planned item. Where the shipped shape differs from the one locked in 0.2:

| Planned | Shipped | Why |
|---|---|---|
| `Endpoint::slow_start` | `Pool::slow_start(ramp)` | one ramp per pool is what deployments set, and it applies to endpoints that discovery adds |
| `ProxyProtocol::V2 { authority, alpn, unique_id, crc }` | `V2 { authority, crc }` | the proxy has no ALPN or unique id to send for plain upstreams; the enum is `#[non_exhaustive]`, so they can join later |
| `UpstreamTls` pinning | trust a private CA with `roots_pem` | tls-net has no SPKI pins on the client yet |
| `ProxyHandle::balancer(pool) -> Option<&Balancer>` (0.4) | `is_eligible(pool, id)` | the balancer now lives in the swappable running state, so a reference to it cannot outlive a reload |
| `Proxy::config() -> &Config` (0.4) | `Proxy::config() -> Config` | the same reason: the current configuration is a snapshot |
