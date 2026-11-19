//! Upstream endpoints: where a pool sends requests.

use alloc::boxed::Box;
use core::fmt;
use core::net::{IpAddr, SocketAddr, SocketAddrV6};
use core::str::FromStr;

use crate::error::{ConfigError, ConfigItem, ConfigReason, EndpointErrorKind};
use crate::syntax;

/// The longest text that can be an endpoint: a 253-byte host name, `:`, and a
/// five-digit port, or a bracketed IPv6 address and a port.
const MAX_LEN: usize = 253 + 1 + 5;

/// The default cap on connections to one endpoint.
pub const DEFAULT_MAX_CONNECTIONS: u32 = 256;

/// The address of an upstream endpoint: an IP address and port, or a host name
/// and port that is resolved when connections are made.
///
/// `Display` writes the form [`Endpoint::parse`] reads back.
///
/// ```
/// use proxy_net::{Endpoint, EndpointAddr};
/// let e = Endpoint::parse("[2001:db8::1]:8080")?;
/// assert!(matches!(e.addr(), EndpointAddr::Ip(_)));
/// assert_eq!(e.addr().to_string(), "[2001:db8::1]:8080");
/// let e = Endpoint::parse("App.Internal:80")?;
/// assert_eq!(e.addr(), &EndpointAddr::Name { host: "app.internal".into(), port: 80 });
/// # Ok::<(), proxy_net::ConfigError>(())
/// ```
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum EndpointAddr {
    /// An IPv4 or IPv6 address with a port.
    Ip(SocketAddr),
    /// A host name, lowercase and without a trailing dot, with a port.
    Name {
        /// The host name.
        host: Box<str>,
        /// The port.
        port: u16,
    },
}

impl EndpointAddr {
    /// The port.
    ///
    /// ```
    /// let e = proxy_net::Endpoint::parse("db.internal:5432")?;
    /// assert_eq!(e.addr().port(), 5432);
    /// # Ok::<(), proxy_net::ConfigError>(())
    /// ```
    pub fn port(&self) -> u16 {
        match self {
            EndpointAddr::Ip(a) => a.port(),
            EndpointAddr::Name { port, .. } => *port,
        }
    }

    /// The host name, or `None` for an IP address.
    ///
    /// ```
    /// let e = proxy_net::Endpoint::parse("db.internal:5432")?;
    /// assert_eq!(e.addr().host_name(), Some("db.internal"));
    /// assert_eq!(proxy_net::Endpoint::parse("10.0.0.1:80")?.addr().host_name(), None);
    /// # Ok::<(), proxy_net::ConfigError>(())
    /// ```
    pub fn host_name(&self) -> Option<&str> {
        match self {
            EndpointAddr::Ip(_) => None,
            EndpointAddr::Name { host, .. } => Some(host),
        }
    }
}

impl fmt::Display for EndpointAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EndpointAddr::Ip(a) => write!(f, "{a}"),
            EndpointAddr::Name { host, port } => write!(f, "{host}:{port}"),
        }
    }
}

/// One upstream endpoint of a pool: its address and how much traffic it takes.
///
/// ```
/// use proxy_net::Endpoint;
/// let e = Endpoint::parse("10.0.0.7:80")?.weight(3).tier(1).max_connections(64);
/// assert_eq!(e.weight_value(), 3);
/// assert_eq!(e.tier_value(), 1);
/// assert_eq!(e.max_connections_value(), 64);
/// # Ok::<(), proxy_net::ConfigError>(())
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoint {
    addr: EndpointAddr,
    weight: u16,
    tier: u8,
    max_connections: u32,
    server_name: Option<Box<str>>,
}

