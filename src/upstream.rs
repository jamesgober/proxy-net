//! How a pool reaches its endpoints: the PROXY protocol, TLS settings, and
//! address ranges.

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::fmt;
use core::net::IpAddr;

use crate::error::ConfigReason;

/// The PROXY protocol header a pool sends at the start of each upstream
/// connection, so the endpoint sees the real client (from HAProxy's PROXY
/// protocol specification). A connection carrying one is never reused for
/// another client: with it on, every exchange opens its own connection.
///
/// ```
/// use proxy_net::{Pool, ProxyProtocol};
/// let pool = Pool::new(["10.0.0.1:80"]).proxy_protocol(ProxyProtocol::v2());
/// # let _ = pool;
/// ```
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ProxyProtocol {
    /// No header (the default).
    #[default]
    Off,
    /// The text header, version 1.
    V1,
    /// The binary header, version 2.
    V2 {
        /// Add the request's `Host` as the authority TLV.
        authority: bool,
        /// Add a CRC32C TLV.
        crc: bool,
    },
}

impl ProxyProtocol {
    /// Version 2 with the authority and CRC32C TLVs.
    pub fn v2() -> ProxyProtocol {
        ProxyProtocol::V2 {
            authority: true,
            crc: true,
        }
    }

    /// True when a header is sent.
    pub fn is_on(self) -> bool {
        self != ProxyProtocol::Off
    }
}

/// Which certificates an upstream's must chain to.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum UpstreamRoots {
    /// The Mozilla root set (the default).
    #[default]
    Public,
    /// The certificates in this PEM text only (a private CA, or the
    /// upstream's own certificate: the way to pin one).
    Pem(Box<[u8]>),
}

/// A PEM certificate chain and its PEM private key.
type CertAndKey = (Box<[u8]>, Box<[u8]>);

/// TLS toward a pool's endpoints (feature `tls`): verification against a
/// root set, SNI, and an optional client certificate.
///
/// ```
/// use proxy_net::{Pool, UpstreamTls};
/// let pool = Pool::new(["10.0.0.1:443"]).tls(UpstreamTls::new().server_name("app.internal"));
/// # let _ = pool;
/// ```
#[derive(Clone, PartialEq, Eq, Default)]
#[must_use]
pub struct UpstreamTls {
    roots: UpstreamRoots,
    server_name: Option<Box<str>>,
    client_cert: Option<CertAndKey>,
}

impl fmt::Debug for UpstreamTls {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The client key never appears in output.
        f.debug_struct("UpstreamTls")
            .field("roots", &matches!(self.roots, UpstreamRoots::Public))
            .field("server_name", &self.server_name)
            .field("client_cert", &self.client_cert.is_some())
            .finish()
    }
}

impl UpstreamTls {
    /// TLS verified against the public root set, SNI from each endpoint's
    /// server name.
    pub fn new() -> UpstreamTls {
        UpstreamTls::default()
    }

    /// Verify against the certificates in `pem` only.
    pub fn roots_pem(mut self, pem: &[u8]) -> UpstreamTls {
        self.roots = UpstreamRoots::Pem(pem.into());
        self
    }

    /// The name sent as SNI and verified in the certificate, for every
    /// endpoint of the pool (default: each endpoint's server name).
    pub fn server_name(mut self, name: &str) -> UpstreamTls {
        self.server_name = Some(name.into());
        self
    }

    /// A client certificate chain and its private key, both PEM.
    pub fn client_certificate(mut self, chain_pem: &[u8], key_pem: &[u8]) -> UpstreamTls {
        self.client_cert = Some((chain_pem.into(), key_pem.into()));
        self
    }

    /// The roots.
    pub fn roots(&self) -> &UpstreamRoots {
        &self.roots
    }

    /// The pool-wide server name, if set.
    pub fn server_name_value(&self) -> Option<&str> {
        self.server_name.as_deref()
    }

    /// The client certificate chain and key, if set.
    pub fn client_certificate_value(&self) -> Option<(&[u8], &[u8])> {
        self.client_cert.as_ref().map(|(c, k)| (&**c, &**k))
    }

    pub(crate) fn validate(&self) -> Result<(), ConfigReason> {
        if let Some(n) = &self.server_name {
            if crate::syntax::host_name(n.as_bytes()).is_none() {
                return Err(ConfigReason::HeaderValue);
            }
        }
        if let UpstreamRoots::Pem(p) = &self.roots {
            if p.is_empty() {
                return Err(ConfigReason::Inconsistent {
                    what: "tls: an empty root PEM trusts nothing",
                });
            }
        }
        Ok(())
    }
}

/// An address range: `10.0.0.0/8`, `fd00::/8`, or one address.
///
/// ```
/// use proxy_net::Cidr;
/// let net: Cidr = "10.0.0.0/8".parse()?;
/// assert!(net.contains("10.1.2.3".parse().unwrap()));
/// assert!(!net.contains("192.168.0.1".parse().unwrap()));
/// # Ok::<(), proxy_net::ConfigError>(())
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Cidr {
    addr: IpAddr,
    prefix: u8,
}

