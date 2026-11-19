//! The transport seam: how the proxy reaches an endpoint.

use std::boxed::Box;
use std::time::{Duration, Instant};

use net_io::{ConnectOptions, MinRate, NetError, Phase, TcpConnection};

use crate::core::{ErrorClass, TimeoutKind};
use crate::endpoint::EndpointAddr;

/// One connection to an upstream endpoint, as the forwarder uses it. Every
/// call takes a deadline; a call that passes it fails with a timeout.
///
/// Implement it for a transport of your own (an in-memory pair in tests, a
/// tunnel); [`TcpConnector`] gives the TCP one.
pub trait UpstreamIo: Send + 'static {
    /// Sends all of `bytes` by `deadline`.
    ///
    /// # Errors
    ///
    /// `Timeout(WriteIdle)`, `UpstreamReset`.
    fn send(&mut self, bytes: &[u8], deadline: Instant) -> Result<(), ErrorClass>;

    /// Receives into `buf` by `deadline`; `Ok(0)` when the upstream closed.
    /// `kind` names the timeout reported when the deadline passes.
    ///
    /// # Errors
    ///
    /// `Timeout(kind)`, `UpstreamReset`.
    fn recv(
        &mut self,
        buf: &mut [u8],
        deadline: Instant,
        kind: TimeoutKind,
    ) -> Result<usize, ErrorClass>;

    /// True when the connection is open with nothing waiting, so a pooled
    /// connection may carry another request (a probe before every reuse).
    fn is_reusable(&mut self) -> bool;

    /// Closes the connection.
    fn close(&mut self);
}

/// Opens connections to endpoints.
pub trait Connector: Send + Sync + 'static {
    /// Connects to `endpoint` by `deadline` and sends `prelude` first (the
    /// PROXY protocol header, when the pool uses one; empty otherwise).
    ///
    /// # Errors
    ///
    /// `Connect`, `Tls`, `Timeout(Connect)`.
    fn connect(
        &self,
        endpoint: &EndpointAddr,
        prelude: &[u8],
        deadline: Instant,
    ) -> Result<Box<dyn UpstreamIo>, ErrorClass>;
}

/// TCP connections through net-io: happy eyeballs for names with several
/// addresses, the system resolver for host names, and net-io's deadlines.
///
/// ```no_run
/// use std::time::{Duration, Instant};
/// use proxy_net::client::{Connector, TcpConnector};
/// let endpoint = proxy_net::Endpoint::parse("127.0.0.1:8080")?;
/// let conn = TcpConnector::new().connect(endpoint.addr(), b"", Instant::now() + Duration::from_secs(1));
/// # let _ = conn;
/// # Ok::<(), proxy_net::ConfigError>(())
/// ```
#[derive(Clone, Debug)]
pub struct TcpConnector {
    limits: net_io::Limits,
}

/// Phase timeouts long enough that the per-call deadline always decides.
const LONG: Duration = Duration::from_secs(24 * 3600);

impl TcpConnector {
    /// A connector with net-io's phase timeouts set out of the way: every
    /// call's own deadline decides, and the minimum transfer rate is off
    /// (an upstream may legitimately trickle a long response).
    pub fn new() -> TcpConnector {
        let limits = net_io::Limits::builder()
            .read_timeout(LONG)
            .write_timeout(LONG)
            .idle_timeout(LONG)
            .min_rate(MinRate::off())
            .build()
            .unwrap_or_default();
        TcpConnector { limits }
    }
}

impl Default for TcpConnector {
    fn default() -> TcpConnector {
        TcpConnector::new()
    }
}

fn connect_class(e: &NetError) -> ErrorClass {
    match e {
        NetError::TimedOut { .. } => ErrorClass::Timeout(TimeoutKind::Connect),
        _ => ErrorClass::Connect,
    }
}

impl TcpConnector {
    /// The TCP connection, with `prelude` sent.
    pub(crate) fn dial(
        &self,
        endpoint: &EndpointAddr,
        prelude: &[u8],
        deadline: Instant,
    ) -> Result<TcpConnection, ErrorClass> {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(ErrorClass::Timeout(TimeoutKind::Connect));
        }
        let options = ConnectOptions::new()
            .timeout(left)
            .limits(self.limits.clone());
        let conn = match endpoint {
            EndpointAddr::Ip(addr) => TcpConnection::connect_with(*addr, options),
            EndpointAddr::Name { host, port } => TcpConnection::connect_host(host, *port, options),
        }
        .map_err(|e| connect_class(&e))?;
        let mut up = TcpUpstream { conn };
        if !prelude.is_empty() {
            up.send(prelude, deadline)
                .map_err(|_| ErrorClass::Connect)?;
        }
        Ok(up.conn)
    }
}

impl Connector for TcpConnector {
    fn connect(
        &self,
        endpoint: &EndpointAddr,
        prelude: &[u8],
        deadline: Instant,
    ) -> Result<Box<dyn UpstreamIo>, ErrorClass> {
        let conn = self.dial(endpoint, prelude, deadline)?;
        Ok(Box::new(TcpUpstream { conn }))
    }
}

/// A TCP connection to an upstream.
#[derive(Debug)]
pub struct TcpUpstream {
    conn: TcpConnection,
}

fn io_class(e: &NetError, kind: TimeoutKind) -> ErrorClass {
    match e {
        NetError::TimedOut { .. } => ErrorClass::Timeout(kind),
        _ => ErrorClass::UpstreamReset,
    }
}

impl UpstreamIo for TcpUpstream {
    fn send(&mut self, bytes: &[u8], deadline: Instant) -> Result<(), ErrorClass> {
        self.conn.set_phase(Phase::Sending);
        self.conn.set_exchange_deadline(Some(deadline));
        self.conn
            .send_all(bytes)
            .map_err(|e| io_class(&e, TimeoutKind::WriteIdle))
    }

    fn recv(
        &mut self,
        buf: &mut [u8],
        deadline: Instant,
        kind: TimeoutKind,
    ) -> Result<usize, ErrorClass> {
        self.conn.set_phase(Phase::Receiving);
        self.conn.set_exchange_deadline(Some(deadline));
        match self.conn.recv(buf) {
            Ok(n) => Ok(n),
            Err(NetError::Closed) => Ok(0),
            Err(e) => Err(io_class(&e, kind)),
        }
    }

    fn is_reusable(&mut self) -> bool {
        matches!(self.conn.probe(), Ok(net_io::Health::Open))
    }

    fn close(&mut self) {
        let _ = self.conn.shutdown(std::net::Shutdown::Both);
    }
}
