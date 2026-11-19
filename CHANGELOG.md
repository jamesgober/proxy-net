<h1 align="center">
    <img width="90px" height="auto" src="https://raw.githubusercontent.com/jamesgober/jamesgober/main/media/icons/hexagon-3.svg" alt="Triple Hexagon">
    <br><b>CHANGELOG</b>
</h1>
<p>
  All notable changes to <code>proxy-net</code> will be documented in this file. The format is based on <a href="https://keepachangelog.com/en/1.1.0/">Keep a Changelog</a>,
  and this project adheres to <a href="https://semver.org/spec/v2.0.0.html/">Semantic Versioning</a>.
</p>

---

## [Unreleased]

---

## [0.6.0] - 2026-11-19

Optimisation: the request path measured against other proxies and made
faster where the measurements pointed.

### Added

- `wss` upstreams through the WebSocket bridge: a WebSocket route to a pool
  with TLS is bridged, with the certificate verified before the upgrade is
  sent. tls-net's session is shared by the bridge's two threads under one
  lock.
- `Proxy::serve_with` and `Inbound`: workers, the per-source connection cap,
  and a connection cap for the client side. `ProxyError::Inbound` for a zero
  setting.
- `docs/PERFORMANCE.md`: proxy-net against nginx, HAProxy, and Envoy,
  allocations per request, the core's hot paths, permessage-deflate costs,
  and the adaptive-concurrency experiment.
- `benches/compare.rs` (a standard-library load generator and origin),
  `benches/compare/` (nginx, HAProxy, Envoy, and Pingora set up in front of
  the same origin), and the Bench workflow; `benches/core.rs` for route
  matching, endpoint picks, and head rewriting.
- `tests/alloc.rs`: allocations per proxied request (at most 12) and none in
  route matching, counted with a global allocator.
- Tests for `wss`: messages both ways (one larger than a TLS record), both
  directions at once, an upstream pushing to a silent client, and upstreams
  with an untrusted certificate or another name getting no upgrade.

### Changed

- A proxied keep-alive `GET` makes 10 heap allocations instead of 42: each
  worker keeps a few buffers between exchanges, and head rewriting reads the
  client's fields in place and writes values straight into the field list.
- The upstream connection pool no longer makes system calls under its lock;
  at 64 connections on Linux throughput rose from about 64,000 to 409,000
  requests per second.
- Route matching looks only at the routes that can match the request's
  host: 39 ns instead of 86 ns for 32 routes.
- The health checker no longer copies the configuration on every tick when
  no pool has discovery.
- `RequestParts::fields` is a `FieldList`.
- `Finished` borrows: `Finished<'a>` with `pool: &str` and
  `endpoint: Option<&EndpointAddr>`.
- With the `tls` feature, a pool's TLS settings are loaded even when a custom
  connector is set (the bridge uses them).

### Known gaps

- serve-net 0.8 on Windows can hold messages queued for a WebSocket client
  until its next ping when the client is sending at the same time; Linux
  delivers them. The pipelined bridge tests skip Windows.
- The reactor path waits for serve-net and net-io (see the roadmap).

---

## [0.5.0] - 2026-11-19

Feature complete: every feature planned for 1.0 is in.

### Added

- Live changes on `ProxyHandle`: `reload` replaces the whole configuration,
  `set_endpoints` replaces one pool's endpoints, and `drain_endpoint` takes
  an endpoint out of rotation while its exchanges finish. Endpoints present
  before and after keep their health, pooled connections, and slow-start
  ramp; a request whose head arrived before a change is routed again under
  the new configuration.
- Discovery: the `Discovery` trait, `StaticDiscovery`, `DnsDiscovery`,
  `Pool::discovery`, and `Pool::discovery_allow` with `Cidr` ranges. Without
  an allowlist, discovered loopback, link-local, unspecified, multicast, and
  broadcast addresses are refused. A failed or empty refresh keeps the last
  set and is reported.
- TLS to upstreams (feature `tls`): `UpstreamTls` (public or private roots,
  a server name, a client certificate), `Pool::tls`, and
  `client::TlsConnector` through tls-net.
- The PROXY protocol to upstreams: `ProxyProtocol` (v1, v2 with the
  authority and CRC32C TLVs) and `Pool::proxy_protocol`. Health checks send
  `LOCAL` or `UNKNOWN`; connections that carry a header are not reused.
