//! The upstream side over net-io (feature `client`): the transport seam,
//! connections and their pool, the forwarder that drives one exchange, and
//! the active health checker.

pub(crate) mod checker;
mod connector;
pub(crate) mod discovery;
pub(crate) mod forward;
pub(crate) mod pool;
pub(crate) mod runtime;
#[cfg(all(feature = "tls", feature = "ws"))]
pub(crate) mod split;
#[cfg(feature = "tls")]
pub(crate) mod tls;

pub use connector::{Connector, TcpConnector, TcpUpstream, UpstreamIo};
pub use discovery::{Discovery, DiscoveryError, DnsDiscovery, StaticDiscovery};
pub use forward::{Failure, Forward, RequestParts, ResponseStart};
#[cfg(feature = "tls")]
pub use tls::TlsConnector;
