//! Header rules, `Host` handling, and the path safety policy of a route.

use alloc::boxed::Box;

use crate::error::ConfigReason;
use crate::syntax;

/// Fields the proxy writes or removes itself; no header rule may touch them.
///
/// Framing and hop-by-hop fields (RFC 9110 7.6.1), `Host`, `Via`, `Expect`, and
/// the default forwarding set.
pub(crate) const RESERVED: &[&str] = &[
    "connection",
    "content-length",
    "expect",
    "host",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "proxy-connection",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
    "via",
];

/// The fields removed from every request before the proxy writes its own
/// forwarding fields. [`ProxyBuilder::strip_header`](crate::ProxyBuilder::strip_header)
/// adds to this set.
pub const FORWARDING_SET: &[&str] = &[
    "forwarded",
    "x-forwarded-for",
    "x-forwarded-proto",
    "x-forwarded-host",
    "x-forwarded-port",
    "x-forwarded-prefix",
    "x-real-ip",
    "x-client-ip",
    "true-client-ip",
    "x-original-forwarded-for",
];

/// True when `name` (any case) is reserved or in the forwarding set.
pub(crate) fn is_reserved(name: &str) -> bool {
    RESERVED
        .iter()
        .chain(FORWARDING_SET)
        .any(|r| r.eq_ignore_ascii_case(name))
}

/// One change to the fields of a request or a response, applied in the order
/// the route lists them.
///
/// Names are checked to be tokens and values to be field values when the
/// configuration is built; a rule that touches a field the proxy writes itself
/// (framing, hop-by-hop, `Host`, `Via`, the forwarding fields) is refused.
///
/// ```
/// use proxy_net::{HeaderRule, Route};
/// let route = Route::any()
///     .to("app")
///     .request_header(HeaderRule::set("x-env", "production"))
///     .response_header(HeaderRule::remove("server"));
/// # let _ = route;
/// ```
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HeaderRule {
    /// Replace every field of this name with one field of this value.
    Set(Box<str>, Box<str>),
    /// Add a field, keeping any of the same name.
    Add(Box<str>, Box<str>),
    /// Remove every field of this name.
    Remove(Box<str>),
}

impl HeaderRule {
    /// Replace every `name` field with one holding `value`.
    ///
    /// ```
    /// let rule = proxy_net::HeaderRule::set("x-env", "prod");
    /// assert_eq!(rule.name(), "x-env");
    /// ```
    pub fn set(name: &str, value: &str) -> HeaderRule {
        HeaderRule::Set(syntax::lower(name), value.into())
    }

    /// Add a `name` field holding `value`.
    ///
    /// ```
    /// let rule = proxy_net::HeaderRule::add("Cache-Status", "proxy-net");
    /// assert_eq!(rule.name(), "cache-status");
    /// ```
    pub fn add(name: &str, value: &str) -> HeaderRule {
        HeaderRule::Add(syntax::lower(name), value.into())
    }

    /// Remove every `name` field.
    ///
    /// ```
    /// let rule = proxy_net::HeaderRule::remove("X-Powered-By");
    /// assert_eq!(rule.name(), "x-powered-by");
    /// ```
    pub fn remove(name: &str) -> HeaderRule {
        HeaderRule::Remove(syntax::lower(name))
    }

    /// The field name, lowercase.
    pub fn name(&self) -> &str {
        match self {
            HeaderRule::Set(n, _) | HeaderRule::Add(n, _) | HeaderRule::Remove(n) => n,
        }
    }

    /// The value, for `Set` and `Add`.
    pub fn value(&self) -> Option<&str> {
        match self {
            HeaderRule::Set(_, v) | HeaderRule::Add(_, v) => Some(v),
            HeaderRule::Remove(_) => None,
        }
    }

    pub(crate) fn validate(&self) -> Result<(), ConfigReason> {
        if !syntax::is_token(self.name().as_bytes()) {
            return Err(ConfigReason::HeaderName);
        }
        if is_reserved(self.name()) {
            return Err(ConfigReason::ReservedHeader);
        }
        if let Some(v) = self.value() {
            if !syntax::is_field_value(v.as_bytes()) {
                return Err(ConfigReason::HeaderValue);
            }
        }
        Ok(())
    }
}

/// What the upstream request's `Host` holds.
///
/// ```
/// use proxy_net::{HostHeader, Route};
/// let route = Route::host("example.com").to("app").host_header(HostHeader::fixed("internal.app"));
/// # let _ = route;
/// ```
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum HostHeader {
    /// The client's `Host`, as received (the default).
    #[default]
    Preserve,
    /// The endpoint's server name, or its address when it has none.
    Endpoint,
    /// A fixed host (and optional port), checked when the configuration is
    /// built.
    Fixed(Box<str>),
}

impl HostHeader {
    /// A fixed `Host` value.
    ///
    /// ```
    /// let h = proxy_net::HostHeader::fixed("app.internal:8080");
    /// assert_eq!(h, proxy_net::HostHeader::Fixed("app.internal:8080".into()));
    /// ```
    pub fn fixed(host: &str) -> HostHeader {
        HostHeader::Fixed(host.into())
    }

    pub(crate) fn validate(&self) -> Result<(), ConfigReason> {
        match self {
            HostHeader::Fixed(h) => {
                if crate::endpoint::parse_addr(h.as_bytes()).is_ok()
                    || syntax::host_name(h.as_bytes()).is_some()
                    || syntax::ipv4(h.as_bytes()).is_some()
                {
                    Ok(())
                } else {
                    Err(ConfigReason::HeaderValue)
                }
            }
            _ => Ok(()),
        }
    }
}