- The WebSocket bridge (feature `ws`): on routes with `websocket(true)`,
  serve-net terminates the client's WebSocket and ws-net opens one to the
  upstream; messages are parsed and written again both ways, and the client
  gets its 101 only after the upstream's genuine 101.
- `Pool::slow_start(ramp)`: an endpoint that comes back from ejection, or
  that a live change adds, takes a growing share of its turns.
- `ProxyBuilder::request_id(name)`: a client's id is kept when it is a token
  of 1 to 64 bytes, and replaced with 32 hex characters otherwise.
- Append mode (`Forwarded::append_trusted`) is applied: the entries trusted
  proxies added are kept, and the socket peer is added.
- `Observer::response_outlier` (a response far larger than the route's
  usual) and `Observer::discovery_failed`.
- `EndpointHealth::ramp`, `Config::request_id`, `PoolConfig::{proxy_protocol,
  tls, slow_start, discovery_allow}`.
- `docs/ARCHITECTURE.md`.
- Tests: live changes, discovery and its allowlist, the PROXY protocol,
  request ids, slow start across reloads, the WebSocket bridge (messages
  both ways, a subprotocol, and fake upgrades answered 502), TLS settings
  and a plain upstream behind a TLS pool; a fuzz target for append mode and
  request ids.

### Changed

- `ProxyHandle::balancer` is replaced by `ProxyHandle::is_eligible`, and
  `Proxy::config` returns an owned `Config`: the running state can now be
  swapped, so a reference into it cannot be handed out.
- `Pool::slow_start` replaces the planned `Endpoint::slow_start`.
- An address range with bits set past its prefix (`10.1.0.0/8`) is refused.

### Known gaps

- A pool with TLS is not bridged for WebSocket yet (502); see the roadmap.
- serve-net cannot close a client's WebSocket from outside a callback, so an
  upstream close reaches the client at its next message or at the tunnel
  idle timeout.

---

## [0.4.0] - 2026-11-19

Forwarding over real sockets.

### Added

- `proxy_net::proxy(listen, upstreams)`: one line puts a listener in front of
  a set of servers.
- `ProxyBuilder::build` starts a `Proxy`: a balancer, health table, retry
  budget, and connection pool per pool, and one health checker thread.
  `Proxy::serve` runs it on serve-net; `Proxy::service` gives the serve-net
  `HttpService` to mount in a server of your own; `Running` drains or waits.
- `ProxyService`: routes and checks each request before admission (404, 421,
  400, 405, 508), holds a small request body before taking an upstream
  connection, streams a larger one, and streams the response back with
  backpressure.
- The upstream side, `proxy_net::client`: the `Connector` and `UpstreamIo`
  seam, `TcpConnector` over net-io, a connection pool per endpoint (a probe
  before every reuse, idle and age limits, fresh connections for
  non-idempotent requests, caps with a bounded wait), and `Forward`, which
  drives the core's exchange state machine against real connections.
- The active health checker: HTTP or TCP checks on a schedule with jitter,
  ejections ended on time, and idle connections closed past their limit.
- `Observer` with a method per event, `ProxyHandle::health` and `stats`
  snapshots, and `ProxyBuilder::hash_key` for consistent hashing.
- Bytes left after a complete upstream response close the connection and
  count against the endpoint (a desync tripwire).
- Socket tests and attack tests (smuggling vectors, requests hidden in
  bodies, slow clients), loom model checks of the health table and retry
  budget, and a fuzz target for whole exchanges against arbitrary upstream
  responses.

### Changed

- A failure while connecting (the request never left the proxy) is now
  retried for any method whose body is held; once a request may have
  reached the upstream, only idempotent methods are retried, as before.
- The upstream's `Date` field is dropped from responses (serve-net writes
  its own).

---

## [0.3.0] - 2026-11-19

The sans-IO core: every forwarding decision, without sockets, threads, or
clocks. Nothing forwards traffic over sockets yet.

### Added

- `core::Balancer`: smooth weighted round robin (a schedule built once and
  walked with one atomic counter), least connections, power of two choices,
  Maglev consistent hashing with a load bound, random, and first healthy;
  priority tiers and a panic threshold. Picks read atomics only.
- `core::HealthTable`: passive ejection after consecutive failures, for a
  time that grows with each ejection up to a cap, never more than the
  configured share of a pool; active check results with rise and fall
  counts; draining. Each endpoint's state is one word updated without locks.