impl Endpoint {
    /// Parses `host:port`, `ip:port`, or `[ipv6]:port`.
    ///
    /// Host names follow RFC 1123 (labels of 1 to 63 letters, digits, and
    /// hyphens, at most 253 bytes) and are stored in lowercase without a
    /// trailing dot. IPv6 zone ids, userinfo, paths, a port of 0, and a port
    /// with a sign or a leading zero are refused.
    ///
    /// ```
    /// use proxy_net::{ConfigReason, Endpoint, EndpointErrorKind};
    /// assert!(Endpoint::parse("10.0.0.1:80").is_ok());
    /// let err = Endpoint::parse("::1:80").unwrap_err();
    /// assert_eq!(err.reason(), &ConfigReason::EndpointSyntax(EndpointErrorKind::BadHost));
    /// ```
    ///
    /// # Errors
    ///
    /// [`ConfigReason::EndpointSyntax`] with the [`EndpointErrorKind`].
    pub fn parse(text: &str) -> Result<Endpoint, ConfigError> {
        parse_addr(text.as_bytes())
            .map(Endpoint::new)
            .map_err(|kind| {
                ConfigError::new(
                    ConfigItem::Endpoint {
                        pool: None,
                        index: None,
                    },
                    ConfigReason::EndpointSyntax(kind),
                )
            })
    }

    /// An endpoint at `addr` with weight 1, tier 0, and the default connection
    /// cap.
    ///
    /// ```
    /// use proxy_net::{Endpoint, EndpointAddr};
    /// let e = Endpoint::new(EndpointAddr::Ip("10.0.0.1:80".parse().unwrap()));
    /// assert_eq!(e.weight_value(), 1);
    /// ```
    pub fn new(addr: EndpointAddr) -> Endpoint {
        Endpoint {
            addr,
            weight: 1,
            tier: 0,
            max_connections: DEFAULT_MAX_CONNECTIONS,
            server_name: None,
        }
    }

    /// The share of traffic relative to the pool's other endpoints, 1 to 1,000
    /// (checked when the configuration is built). Default 1.
    ///
    /// ```
    /// let e = proxy_net::Endpoint::parse("10.0.0.1:80")?.weight(5);
    /// assert_eq!(e.weight_value(), 5);
    /// # Ok::<(), proxy_net::ConfigError>(())
    /// ```
    #[must_use]
    pub fn weight(mut self, weight: u16) -> Endpoint {
        self.weight = weight;
        self
    }

    /// The priority tier: tier 0 takes traffic, and a higher tier only when the
    /// tiers below it have no healthy endpoint. 0 to 15. Default 0.
    ///
    /// ```
    /// let standby = proxy_net::Endpoint::parse("10.0.1.1:80")?.tier(1);
    /// assert_eq!(standby.tier_value(), 1);
    /// # Ok::<(), proxy_net::ConfigError>(())
    /// ```
    #[must_use]
    pub fn tier(mut self, tier: u8) -> Endpoint {
        self.tier = tier;
        self
    }

    /// The cap on open connections to this endpoint, 1 to 1,000,000. Default
    /// 256.
    ///
    /// ```
    /// let e = proxy_net::Endpoint::parse("10.0.0.1:80")?.max_connections(32);
    /// assert_eq!(e.max_connections_value(), 32);
    /// # Ok::<(), proxy_net::ConfigError>(())
    /// ```
    #[must_use]
    pub fn max_connections(mut self, n: u32) -> Endpoint {
        self.max_connections = n;
        self
    }

    /// The name sent as TLS SNI and as `Host` when a route uses
    /// [`HostHeader::Endpoint`](crate::HostHeader::Endpoint). Default: the
    /// endpoint's host name, or none for an address.
    ///
    /// ```
    /// let e = proxy_net::Endpoint::parse("10.0.0.1:443")?.server_name("app.internal")?;
    /// assert_eq!(e.server_name_value(), Some("app.internal"));
    /// # Ok::<(), proxy_net::ConfigError>(())
    /// ```
    ///
    /// # Errors
    ///
    /// [`EndpointErrorKind::BadHost`] when `name` is not a host name.
    pub fn server_name(mut self, name: &str) -> Result<Endpoint, ConfigError> {
        let name = syntax::host_name(name.as_bytes()).ok_or_else(|| {
            ConfigError::new(
                ConfigItem::Endpoint {
                    pool: None,
                    index: None,
                },
                ConfigReason::EndpointSyntax(EndpointErrorKind::BadHost),
            )
        })?;
        self.server_name = Some(name);
        Ok(self)
    }

    /// The address.
    pub fn addr(&self) -> &EndpointAddr {
        &self.addr
    }

    /// The weight.
    pub fn weight_value(&self) -> u16 {
        self.weight
    }

