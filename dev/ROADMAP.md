# proxy-net - Roadmap

> Path from scaffold to a stable 1.0. Hard parts are front-loaded; each phase has hard exit criteria. One pull request per version, squash-merged after CI passes on every platform.
> Design and decisions: [`PLAN.md`](./PLAN.md). API contract: [`../docs/API.md`](../docs/API.md).
>
> **Anti-deferral rule:** no listed hard task moves to a later phase unless this file records the move and the reason.

## v0.1.0 - Scaffold (DONE, 2026-10-13)
- [x] Manifest, README, CHANGELOG, REPS, dual license, CI, deny, clippy, rustfmt, DIRECTIVES, ROADMAP, PLAN.

## v0.2.0 - Design lock (2026-11-19)
- [x] `Endpoint` and its parser; `EndpointAddr`.
- [x] `Route`, host and path patterns, the compiled `RouteTable` with shadowing refused at build.
- [x] `HeaderRule`, `HostHeader`, `PathPolicy` (settings and validation).
- [x] `Pool`, `Balance`, `HashKey`, `HealthCheck`, `Outlier`, `Retry`, `Timeouts`, `PoolLimits` (settings and validation).
- [x] `ProxyBuilder::{pool, route, unknown_host, limits, forwarded, strip_header, via, no_via, check, config}`, `Config`, `Limits`, `Forwarded`.
- [x] `ConfigError` with the item and reason of every refusal.
- [x] `docs/API.md` with the full API and the version of each item; README, PLAN, ROADMAP rewritten.
- [x] Tests: unit tests, `tests/config.rs`, `tests/routing.rs` (with a reference matcher).
- [x] Fuzz targets `endpoint`, `route_pattern`, `route_match`; CI fuzz smoke and `no_std` jobs.
- [ ] James approves `docs/API.md` (merging this version).

## v0.3.0 - Core
- [ ] `core::Tick`; balancers (round robin, least connections, P2C, Maglev with bounded load, random, first) with tiers.
- [ ] Health table: passive outlier detection, ejection with backoff, panic threshold.
- [ ] Retry eligibility and budget accounting.
- [ ] Head rewriting: hop-by-hop removal, forwarding set, rules, `Host`, path rewrites, `Forwarded` and `X-Forwarded-*`, `Via`, loop detection.
- [ ] Path safety checks; agreement checks (body on `GET`, `TRACE`).
- [ ] `Exchange` state machine; `ErrorClass`, `ForwardError`.
- [ ] Defence idea I-1.
- [ ] Tests: balance, health, forward heads, exchange, core-level attacks, property tests; fuzz `path_check`, `request_fields`, `exchange`.
- [ ] Exit: `core` builds for a `no_std` target; every 0.3 requirement tested.

## v0.4.0 - Forwarding
- [ ] Dependencies: http-net 0.10, parse-net 0.7, net-io 0.8, serve-net 0.8.
- [ ] `Connector`, `TcpConnector`, the connection pool, the upstream connection, the active health checker, slow start, breaker caps.
- [ ] `ProxyService`, `Proxy`, `Running`, Tier 1 `proxy`; streaming both ways; timeouts and retries; 100-continue, 1xx, trailers; `Observer`; health and stats snapshots.
- [ ] Defence ideas I-2 and I-3.
- [ ] Tests: real-socket proxying, socket-level attacks, loom; fuzz `upstream_response`; differential test.
- [ ] Exit: Tier 1 forwards real traffic on Linux, macOS, and Windows.

## v0.5.0 - Feature complete
- [ ] WebSocket bridge (`ws`), TLS to upstreams (`tls`), PROXY protocol to upstreams.
- [ ] Append mode for forwarding headers, `by` nodes, request ids.
- [ ] Live reload, draining, `Discovery` (static, DNS), the discovery allowlist.
- [ ] Defence idea I-4; `docs/ARCHITECTURE.md`.

## v0.6.0 - Optimisation
- [ ] Benchmarks against nginx, HAProxy, Envoy, and Pingora; counting-allocator proofs; `docs/PERFORMANCE.md`.
- [ ] The reactor path once serve-net G-1 and net-io G-11 have closed (see PLAN section 6). If they have not, the reactor path moves to the first version after they close; this is a hard external dependency, not a deferral by choice.
- [ ] Idea I-5 as an experiment; decide OD-3.

## v0.7.0 - Hardening
- [ ] 24-hour fuzz campaign per target, the full attack suite, the multi-node test, a 24-hour soak, idea I-6, `docs/SECURITY.md`.

## v0.8.0 - Alpha
- [ ] WASP's gateway uses proxy-net for its proxy and load-balancer roles end to end; `docs/OPERATIONS.md`, `docs/PLATFORM-NOTES.md`.

## v0.9.0 - Beta
- [ ] API frozen except for simplicity fixes; a 72-hour soak; an adversarial review with every finding fixed test-first.

## v1.0.0 - Stable
- [ ] The full audit and `docs/STABILITY-1.0.md`; a deployment in front of other servers used the API end to end.
