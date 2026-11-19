//! # proxy_net
//!
//! A reverse proxy and load balancer for services on serve-net: upstream
//! pools with health checks, balancing policies, forwarding that re-serializes
//! every request and response, forwarding headers rebuilt from the real
//! client, and the limits and smuggling defences in place.
//!
//! ## Status
//!
//! `0.5.0` is feature complete. With the default features, `proxy_net::proxy`
//! puts a listener in front of a set of servers in one line, and
//! `ProxyBuilder::build` starts a `Proxy` from pools, routes, and health
//! checks; its handle reloads the configuration, replaces endpoints, and
//! drains them while it runs. TLS to upstreams (feature `tls`), the PROXY
//! protocol, and the WebSocket bridge (feature `ws`) are in. The decisions live in [`core`] (sans-IO, no `std` needed); the
//! `client` module (feature `client`) drives them against real connections,
//! and `ProxyService` (feature `serve`) is the serve-net service. The full API,
//! with the version each part ships in, is in `docs/API.md` in the
//! [repository](https://github.com/jamesgober/proxy-net).
//!
//! ## Example
//!
//! ```
//! use proxy_net::{Balance, HealthCheck, Pool, ProxyBuilder, Route};
//! use proxy_net::core::RouteMatch;
//!
//! let config = ProxyBuilder::new()
//!     .pool("api", Pool::new(["10.0.0.1:8080", "10.0.0.2:8080"]).balance(Balance::LeastConnections))
//!     .pool("web", Pool::new(["10.0.1.1:8080"]).health(HealthCheck::http("/health")))
//!     .route(Route::host("api.example.com").path("/v1").to("api").strip_prefix("/v1"))
//!     .route(Route::any().to("web"))
//!     .config()?;
//!
//! match config.routes().find(b"api.example.com", b"/v1/users", b"GET") {
//!     RouteMatch::Route { route, .. } => {
//!         assert_eq!(config.pool_at(route.pool_index()).map(|p| p.name()), Some("api"));
//!     }
//!     other => panic!("{other:?}"),
//! }
//! # Ok::<(), proxy_net::ConfigError>(())
//! ```
//!
//! ## Features
//!
//! | Feature | Default | What it adds |
//! |---|---|---|
//! | `serve` | yes | the serve-net service, `Proxy::serve`, and `proxy` (implies `client`) |
//! | `client` | yes | the upstream side: connections, the pool, the forwarder, the health checker (implies `std`) |
//! | `std` | yes | `std` integration; without it the crate needs only `alloc` (and 64-bit atomics) |
//! | `tls` | no | TLS to upstreams through tls-net (implies `client`) |
//! | `ws` | no | the WebSocket bridge through ws-net (implies `serve`) |

#![cfg_attr(not(feature = "std"), no_std)]
#![doc(html_root_url = "https://docs.rs/proxy-net")]
#![cfg_attr(docsrs, feature(doc_cfg))]
#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![deny(unused_must_use)]
#![deny(unused_results)]
#![deny(clippy::unwrap_used)]
#![deny(clippy::expect_used)]
#![deny(clippy::todo)]
#![deny(clippy::unimplemented)]
#![deny(clippy::print_stdout)]
#![deny(clippy::print_stderr)]
#![deny(clippy::dbg_macro)]
#![deny(clippy::unreachable)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

extern crate alloc;

#[cfg(feature = "client")]
pub mod client;
mod config;
pub mod core;
mod endpoint;
mod error;
#[cfg(feature = "client")]
mod observe;
mod pool;
#[cfg(feature = "client")]
mod proxy;
mod rewrite;
mod route;
#[cfg(feature = "serve")]
mod serve;
mod syntax;
mod upstream;
#[cfg(feature = "ws")]
mod ws;

pub use config::{
    ByNode, Config, DEFAULT_PSEUDONYM, Forwarded, ForwardedStyle, Limits, MAX_ROUTES, ProxyBuilder,
    UnknownHost,
};
pub use endpoint::{DEFAULT_MAX_CONNECTIONS, Endpoint, EndpointAddr};
pub use error::{ConfigError, ConfigItem, ConfigReason, EndpointErrorKind, ForwardError};
pub use pool::{
    Balance, CheckKind, HashKey, HealthCheck, MAX_ENDPOINTS, Outlier, Pool, PoolConfig, PoolLimits,
    Retry, Timeouts,
};
pub use rewrite::{FORWARDING_SET, HeaderRule, HostHeader, PathCheck, PathPolicy};
pub use route::Route;
pub use upstream::{Cidr, ProxyProtocol, UpstreamRoots, UpstreamTls};

#[cfg(feature = "client")]
pub use observe::{Finished, NoObserver, Observer, ProxyStats};
#[cfg(feature = "client")]
pub use proxy::{EndpointHealth, HealthSnapshot, PoolHealth, Proxy, ProxyError, ProxyHandle};
#[cfg(feature = "serve")]
pub use serve::{Inbound, ProxyService, Running, proxy};

// The README's and the API reference's examples run as doctests, so their
// claims are checked against the build.
#[cfg(all(doctest, feature = "serve"))]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;

#[cfg(all(doctest, feature = "serve"))]
#[doc = include_str!("../docs/API.md")]
struct ApiDoctests;

/// Crate version string, populated by Cargo at build time.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::VERSION;

    #[test]
    fn test_version_matches_manifest() {
        assert_eq!(VERSION, env!("CARGO_PKG_VERSION"));
        assert_eq!(VERSION.split('.').count(), 3, "expected MAJOR.MINOR.PATCH");
    }

    #[test]
    fn test_public_types_are_send_and_sync() {
        fn check<T: Send + Sync>() {}
        check::<crate::Config>();
        check::<crate::ProxyBuilder>();
        check::<crate::Pool>();
        check::<crate::Route>();
        check::<crate::Endpoint>();
        check::<crate::ConfigError>();
        check::<crate::core::RouteTable>();
    }
}