    /// The tier.
    pub fn tier_value(&self) -> u8 {
        self.tier
    }

    /// The connection cap.
    pub fn max_connections_value(&self) -> u32 {
        self.max_connections
    }

    /// The configured server name, or the host name of a named endpoint.
    pub fn server_name_value(&self) -> Option<&str> {
        self.server_name.as_deref().or(self.addr.host_name())
    }

    /// Checks the settings that the setters cannot check alone.
    pub(crate) fn validate(&self) -> Result<(), ConfigReason> {
        if !(1..=1000).contains(&self.weight) {
            return Err(ConfigReason::Weight);
        }
        if self.tier > 15 {
            return Err(ConfigReason::Limit { field: "tier" });
        }
        if !(1..=1_000_000).contains(&self.max_connections) {
            return Err(ConfigReason::Limit {
                field: "max_connections",
            });
        }
        Ok(())
    }
}

impl FromStr for Endpoint {
    type Err = ConfigError;

    fn from_str(s: &str) -> Result<Endpoint, ConfigError> {
        Endpoint::parse(s)
    }
}

/// The grammar of [`Endpoint::parse`].
pub(crate) fn parse_addr(s: &[u8]) -> Result<EndpointAddr, EndpointErrorKind> {
    if s.is_empty() {
        return Err(EndpointErrorKind::Empty);
    }
    if s.len() > MAX_LEN {
        return Err(EndpointErrorKind::TooLong);
    }
    if s.contains(&b'@') {
        return Err(EndpointErrorKind::UserInfo);
    }
    if s.iter().any(|b| matches!(b, b'/' | b'?' | b'#')) {
        return Err(EndpointErrorKind::Path);
    }
    if !s
        .iter()
        .all(|&b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b':' | b'[' | b']'))
    {
        return Err(EndpointErrorKind::BadByte);
    }
    if let Some(rest) = s.strip_prefix(b"[") {
        let close = rest
            .iter()
            .position(|&b| b == b']')
            .ok_or(EndpointErrorKind::BadHost)?;
        let ip = syntax::ipv6(&rest[..close]).ok_or(EndpointErrorKind::BadHost)?;
        let after = &rest[close + 1..];
        let port = match after {
            [] | [b':'] => return Err(EndpointErrorKind::MissingPort),
            [b':', port @ ..] => syntax::parse_port(port).ok_or(EndpointErrorKind::BadPort)?,
            _ => return Err(EndpointErrorKind::BadHost),
        };
        return Ok(EndpointAddr::Ip(SocketAddr::V6(SocketAddrV6::new(
            ip, port, 0, 0,
        ))));
    }
    if s.iter().any(|b| matches!(b, b'[' | b']')) {
        return Err(EndpointErrorKind::BadHost);
    }
    let colon = s
        .iter()
        .rposition(|&b| b == b':')
        .ok_or(EndpointErrorKind::MissingPort)?;
    let (host, port) = (&s[..colon], &s[colon + 1..]);
    if host.contains(&b':') {
        // An IPv6 address without brackets: its port cannot be told apart.
        return Err(EndpointErrorKind::BadHost);
    }
    if port.is_empty() {
        return Err(EndpointErrorKind::MissingPort);
    }
    let port = syntax::parse_port(port).ok_or(EndpointErrorKind::BadPort)?;
    if let Some(ip) = syntax::ipv4(host) {
        return Ok(EndpointAddr::Ip(SocketAddr::new(IpAddr::V4(ip), port)));
    }
    let host = syntax::host_name(host).ok_or(EndpointErrorKind::BadHost)?;
    Ok(EndpointAddr::Name { host, port })
}

#[cfg(test)]
mod tests {
    use alloc::string::ToString;

    use super::*;

    fn kind(s: &str) -> EndpointErrorKind {
        match parse_addr(s.as_bytes()) {
            Err(k) => k,
            Ok(a) => panic!("{s} parsed as {a:?}"),
        }
    }

