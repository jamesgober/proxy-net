//! Discovery: where a pool's endpoints come from when they change at run
//! time.

use std::boxed::Box;
use std::fmt;
use std::net::ToSocketAddrs;
use std::sync::Arc;
use std::time::Duration;
use std::vec::Vec;

use crate::endpoint::{Endpoint, EndpointAddr};
use crate::pool::Pool;

/// Why discovery could not give a set of endpoints. The pool keeps the set
/// it had (never an empty one) and the observer hears about it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiscoveryError(Box<str>);

impl DiscoveryError {
    /// An error with a message saying what failed.
    pub fn new(message: &str) -> DiscoveryError {
        DiscoveryError(message.into())
    }

    /// The message.
    pub fn message(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for DiscoveryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for DiscoveryError {}

/// A source of a pool's endpoints: a registry, DNS, a file, a control plane.
/// Configuration is trusted, and so is discovery; [`Pool::discovery_allow`]
/// limits the addresses it may give.
///
/// ```
/// use proxy_net::client::{Discovery, DiscoveryError};
/// use proxy_net::Endpoint;
///
/// struct Fixed;
/// impl Discovery for Fixed {
///     fn endpoints(&self) -> Result<Vec<Endpoint>, DiscoveryError> {
///         Ok(vec![Endpoint::parse("10.0.0.5:80").map_err(|e| DiscoveryError::new(&e.to_string()))?])
///     }
/// }
/// let pool = proxy_net::Pool::new(Vec::<&str>::new()).discovery(Fixed);
/// # let _ = pool;
/// ```
pub trait Discovery: Send + Sync + 'static {
    /// The endpoints now.
    ///
    /// # Errors
    ///
    /// A [`DiscoveryError`]; the pool keeps its last set.
    fn endpoints(&self) -> Result<Vec<Endpoint>, DiscoveryError>;

    /// How often to ask again; `None` asks once, when the proxy is built.
    fn refresh_every(&self) -> Option<Duration> {
        None
    }
}

/// A fixed set (useful in tests and as a placeholder).
#[derive(Clone, Debug)]
pub struct StaticDiscovery(Vec<Endpoint>);

impl StaticDiscovery {
    /// Always these endpoints.
    pub fn new(endpoints: Vec<Endpoint>) -> StaticDiscovery {
        StaticDiscovery(endpoints)
    }
}

impl Discovery for StaticDiscovery {
    fn endpoints(&self) -> Result<Vec<Endpoint>, DiscoveryError> {
        Ok(self.0.clone())
    }
}

/// Every address a host name resolves to (the system resolver), one
/// endpoint each, asked again every `refresh` (clamped to 1 s to 1 h).
///
/// ```no_run
/// use std::time::Duration;
/// let dns = proxy_net::client::DnsDiscovery::new("app.internal", 8080, Duration::from_secs(30));
/// let pool = proxy_net::Pool::new(Vec::<&str>::new()).discovery(dns);
/// # let _ = pool;
/// ```
#[derive(Clone, Debug)]
pub struct DnsDiscovery {
    host: Box<str>,
    port: u16,
    refresh: Duration,
}

impl DnsDiscovery {
    /// Resolves `host` and uses `port` for each address.
    pub fn new(host: &str, port: u16, refresh: Duration) -> DnsDiscovery {
        DnsDiscovery {
            host: host.into(),
            port,
            refresh: refresh.clamp(Duration::from_secs(1), Duration::from_secs(3600)),
        }
    }
}

impl Discovery for DnsDiscovery {
    fn endpoints(&self) -> Result<Vec<Endpoint>, DiscoveryError> {
        let addrs = (&*self.host, self.port)
            .to_socket_addrs()
            .map_err(|e| DiscoveryError::new(&format!("resolving {}: {e}", self.host)))?;
        let mut out: Vec<Endpoint> = Vec::new();
        for a in addrs {
            let e = Endpoint::new(EndpointAddr::Ip(a));
            if !out.iter().any(|x| x.addr() == e.addr()) {
                out.push(e);
            }
        }
        if out.is_empty() {
            return Err(DiscoveryError::new(&format!(
                "{} resolved to nothing",
                self.host
            )));
        }
        Ok(out)
    }