/// One kind of request path that a proxy and an origin could read
/// differently (T-7).
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PathCheck {
    /// A `.` or `..` segment, raw or percent-encoded in any case.
    DotSegment,
    /// `%2f` or `%5c`: an encoded `/` or `\`.
    EncodedSlash,
    /// A raw `\`.
    Backslash,
    /// `%00`, or a raw NUL.
    EncodedNul,
    /// `;` inside a segment matched by a prefix route (`/app/..;/admin`).
    Semicolon,
}

/// Which ambiguous request paths a route refuses with 400. The default,
/// [`PathPolicy::STRICT`], refuses all of them.
///
/// ```
/// use proxy_net::{PathCheck, PathPolicy};
/// let p = PathPolicy::STRICT.allow(PathCheck::EncodedSlash);
/// assert!(!p.refuses(PathCheck::EncodedSlash));
/// assert!(p.refuses(PathCheck::DotSegment));
/// assert!(!PathPolicy::PASS.refuses(PathCheck::DotSegment));
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PathPolicy {
    refused: u8,
}

impl PathPolicy {
    /// Refuse every ambiguous form (the default).
    pub const STRICT: PathPolicy = PathPolicy { refused: 0b1_1111 };
    /// Refuse none: the route forwards paths as received.
    pub const PASS: PathPolicy = PathPolicy { refused: 0 };

    const fn bit(check: PathCheck) -> u8 {
        match check {
            PathCheck::DotSegment => 1,
            PathCheck::EncodedSlash => 2,
            PathCheck::Backslash => 4,
            PathCheck::EncodedNul => 8,
            PathCheck::Semicolon => 16,
        }
    }

    /// The same policy with `check` allowed.
    #[must_use]
    pub const fn allow(self, check: PathCheck) -> PathPolicy {
        PathPolicy {
            refused: self.refused & !PathPolicy::bit(check),
        }
    }

    /// The same policy with `check` refused.
    ///
    /// ```
    /// use proxy_net::{PathCheck, PathPolicy};
    /// assert!(PathPolicy::PASS.refuse(PathCheck::Backslash).refuses(PathCheck::Backslash));
    /// ```
    #[must_use]
    pub const fn refuse(self, check: PathCheck) -> PathPolicy {
        PathPolicy {
            refused: self.refused | PathPolicy::bit(check),
        }
    }

    /// True when this policy refuses `check`.
    pub const fn refuses(self, check: PathCheck) -> bool {
        self.refused & PathPolicy::bit(check) != 0
    }
}

impl Default for PathPolicy {
    fn default() -> PathPolicy {
        PathPolicy::STRICT
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rules_validate() {
        assert_eq!(HeaderRule::set("x-a", "b").validate(), Ok(()));
        assert_eq!(HeaderRule::set("x-a", "").validate(), Ok(()));
        assert_eq!(HeaderRule::remove("server").validate(), Ok(()));
        assert_eq!(
            HeaderRule::set("x a", "b").validate(),
            Err(ConfigReason::HeaderName)
        );
        assert_eq!(
            HeaderRule::add("", "b").validate(),
            Err(ConfigReason::HeaderName)
        );
        for v in ["a\r\nb", "a\nb", "a\0", "a\x7f", " a", "a "] {
            assert_eq!(
                HeaderRule::set("x-a", v).validate(),
                Err(ConfigReason::HeaderValue),
                "{v:?}"
            );
        }
        for n in [
            "Content-Length",
            "transfer-encoding",
            "CONNECTION",
            "host",
            "via",
            "x-forwarded-for",
            "Forwarded",
            "expect",
            "te",
        ] {
            assert_eq!(
                HeaderRule::remove(n).validate(),
                Err(ConfigReason::ReservedHeader),
                "{n}"
            );
        }
    }

    #[test]
    fn test_rule_accessors() {
        let r = HeaderRule::add("X-A", "1");
        assert_eq!(r.name(), "x-a");
        assert_eq!(r.value(), Some("1"));
        assert_eq!(HeaderRule::remove("x").value(), None);
    }

    #[test]
    fn test_host_header_validation() {
        assert_eq!(HostHeader::Preserve.validate(), Ok(()));
        assert_eq!(HostHeader::Endpoint.validate(), Ok(()));
        for ok in ["app.internal", "app.internal:8080", "10.0.0.1", "[::1]:80"] {
            assert_eq!(HostHeader::fixed(ok).validate(), Ok(()), "{ok}");
        }
        for bad in ["", "a b", "a\r\n", "a:0", "a/b"] {
            assert_eq!(
                HostHeader::fixed(bad).validate(),
                Err(ConfigReason::HeaderValue),
                "{bad}"
            );
        }
        assert_eq!(HostHeader::default(), HostHeader::Preserve);
    }

    #[test]
    fn test_path_policy_bits() {
        let all = [
            PathCheck::DotSegment,
            PathCheck::EncodedSlash,
            PathCheck::Backslash,
            PathCheck::EncodedNul,
            PathCheck::Semicolon,
        ];
        for c in all {
            assert!(PathPolicy::STRICT.refuses(c));
            assert!(!PathPolicy::PASS.refuses(c));
            assert!(!PathPolicy::STRICT.allow(c).refuses(c));
            assert!(PathPolicy::PASS.refuse(c).refuses(c));
        }
        assert_eq!(PathPolicy::default(), PathPolicy::STRICT);
    }
}