    #[test]
    fn test_parse_accepts_each_form() {
        let a = parse_addr(b"10.0.0.1:80").expect("v4");
        assert_eq!(a.to_string(), "10.0.0.1:80");
        let a = parse_addr(b"[::1]:8080").expect("v6");
        assert_eq!(a.to_string(), "[::1]:8080");
        let a = parse_addr(b"Host.Example.:443").expect("name");
        assert_eq!(
            a,
            EndpointAddr::Name {
                host: "host.example".into(),
                port: 443
            }
        );
        assert_eq!(a.port(), 443);
        assert_eq!(parse_addr(b"h:65535").expect("max").port(), 65535);
    }

    #[test]
    fn test_parse_refuses_with_the_right_kind() {
        assert_eq!(kind(""), EndpointErrorKind::Empty);
        assert_eq!(kind("host"), EndpointErrorKind::MissingPort);
        assert_eq!(kind("host:"), EndpointErrorKind::MissingPort);
        assert_eq!(kind("[::1]"), EndpointErrorKind::MissingPort);
        assert_eq!(kind("[::1]:"), EndpointErrorKind::MissingPort);
        assert_eq!(kind("host:0"), EndpointErrorKind::BadPort);
        assert_eq!(kind("host:65536"), EndpointErrorKind::BadPort);
        assert_eq!(kind("host:080"), EndpointErrorKind::BadPort);
        assert_eq!(kind(":80"), EndpointErrorKind::BadHost);
        assert_eq!(kind("::1:80"), EndpointErrorKind::BadHost);
        assert_eq!(kind("1.2.3:80"), EndpointErrorKind::BadHost);
        assert_eq!(kind("-a:80"), EndpointErrorKind::BadHost);
        assert_eq!(kind("a..b:80"), EndpointErrorKind::BadHost);
        assert_eq!(kind("[::1]x:80"), EndpointErrorKind::BadHost);
        assert_eq!(kind("[zz]:80"), EndpointErrorKind::BadHost);
        assert_eq!(kind("a]b:80"), EndpointErrorKind::BadHost);
        assert_eq!(kind("user@host:80"), EndpointErrorKind::UserInfo);
        assert_eq!(kind("host:80/x"), EndpointErrorKind::Path);
        assert_eq!(kind("host:80?x"), EndpointErrorKind::Path);
        assert_eq!(kind("[fe80::1%eth0]:80"), EndpointErrorKind::BadByte);
        assert_eq!(kind("ho st:80"), EndpointErrorKind::BadByte);
        assert_eq!(kind("h\u{e9}:80"), EndpointErrorKind::BadByte);
        assert_eq!(kind(&"a".repeat(260)), EndpointErrorKind::TooLong);
        assert_eq!(kind("host:+80"), EndpointErrorKind::BadByte);
    }

    #[test]
    fn test_display_round_trips() {
        for s in ["10.0.0.1:80", "[2001:db8::1]:443", "app.internal:8080"] {
            let a = parse_addr(s.as_bytes()).expect(s);
            let again = parse_addr(a.to_string().as_bytes()).expect("again");
            assert_eq!(a, again);
        }
    }

    #[test]
    fn test_setters_and_validation() {
        let e = Endpoint::parse("10.0.0.1:80").expect("parse");
        assert_eq!(e.validate(), Ok(()));
        assert_eq!(e.clone().weight(0).validate(), Err(ConfigReason::Weight));
        assert_eq!(e.clone().weight(1001).validate(), Err(ConfigReason::Weight));
        assert_eq!(e.clone().weight(1000).validate(), Ok(()));
        assert_eq!(
            e.clone().tier(16).validate(),
            Err(ConfigReason::Limit { field: "tier" })
        );
        assert_eq!(
            e.clone().max_connections(0).validate(),
            Err(ConfigReason::Limit {
                field: "max_connections"
            })
        );
        assert_eq!(e.server_name_value(), None);
        let named = Endpoint::parse("a.example:80").expect("parse");
        assert_eq!(named.server_name_value(), Some("a.example"));
        let sni = e.server_name("B.example").expect("sni");
        assert_eq!(sni.server_name_value(), Some("b.example"));
        assert!(
            Endpoint::parse("10.0.0.1:80")
                .expect("parse")
                .server_name("bad name")
                .is_err()
        );
        let from: Endpoint = "10.0.0.2:81".parse().expect("from_str");
        assert_eq!(from.addr().port(), 81);
    }
}
