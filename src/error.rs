//! Configuration errors: what was refused, where, and what to change.

use alloc::boxed::Box;
use core::fmt;

/// A configuration that cannot be built.
///
/// [`item`](ConfigError::item) says where the problem is and
/// [`reason`](ConfigError::reason) says what is wrong; `Display` gives both in
/// one line that names what to change.
///
/// ```
/// let err = proxy_net::Endpoint::parse("10.0.0.1").unwrap_err();
/// assert_eq!(
///     err.reason(),
///     &proxy_net::ConfigReason::EndpointSyntax(proxy_net::EndpointErrorKind::MissingPort),
/// );
/// assert_eq!(err.to_string(), "endpoint: the port is missing (write host:port)");
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigError {
    item: ConfigItem,
    reason: ConfigReason,
}

impl ConfigError {
    pub(crate) fn new(item: ConfigItem, reason: ConfigReason) -> ConfigError {
        ConfigError { item, reason }
    }

    /// Where the problem is.
    pub fn item(&self) -> &ConfigItem {
        &self.item
    }

    /// What is wrong.
    pub fn reason(&self) -> &ConfigReason {
        &self.reason
    }
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.item, self.reason)
    }
}

impl core::error::Error for ConfigError {}

/// Where a configuration error is.
///
/// ```
/// use proxy_net::{ConfigItem, Pool, ProxyBuilder};
/// let err = ProxyBuilder::new()
///     .pool("app", Pool::new(["10.0.0.1:80", "10.0.0.2"]))
///     .config()
///     .unwrap_err();
/// assert_eq!(
///     err.item(),
///     &ConfigItem::Endpoint { pool: Some("app".into()), index: Some(1) },
/// );
/// ```
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfigItem {
    /// An endpoint: its pool and its position there, when known.
    Endpoint {
        /// The pool's name.
        pool: Option<Box<str>>,
        /// The endpoint's position in the pool, from 0.
        index: Option<usize>,
    },
    /// A pool, by name.
    Pool(Box<str>),
    /// A route, by its position in the table, from 0.
    Route(usize),
    /// A setting of the proxy as a whole.
    Proxy,
}

impl fmt::Display for ConfigItem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigItem::Endpoint { pool, index } => {
                if let Some(pool) = pool {
                    write!(f, "pool \"{pool}\" ")?;
                }
                f.write_str("endpoint")?;
                if let Some(index) = index {
                    write!(f, " {index}")?;
                }
                Ok(())
            }
            ConfigItem::Pool(name) => write!(f, "pool \"{name}\""),
            ConfigItem::Route(index) => write!(f, "route {index}"),
            ConfigItem::Proxy => f.write_str("proxy"),
        }
    }
}

/// What is wrong with a configuration item.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfigReason {
    /// An endpoint address does not parse.
    EndpointSyntax(EndpointErrorKind),
    /// A pool has no endpoints.
    NoEndpoints,
    /// The same endpoint appears twice in one pool.
    DuplicateEndpoint,
    /// Two pools have the same name.
    DuplicatePool,
    /// A pool name is empty, longer than 64 bytes, or has a byte other than an
    /// ASCII letter, digit, `-`, `_`, or `.`.
    PoolName,
    /// A route names a pool that does not exist.
    UnknownPool,
    /// A route does not say which pool it goes to (`Route::to`).
    MissingPool,
    /// The proxy has no pools, or several pools and no routes.
    NoRoutes,
    /// A host pattern is not a host name, an address, `*`, or `*.` followed by
    /// a host name.
    HostPattern,
    /// A path pattern does not start with `/`, has a byte that cannot be in a
    /// path, a bad percent escape, a dot segment, a query, or a fragment.
    PathPattern,
    /// A path rewrite does not start at the beginning of the route's path or
    /// ends inside a segment.
    PathRewrite,
    /// A route can never match: the route at `by` matches everything it would.
    Shadowed {
        /// The earlier route that takes all of this route's requests.
        by: usize,
    },
    /// A method is not a token.
    Method,
    /// A header name is not a token.
    HeaderName,
    /// A header value has a control byte, or leading or trailing whitespace.
    HeaderValue,
    /// A header rule names a field the proxy writes itself (framing,
    /// hop-by-hop, `Host`, `Via`, or the forwarding fields).
    ReservedHeader,
    /// The `Via` pseudonym or an obfuscated node name has the wrong form.
    Pseudonym,
    /// An endpoint weight is outside 1 to 1,000.
    Weight,
    /// A limit, a count, or a duration is outside its allowed range; the field
    /// is named.
    Limit {
        /// The setting, as named in the API (`timeouts.connect`).
        field: &'static str,
    },
    /// Two settings contradict each other; the setting is named.
    Inconsistent {
        /// What contradicts.
        what: &'static str,
    },
}

