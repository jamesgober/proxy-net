//! TLS toward upstreams (feature `tls`), through tls-net.

use std::boxed::Box;
use std::io::{Read, Write};
use std::string::{String, ToString};
use std::time::Instant;

use net_io::{Phase, TcpConnection};
use tls_net::{Alpn, ClientSession, KeySource, TlsClient, TlsStream, TrustAnchors};

use super::connector::{Connector, TcpConnector, UpstreamIo};
use crate::core::{ErrorClass, TimeoutKind};
use crate::endpoint::EndpointAddr;
use crate::error::ConfigReason;
use crate::upstream::{UpstreamRoots, UpstreamTls};

/// TCP connections with TLS on top: the certificate verified against the
/// pool's roots, SNI from the pool's server name or each endpoint's, ALPN
/// `http/1.1`, and the client certificate when one is set.
pub struct TlsConnector {
    tcp: TcpConnector,
    client: TlsClient,
    server_name: Option<String>,
}

impl std::fmt::Debug for TlsConnector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TlsConnector")
            .field("server_name", &self.server_name)
            .finish_non_exhaustive()
    }
}

impl TlsConnector {
    /// A connector for `tls`.
    ///
    /// # Errors
    ///
    /// `Inconsistent` naming the setting when the roots, the client
    /// certificate, or the provider cannot be loaded.
    pub fn new(tls: &UpstreamTls) -> Result<TlsConnector, ConfigReason> {
        let roots = match tls.roots() {
            UpstreamRoots::Pem(pem) => {
                TrustAnchors::from_pem(pem).map_err(|_| ConfigReason::Inconsistent {
                    what: "tls: the root certificates do not parse",
                })?
            }
            _ => TrustAnchors::mozilla(),
        };
        let mut builder = TlsClient::builder().trust(roots).alpn(&[Alpn::HTTP_1_1]);
        if let Some((chain, key)) = tls.client_certificate_value() {
            builder = builder
                .client_certificate(chain, KeySource::Pem(key))
                .map_err(|_| ConfigReason::Inconsistent {
                    what: "tls: the client certificate or key does not load",
                })?;
        }
        let client = builder.build().map_err(|_| ConfigReason::Inconsistent {
            what: "tls: the TLS client could not be built (no crypto provider?)",
        })?;
        Ok(TlsConnector {
            tcp: TcpConnector::new(),
            client,
            server_name: tls.server_name_value().map(str::to_string),
        })
    }
}

impl TlsConnector {
    /// A new client session for `endpoint`: the pool's server name, or the
    /// endpoint's host or address.
    pub(crate) fn session(&self, endpoint: &EndpointAddr) -> Result<ClientSession, ErrorClass> {
        let name = match (&self.server_name, endpoint) {
            (Some(n), _) => n.clone(),
            (None, EndpointAddr::Name { host, .. }) => host.to_string(),
            (None, EndpointAddr::Ip(a)) => a.ip().to_string(),
        };
        self.client.connect(&name).map_err(|_| ErrorClass::Tls)
    }
}

impl Connector for TlsConnector {
    fn connect(
        &self,
        endpoint: &EndpointAddr,
        prelude: &[u8],
        deadline: Instant,
    ) -> Result<Box<dyn UpstreamIo>, ErrorClass> {
        let mut tcp = self.tcp.dial(endpoint, prelude, deadline)?;
        let session = self.session(endpoint)?;
        tcp.set_phase(Phase::Receiving);
        tcp.set_exchange_deadline(Some(deadline));
        let stream = TlsStream::client(session, tcp).map_err(|_| ErrorClass::Tls)?;
        Ok(Box::new(TlsUpstream { stream }))
    }
}

/// A TLS connection to an upstream.
struct TlsUpstream {
    stream: TlsStream<TcpConnection>,
}

fn class(e: &std::io::Error, kind: TimeoutKind) -> ErrorClass {
    match e.kind() {
        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock => ErrorClass::Timeout(kind),
        std::io::ErrorKind::InvalidData => ErrorClass::Tls,
        _ => ErrorClass::UpstreamReset,
    }
}

impl UpstreamIo for TlsUpstream {
    fn send(&mut self, bytes: &[u8], deadline: Instant) -> Result<(), ErrorClass> {
        let tcp = self.stream.get_mut();
        tcp.set_phase(Phase::Sending);
        tcp.set_exchange_deadline(Some(deadline));
        self.stream
            .write_all(bytes)
            .and_then(|()| self.stream.flush())
            .map_err(|e| class(&e, TimeoutKind::WriteIdle))
    }

    fn recv(
        &mut self,
        buf: &mut [u8],
        deadline: Instant,
        kind: TimeoutKind,
    ) -> Result<usize, ErrorClass> {
        let tcp = self.stream.get_mut();
        tcp.set_phase(Phase::Receiving);
        tcp.set_exchange_deadline(Some(deadline));
        self.stream.read(buf).map_err(|e| class(&e, kind))
    }

    fn is_reusable(&mut self) -> bool {
        // Anything waiting (records or data) makes the connection unfit: data
        // left on it would be read as the next response.
        matches!(self.stream.get_mut().probe(), Ok(net_io::Health::Open))
    }

    fn close(&mut self) {
        let _ = self.stream.get_mut().shutdown(std::net::Shutdown::Both);
    }
}
