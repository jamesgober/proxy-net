//! The sans-IO core: decisions without sockets, threads, or clocks.
//!
//! Everything here takes facts and time ([`Tick`]) as input and returns
//! decisions; it needs only `alloc` and 64-bit atomics, so it builds without
//! the standard library. It holds the compiled route table, the balancers,
//! endpoint health, the retry budget, the request checks, head rewriting,
//! and [`Exchange`], the forwarding state machine. The upstream client and
//! the serve-net service that drive it arrive in `0.4.0`.

mod adaptive;
mod atomic;
mod balance;
mod budget;
mod class;
mod exchange;
mod fields;
mod forward;
mod hash;
mod health;
mod path;
pub(crate) mod route;
mod tick;

pub use balance::Balancer;
pub use budget::{RetryBudget, is_idempotent};
pub use class::{ErrorClass, LimitKind, Outcome, RefusalKind, TimeoutKind};
pub use exchange::{Action, Event, Exchange};
pub use fields::FieldList;
#[cfg(feature = "client")]
pub(crate) use fields::FieldSource;
pub use forward::{
    BodyFacts, FRAME_FIELD, ForwardFacts, detect_loop, request_fields, response_fields,
    rewrite_target,
};
#[cfg(feature = "client")]
pub(crate) use forward::{request_fields_in, response_fields_in};
pub use health::{EndpointId, EndpointState, HealthTable, Transition};
pub use path::{check_path, check_request};
pub use route::{CompiledRoute, PathRewrite, RouteMatch, RouteTable};
pub use tick::Tick;

/// A well-mixed pseudo-random value from a sequence number (for jitter).
#[cfg(feature = "client")]
pub(crate) fn jitter(seq: u64) -> u64 {
    hash::mix(seq)
}
