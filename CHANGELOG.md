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

[Unreleased]: https://github.com/jamesgober/proxy-net/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/jamesgober/proxy-net/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/jamesgober/proxy-net/releases/tag/v0.1.0
