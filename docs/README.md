<div align="center">
        <img width="99" alt="Rust logo" src="https://raw.githubusercontent.com/jamesgober/rust-collection/72baabd71f00e14aa9184efcb16fa3deddda3a0a/assets/rust-logo.svg">
    <br>
    <h1>
        <strong>proxy-net</strong>
        <sup>
            <br>
            <sub>DOCUMENTATION</sub>
            <br>
        </sup>
    </h1>
</div>
<div align="center">
    <sup>
        <a href="../README.md" title="Project Home"><b>HOME</b></a>
        <span>&nbsp;│&nbsp;</span>
        <span>DOCS</span>
        <span>&nbsp;│&nbsp;</span>
        <a href="./API.md" title="API Reference"><b>API</b></a>
    </sup>
</div>
<br>

## Guides

- [API reference](./API.md): every public item, with examples and the version it ships in.
- [Architecture](./ARCHITECTURE.md): the layers, the threads, how a request moves through the proxy, and how live changes swap the running state.
- [Security](./SECURITY.md): the threat model, every threat with its defence and its test, where proxy-net is stricter than the RFCs, the settings that matter, and the hardening record.
- [Performance](./PERFORMANCE.md): proxy-net against nginx, HAProxy, and Envoy, allocations per request, the core's hot paths, and how each was measured.
- [Release notes](./release/): one file per release.
- [Changelog](../CHANGELOG.md)
- [Plan](../dev/PLAN.md): the design, the threats and their defences, the decision log.
- [Roadmap](../dev/ROADMAP.md): what each version delivers, with its exit criteria.
- [Directives](../dev/DIRECTIVES.md): the engineering standards and the definition of done.

The operations and platform guides arrive with 0.8, and `STABILITY-1.0.md` with 1.0.

## Help

- Issues: <https://github.com/jamesgober/proxy-net/issues>
- MSRV: Rust 1.85. The feature flags are `serve`, `client`, and `std` (on by default), and `tls` and `ws`; see
  [`Cargo.toml`](../Cargo.toml).

<br>

<div align="center">
  <h2></h2>
  <sup>COPYRIGHT <small>&copy;</small> 2026 <strong>JAMES GOBER.</strong></sup>
</div>