impl fmt::Display for ConfigReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigReason::EndpointSyntax(kind) => write!(f, "{kind}"),
            ConfigReason::NoEndpoints => f.write_str("the pool has no endpoints"),
            ConfigReason::DuplicateEndpoint => {
                f.write_str("the endpoint appears twice in the pool")
            }
            ConfigReason::DuplicatePool => f.write_str("another pool has the same name"),
            ConfigReason::PoolName => f.write_str(
                "a pool name is 1 to 64 ASCII letters, digits, '-', '_', or '.'",
            ),
            ConfigReason::UnknownPool => f.write_str("the route names a pool that does not exist"),
            ConfigReason::MissingPool => {
                f.write_str("the route has no pool (add .to(\"pool name\"))")
            }
            ConfigReason::NoRoutes => {
                f.write_str("add a pool, and a route for each pool when there are several")
            }
            ConfigReason::HostPattern => f.write_str(
                "the host pattern must be a host name, an address, \"*\", or \"*.\" and a host name",
            ),
            ConfigReason::PathPattern => f.write_str(
                "the path pattern must start with '/' and hold only path bytes, with no dot segment, query, or fragment",
            ),
            ConfigReason::PathRewrite => f.write_str(
                "the rewrite prefix must start the route's path and end at a segment boundary",
            ),
            ConfigReason::Shadowed { by } => {
                write!(f, "route {by} already matches every request this route would")
            }
            ConfigReason::Method => f.write_str("a method must be a token"),
            ConfigReason::HeaderName => f.write_str("a header name must be a token"),
            ConfigReason::HeaderValue => f.write_str(
                "a header value must not hold control bytes or start or end with whitespace",
            ),
            ConfigReason::ReservedHeader => f.write_str(
                "the proxy writes this field itself (framing, hop-by-hop, Host, Via, forwarding)",
            ),
            ConfigReason::Pseudonym => f.write_str(
                "a Via pseudonym must be a token; an obfuscated node is '_' and letters, digits, '.', '_', '-'",
            ),
            ConfigReason::Weight => f.write_str("a weight must be 1 to 1000"),
            ConfigReason::Limit { field } => write!(f, "{field} is out of range"),
            ConfigReason::Inconsistent { what } => write!(f, "{what}"),
        }
    }
}

/// Why an endpoint address does not parse.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EndpointErrorKind {
    /// The text is empty.
    Empty,
    /// No port, or nothing after the `:`.
    MissingPort,
    /// The port is not a number from 1 to 65,535 without a sign or a leading
    /// zero.
    BadPort,
    /// The host is not an IPv4 address, a bracketed IPv6 address, or a host
    /// name of letters, digits, and hyphens in labels of 1 to 63 bytes.
    BadHost,
    /// The text has userinfo (`user@host`).
    UserInfo,
    /// The text has a path, query, or fragment.
    Path,
    /// The text is longer than any valid endpoint.
    TooLong,
    /// A byte that cannot be in an endpoint address (whitespace, `%`,
    /// non-ASCII, and others).
    BadByte,
}

impl fmt::Display for EndpointErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            EndpointErrorKind::Empty => "the endpoint is empty (write host:port)",
            EndpointErrorKind::MissingPort => "the port is missing (write host:port)",
            EndpointErrorKind::BadPort => "the port must be 1 to 65535, without a leading zero",
            EndpointErrorKind::BadHost => {
                "the host must be an IPv4 address, [an IPv6 address], or a host name"
            }
            EndpointErrorKind::UserInfo => "an endpoint has no user name (remove \"...@\")",
            EndpointErrorKind::Path => "an endpoint has no path, query, or fragment",
            EndpointErrorKind::TooLong => "the endpoint is longer than any valid host:port",
            EndpointErrorKind::BadByte => "the endpoint holds a byte that cannot be in host:port",
        })
    }
}

#[cfg(test)]
mod tests {
    use alloc::string::ToString;

    use super::*;

    #[test]
    fn test_display_names_item_and_reason() {
        let e = ConfigError::new(
            ConfigItem::Endpoint {
                pool: Some("app".into()),
                index: Some(2),
            },
            ConfigReason::EndpointSyntax(EndpointErrorKind::BadPort),
        );
        assert_eq!(
            e.to_string(),
            "pool \"app\" endpoint 2: the port must be 1 to 65535, without a leading zero"
        );
        let e = ConfigError::new(ConfigItem::Route(3), ConfigReason::Shadowed { by: 1 });
        assert_eq!(
            e.to_string(),
            "route 3: route 1 already matches every request this route would"
        );
        let e = ConfigError::new(
            ConfigItem::Pool("db".into()),
            ConfigReason::Limit {
                field: "timeouts.connect",
            },
        );
        assert_eq!(
            e.to_string(),
            "pool \"db\": timeouts.connect is out of range"
        );
        let e = ConfigError::new(ConfigItem::Proxy, ConfigReason::NoRoutes);
        assert!(e.to_string().starts_with("proxy: "));
    }

    #[test]
    fn test_every_reason_has_a_message() {
        let reasons = [
            ConfigReason::NoEndpoints,
            ConfigReason::DuplicateEndpoint,
            ConfigReason::DuplicatePool,
            ConfigReason::PoolName,
            ConfigReason::UnknownPool,
            ConfigReason::MissingPool,
            ConfigReason::NoRoutes,
            ConfigReason::HostPattern,
            ConfigReason::PathPattern,
            ConfigReason::PathRewrite,
            ConfigReason::Method,
            ConfigReason::HeaderName,
            ConfigReason::HeaderValue,
            ConfigReason::ReservedHeader,
            ConfigReason::Pseudonym,
            ConfigReason::Weight,
            ConfigReason::Inconsistent { what: "x" },
        ];
        for r in reasons {
            assert!(!r.to_string().is_empty(), "{r:?}");
        }
        let kinds = [
            EndpointErrorKind::Empty,
            EndpointErrorKind::MissingPort,
            EndpointErrorKind::BadPort,
            EndpointErrorKind::BadHost,
            EndpointErrorKind::UserInfo,
            EndpointErrorKind::Path,
            EndpointErrorKind::TooLong,
            EndpointErrorKind::BadByte,
        ];
        for k in kinds {
            assert!(!k.to_string().is_empty(), "{k:?}");
        }
    }
}
