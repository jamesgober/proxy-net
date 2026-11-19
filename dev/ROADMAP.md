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
- [x] James approves `docs/API.md` (merged as #2; GitHub release v0.2.0; not on crates.io, which waits for James).

## v0.3.0 - Core (2026-11-19)
- [x] `core::Tick`; balancers (round robin, least connections, P2C, Maglev with bounded load, random, first) with tiers.
- [x] Health table: passive outlier detection, ejection with backoff, panic threshold.
- [x] Retry eligibility and budget accounting.
- [x] Head rewriting: hop-by-hop removal, forwarding set, rules, `Host`, path rewrites, `Forwarded` and `X-Forwarded-*`, `Via`, loop detection.
- [x] Path safety checks; agreement checks (body on `GET`, `TRACE`, `CONNECT`).
- [x] `Exchange` state machine; `ErrorClass`, `ForwardError`.
- [x] Defence idea I-1 (`frame_facts`).
- [x] Tests: balance, health, forward heads, core-level attacks, property tests; fuzz `path_check`, `request_fields`, `exchange`.
- [x] Exit: `core` builds for a `no_std` target (`aarch64-unknown-none`, D-025); every 0.3 requirement tested.
- Moved to 0.4, with the reason in PLAN D-030: `ProxyBuilder::hash_key`.

## v0.4.0 - Forwarding (2026-11-19)
- [x] Dependencies: http-net 0.10, parse-net 0.7, net-io 0.8.1, serve-net 0.8.2.
- [x] `Connector`, `TcpConnector`, the connection pool, the upstream connection, the active health checker, breaker caps (endpoint caps with a bounded wait).
- [x] `ProxyBuilder::hash_key` (moved from 0.3, D-030).
- [x] `ProxyService`, `Proxy`, `Running`, Tier 1 `proxy`; streaming both ways; timeouts and retries; 100-continue (serve-net answers it), 1xx and trailers (dropped until serve-net G-2 and G-3); `Observer`; health and stats snapshots.
- [x] Defence ideas I-2 (desync tripwire) and I-3 (fresh connections for non-idempotent requests).
- [x] Tests: real-socket proxying, socket-level attacks, loom; fuzz `upstream_response`.
- [x] Exit: Tier 1 forwards real traffic on Linux, macOS, and Windows.
- Moved to 0.5, with the reason in PLAN D-042: slow start.
- Moved to 0.7, where idea I-6 builds it: the differential test against a second parser (the socket attack tests already parse everything the upstream receives with http-net).

## v0.5.0 - Feature complete (2026-11-19)
- [x] WebSocket bridge (`ws`), TLS to upstreams (`tls`), PROXY protocol to upstreams.
- [x] Slow start (moved from 0.4, D-042; built as ramps, D-051).
- [x] Append mode for forwarding headers, `by` nodes, request ids.
- [x] Live reload, draining, `Discovery` (static, DNS), the discovery allowlist.
- [x] Defence idea I-4; `docs/ARCHITECTURE.md`.
- [x] Tests: live changes, PROXY protocol, request ids, the bridge (T-7 and T-11), TLS settings; fuzz `forwarded_append`.
- Moved to 0.6, with the reason in PLAN D-048: `wss` upstreams through the bridge. It needs one thread to read and write the TLS stream, which is the reactor path 0.6 builds (or a split from tls-net, G-15). Plain `ws` upstreams are bridged now; a `wss` upstream answers 502, so nothing passes unencrypted by mistake.

## v0.6.0 - Optimisation (2026-11-19)
- [x] Benchmarks against nginx, HAProxy, Envoy, and Pingora (`benches/compare`, the Bench workflow, D-062); counting-allocator proofs (`tests/alloc.rs`: 42 allocations per request down to 10, bound 12); `docs/PERFORMANCE.md`.
- [x] Optimisation the benchmarks pointed to: buffers kept per worker (D-059), the connection pool's lock without system calls (D-060), and `Inbound` for the per-source cap that closed load-balancer traffic (D-061).
- [x] Idea I-5 as an experiment (D-063); OD-3 decided (D-064).
- [x] `wss` upstreams through the WebSocket bridge (moved from 0.5, D-048; built without the reactor, D-058).
- Moved, with the reason in PLAN D-065: the reactor path. serve-net G-1 and net-io G-11 are still open (serve-net 0.8.2 and net-io 0.8.1 are the newest on 2026-11-19); it comes in the first version after both close. This is a hard external dependency, not a deferral by choice.

## v0.7.0 - Hardening (2026-11-20)
- [ ] 24-hour fuzz campaign per target (nine targets, `differential` included).
- [ ] 24-hour soak on Linux and on Windows on serve-net 0.8.3 (`benches/soak.rs`, D-071, D-072).
- [x] The full attack suite: every threat T-1 to T-22 has a named test (`tests/attacks_suite.rs` adds T-8, T-9, T-12, T-16, T-18, T-19, T-21, T-22; D-069).
- [x] The multi-node test (`tests/multi_node.rs`: a chain, a loop, failover).
- [x] Idea I-6 (`tests/differential.rs`, the `differential` fuzz target; D-070).
- [x] `docs/SECURITY.md`.
- [x] Memory per active exchange: 27.3 KiB by default, 15.3 KiB with `Inbound::read_buffer(4096)`, under the 20 KiB target (`tests/memory.rs`, D-067).
- [x] Idea I-5 on sockets: it agrees with the simulation when clients back off on 503; shipped as an experimental opt-in, `PoolLimits::adaptive_concurrency` (D-068).
- [x] serve-net 0.8.3 fixes G-16: proxy-net moved to it and the two pipelined bridge tests run again (D-072).

## v0.8.0 - Alpha
- [ ] A gateway deployment uses proxy-net for its proxy and load-balancer roles end to end; `docs/OPERATIONS.md`, `docs/PLATFORM-NOTES.md`.

## v0.9.0 - Beta
- [ ] API frozen except for simplicity fixes; a 72-hour soak; an adversarial review with every finding fixed test-first.

## v1.0.0 - Stable
- [ ] The full audit and `docs/STABILITY-1.0.md`; a deployment in front of other servers used the API end to end.
