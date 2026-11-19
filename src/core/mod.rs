//! The sans-IO core: decisions without sockets, threads, or clocks.
//!
//! Everything here takes facts as input and returns decisions; it needs only
//! `alloc`, so it builds without the standard library. In `0.2.0` it holds the
//! compiled route table; balancing, health, head rewriting, and the forwarding
//! state machine follow in `0.3.0`.

pub(crate) mod route;

pub use route::{CompiledRoute, PathRewrite, RouteMatch, RouteTable};