impl Cidr {
    /// The range's first address.
    fn masked(&self) -> IpAddr {
        match self.addr {
            IpAddr::V4(a) => {
                let mask = u32::MAX
                    .checked_shl(32 - u32::from(self.prefix))
                    .unwrap_or(0);
                IpAddr::V4((u32::from(a) & mask).into())
            }
            IpAddr::V6(a) => {
                let mask = u128::MAX
                    .checked_shl(128 - u32::from(self.prefix))
                    .unwrap_or(0);
                IpAddr::V6((u128::from(a) & mask).into())
            }
        }
    }

    /// True when `ip` is in the range.
    pub fn contains(&self, ip: IpAddr) -> bool {
        match (self.addr, ip) {
            (IpAddr::V4(a), IpAddr::V4(b)) => {
                let mask = u32::MAX
                    .checked_shl(32 - u32::from(self.prefix))
                    .unwrap_or(0);
                u32::from(a) & mask == u32::from(b) & mask
            }
            (IpAddr::V6(a), IpAddr::V6(b)) => {
                let mask = u128::MAX
                    .checked_shl(128 - u32::from(self.prefix))
                    .unwrap_or(0);
                u128::from(a) & mask == u128::from(b) & mask
            }
            _ => false,
        }
    }
}

impl core::str::FromStr for Cidr {
    type Err = crate::ConfigError;

    fn from_str(s: &str) -> Result<Cidr, crate::ConfigError> {
        let bad = || {
            crate::ConfigError::new(
                crate::ConfigItem::Proxy,
                ConfigReason::Inconsistent {
                    what: "an address range is an address, or an address and /prefix with no bits set past the prefix",
                },
            )
        };
        let (ip, prefix) = match s.split_once('/') {
            Some((ip, p)) => (ip, Some(p)),
            None => (s, None),
        };
        let addr: IpAddr = ip.parse().map_err(|_| bad())?;
        let max = if addr.is_ipv4() { 32 } else { 128 };
        let prefix = match prefix {
            None => max,
            Some(p) => crate::syntax::parse_port(p.as_bytes())
                .and_then(|n| u8::try_from(n).ok())
                .or_else(|| (p == "0").then_some(0))
                .filter(|n| *n <= max)
                .ok_or_else(bad)?,
        };
        let c = Cidr { addr, prefix };
        // Bits past the prefix are a typo for a narrower range; refuse them
        // instead of allowing the wider one.
        if c.masked() != addr {
            return Err(bad());
        }
        Ok(c)
    }
}

/// Parses a list of ranges.
pub(crate) fn parse_ranges(ranges: &[&str]) -> Result<Vec<Cidr>, crate::ConfigError> {
    ranges.iter().map(|r| r.parse()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cidr() {
        let c: Cidr = "10.0.0.0/8".parse().expect("cidr");
        assert!(c.contains("10.255.0.1".parse().expect("ip")));
        assert!(!c.contains("11.0.0.1".parse().expect("ip")));
        assert!(!c.contains("::1".parse().expect("ip")));
        let one: Cidr = "192.0.2.7".parse().expect("one");
        assert!(one.contains("192.0.2.7".parse().expect("ip")));
        assert!(!one.contains("192.0.2.8".parse().expect("ip")));
        let all: Cidr = "0.0.0.0/0".parse().expect("all");
        assert!(all.contains("8.8.8.8".parse().expect("ip")));
        let v6: Cidr = "fd00::/8".parse().expect("v6");
        assert!(v6.contains("fd12::1".parse().expect("ip")));
        for bad in [
            "",
            "10.0.0.0/33",
            "10.0.0.0/-1",
            "x/8",
            "::/129",
            "10.0.0.0/08",
            "10.1.0.0/8",
            "fd00::1/8",
        ] {
            assert!(bad.parse::<Cidr>().is_err(), "{bad}");
        }
        assert_eq!(
            parse_ranges(&["10.0.0.0/8", "fd00::/8"]).expect("ok").len(),
            2
        );
    }

    #[test]
    fn test_tls_validation_and_debug() {
        assert!(UpstreamTls::new().validate().is_ok());
        assert!(
            UpstreamTls::new()
                .server_name("bad name")
                .validate()
                .is_err()
        );
        assert!(UpstreamTls::new().roots_pem(b"").validate().is_err());
        let t = UpstreamTls::new().client_certificate(b"CERT", b"SECRET KEY");
        assert!(!alloc::format!("{t:?}").contains("SECRET"));
        assert_eq!(
            t.client_certificate_value().map(|(c, _)| c),
            Some(&b"CERT"[..])
        );
        assert!(ProxyProtocol::v2().is_on());
        assert!(!ProxyProtocol::default().is_on());
    }
}
