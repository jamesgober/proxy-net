//! # proxy_net
//!
//! A reverse proxy and load balancer for services on serve-net: upstream
//! pools with health checks, balancing policies, forwarding that re-serializes
//! every request and response, forwarding headers rebuilt from the real
//! client, and the limits and smuggling defences in place.
//!
//! ## Status
//!
//! `0.2.0` is the design lock. It ships the configuration surface, built and
//! checked: endpoints, routes and the compiled route table, pools and their
//! policies (balancing, health checks, passive health, retries, timeouts,
//! connection limits), header rules, path safety policies, forwarding
//! headers, and the proxy-wide limits. Nothing forwards traffic yet: the
//! sans-IO core (balancing, health, head rewriting, the forwarding state
//! machine) lands in `0.3.0`, and the serve-net service and the upstream
//! client in `0.4.0`. The full API, with the version each part ships in, is
//! in `docs/API.md` in the [repository](https://github.com/jamesgober/proxy-net).
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
//! | `std` | yes | `std` integration; without it the crate needs only `alloc` |

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

mod config;
pub mod core;
mod endpoint;
mod error;
mod pool;
mod rewrite;
mod route;
mod syntax;

pub use config::{
    ByNode, Config, DEFAULT_PSEUDONYM, Forwarded, ForwardedStyle, Limits, MAX_ROUTES, ProxyBuilder,
    UnknownHost,
};
pub use endpoint::{DEFAULT_MAX_CONNECTIONS, Endpoint, EndpointAddr};
pub use error::{ConfigError, ConfigItem, ConfigReason, EndpointErrorKind};
pub use pool::{
    Balance, CheckKind, HashKey, HealthCheck, MAX_ENDPOINTS, Outlier, Pool, PoolConfig, PoolLimits,
    Retry, Timeouts,
};
pub use rewrite::{FORWARDING_SET, HeaderRule, HostHeader, PathCheck, PathPolicy};
pub use route::Route;

// The README's and the API reference's examples run as doctests, so their
// claims are checked against the build.
#[cfg(all(doctest, feature = "std"))]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;

#[cfg(all(doctest, feature = "std"))]
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