    fn refresh_every(&self) -> Option<Duration> {
        Some(self.refresh)
    }
}

/// A discovery source inside a `Pool`, compared by identity.
#[derive(Clone)]
pub(crate) struct DiscoverySlot(pub Arc<dyn Discovery>);

impl fmt::Debug for DiscoverySlot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Discovery")
    }
}

impl PartialEq for DiscoverySlot {
    fn eq(&self, other: &DiscoverySlot) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for DiscoverySlot {}

impl Pool {
    /// Takes the pool's endpoints from `source` (feature `client`): asked
    /// when the proxy is built and, when it says so, again on its schedule.
    /// A failed refresh keeps the last set. Static endpoints, if any, are
    /// replaced by the first answer.
    pub fn discovery(mut self, source: impl Discovery) -> Pool {
        self.set_discovery(DiscoverySlot(Arc::new(source)));
        self
    }
}

/// True for addresses discovery never yields without an allowlist naming
/// them: loopback, link-local (which holds cloud metadata services),
/// unspecified, multicast, and broadcast.
fn special(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v) => {
            v.is_loopback()
                || v.is_link_local()
                || v.is_unspecified()
                || v.is_multicast()
                || v.is_broadcast()
        }
        std::net::IpAddr::V6(v) => {
            let mapped = v.to_ipv4_mapped();
            v.is_loopback()
                || v.is_unspecified()
                || v.is_multicast()
                || (v.segments()[0] & 0xffc0) == 0xfe80
                || mapped.is_some_and(|m| special(std::net::IpAddr::V4(m)))
        }
    }
}

/// Keeps the endpoints inside the allowlist. With no allowlist, every
/// address but the special ones (see `special`) and every name is kept;
/// with one, no name is kept, since a name cannot be checked.
pub(crate) fn allowed(endpoints: Vec<Endpoint>, allow: &[crate::Cidr]) -> Vec<Endpoint> {
    if allow.is_empty() {
        return endpoints
            .into_iter()
            .filter(|e| match e.addr() {
                EndpointAddr::Ip(a) => !special(a.ip()),
                _ => true,
            })
            .collect();
    }
    endpoints
        .into_iter()
        .filter(|e| match e.addr() {
            EndpointAddr::Ip(a) => allow.iter().any(|c| c.contains(a.ip())),
            _ => false,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_allowlist_filters() {
        let eps = vec![
            Endpoint::parse("10.1.0.1:80").expect("e"),
            Endpoint::parse("192.168.0.1:80").expect("e"),
            Endpoint::parse("name.internal:80").expect("e"),
        ];
        let allow = vec!["10.0.0.0/8".parse().expect("cidr")];
        let kept = allowed(eps.clone(), &allow);
        assert_eq!(kept.len(), 1);
        assert_eq!(allowed(eps, &[]).len(), 3);
    }

    #[test]
    fn test_special_addresses_need_the_allowlist() {
        let eps: Vec<Endpoint> = [
            "127.0.0.1:80",
            "169.254.169.254:80",
            "0.0.0.0:80",
            "224.0.0.1:80",
            "255.255.255.255:80",
            "[::1]:80",
            "[fe80::1]:80",
            "[::ffff:127.0.0.1]:80",
            "[ff02::1]:80",
        ]
        .iter()
        .map(|t| Endpoint::parse(t).expect("e"))
        .collect();
        assert!(allowed(eps.clone(), &[]).is_empty());
        let allow = vec!["127.0.0.0/8".parse().expect("cidr")];
        assert_eq!(allowed(eps, &allow).len(), 1);
        let fine = vec![Endpoint::parse("10.0.0.1:80").expect("e")];
        assert_eq!(allowed(fine, &[]).len(), 1);
    }

    #[test]
    fn test_static_and_dns_discovery() {
        let s = StaticDiscovery::new(vec![Endpoint::parse("10.0.0.1:80").expect("e")]);
        assert_eq!(s.endpoints().expect("ok").len(), 1);
        assert_eq!(s.refresh_every(), None);
        let d = DnsDiscovery::new("localhost", 8080, Duration::ZERO);
        assert_eq!(d.refresh_every(), Some(Duration::from_secs(1)));
        let found = d.endpoints().expect("localhost resolves");
        assert!(found.iter().all(|e| e.addr().port() == 8080));
        let bad = DnsDiscovery::new("no-such-host.invalid", 80, Duration::from_secs(5));
        assert!(bad.endpoints().is_err());
        assert_eq!(DiscoveryError::new("x").message(), "x");
    }
}
