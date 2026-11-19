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

[Unreleased]: https://github.com/jamesgober/proxy-net/compare/v0.3.0...HEAD
[0.3.0]: https://github.com/jamesgober/proxy-net/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/jamesgober/proxy-net/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/jamesgober/proxy-net/releases/tag/v0.1.0