- `core::RetryBudget` and `core::is_idempotent`.
- `core::check_request` and `core::check_path`: `CONNECT` and `TRACE`
  refused, a body on `GET`, `HEAD`, `OPTIONS`, `TRACE`, or `DELETE` refused,
  and ambiguous paths refused, each unless the route allows it.
- `core::request_fields` and `core::response_fields`: hop-by-hop fields and
  every field `Connection` names removed, the forwarding set removed and
  rebuilt (`Forwarded` and `X-Forwarded-*`), `Host` written by the route's
  rule, header rules applied, `Via` added, and a request whose `Via` names
  this proxy refused as a loop. `core::rewrite_target` applies prefix
  rewrites.
- `core::Exchange`: the forwarding state machine of one request, with
  retries only for idempotent requests whose body is held and before any
  response byte reached the client, back-off before retrying the same
  endpoint, and a deadline in every state.
- `core::ErrorClass`, `core::Outcome`, and `ForwardError`, with the status,
  retry, and health meaning of each class.
- `ProxyBuilder::frame_facts`: an optional `proxy-frame` field stating how
  the proxy framed each upstream request, so an upstream can detect a
  framing disagreement.
- Property tests of the exchange, path, and rewriting invariants, attack
  tests for the threats the core stops, and fuzz targets for path checks,
  head rewriting, and the exchange state machine.

### Changed

- The first dependency: `parse-net` 0.7 (first-party), for list parsing and
  writing `Forwarded`.
- The `no_std` build now needs 64-bit atomics; CI builds it for
  `aarch64-unknown-none` instead of `thumbv7em-none-eabihf`.

---

## [0.2.0] - 2026-11-19

The design lock. The configuration surface is built and checked; nothing
forwards traffic yet. `docs/API.md` holds the full API, with the version each
part ships in.

### Added

- `Endpoint` and `EndpointAddr`: `host:port`, `ip:port`, and `[ipv6]:port`,
  with weights, priority tiers, connection caps, and server names. Host names
  follow RFC 1123; IPv6 zones, userinfo, paths, and ports with a sign or a
  leading zero are refused.
- `Route`: host patterns (exact, `*.example.com`, addresses, `*`), path
  patterns (`path` for a segment prefix, `path("/x/")` for a prefix, `exact`),
  methods, the pool, prefix rewrites, `Host` handling, request and response
  header rules, a path safety policy, and the methods allowed a body.
- `core::RouteTable`: the compiled table. `find` matches a `Host` value, a
  path, and a method without allocating; a route that an earlier one shadows
  entirely is refused when the configuration is built.
- `Pool` with `Balance`, `HashKey`, `HealthCheck`, `Outlier`, `Retry`,
  `Timeouts`, and `PoolLimits`, each with documented defaults and ranges.
- `ProxyBuilder` and `Config`, `Limits`, `Forwarded`, `ByNode`,
  `UnknownHost`, `HeaderRule`, `HostHeader`, `PathPolicy`, and
  `FORWARDING_SET`.
- `ConfigError` with `ConfigItem` and `ConfigReason`: every refusal names the
  pool, endpoint, route, or setting, and what to change.
- Fuzz targets for endpoint parsing, route patterns, and route matching, with
  a 60-second run of each in CI, and a CI build for a target without `std`.

### Changed

- The crate builds without `std` when the default `std` feature is off; it
  then needs only `alloc`.

---

## [0.1.0] - 2026-10-13

The scaffold and the plan. No functional code yet.

### Added

- Crate manifest (edition 2024, MSRV 1.85, no dependencies), `Apache-2.0 OR MIT`
  license, README, REPS, CI (Linux, macOS, Windows on stable and 1.85; audit
  and deny), `deny.toml`, `clippy.toml`, `rustfmt.toml`, `.gitattributes`.
- `dev/PLAN.md` (purpose, scope, architecture, threats, ecosystem, open
  decisions, decision log), `dev/DIRECTIVES.md`, and `dev/ROADMAP.md`.

[Unreleased]: https://github.com/jamesgober/proxy-net/compare/v0.6.0...HEAD
[0.6.0]: https://github.com/jamesgober/proxy-net/compare/v0.5.0...v0.6.0
[0.5.0]: https://github.com/jamesgober/proxy-net/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/jamesgober/proxy-net/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/jamesgober/proxy-net/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/jamesgober/proxy-net/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/jamesgober/proxy-net/releases/tag/v0.1.0
