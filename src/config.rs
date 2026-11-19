//! The proxy's configuration: the builder and the validated snapshot.

use alloc::boxed::Box;
use alloc::vec::Vec;

use crate::core::RouteTable;
use crate::error::{ConfigError, ConfigItem, ConfigReason};
use crate::pool::{Pool, PoolConfig};
use crate::rewrite::{self, FORWARDING_SET};
use crate::route::Route;
use crate::syntax;

/// The most routes one configuration may hold.
pub const MAX_ROUTES: usize = 4096;

/// The default `Via` pseudonym.
pub const DEFAULT_PSEUDONYM: &str = "proxy-net";

/// Builds a proxy configuration: pools, routes, limits, and forwarding headers.
///
/// [`config`](ProxyBuilder::config) checks everything and returns the
/// validated [`Config`]; nothing is started. A builder with one pool and no
/// routes sends every request to that pool.
///
/// ```
/// use proxy_net::{Balance, Forwarded, HealthCheck, Pool, ProxyBuilder, Route};
/// let config = ProxyBuilder::new()
///     .pool("api", Pool::new(["10.0.0.1:8080", "10.0.0.2:8080"]).balance(Balance::LeastConnections))
///     .pool("web", Pool::new(["10.0.1.1:8080"]).health(HealthCheck::http("/health")))
///     .route(Route::host("api.example.com").to("api"))
///     .route(Route::any().to("web"))
///     .forwarded(Forwarded::standard())
///     .via("edge-1")
///     .config()?;
/// assert_eq!(config.via(), Some("edge-1"));
/// # Ok::<(), proxy_net::ConfigError>(())
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Default)]
#[must_use]
pub struct ProxyBuilder {
    pools: Vec<(Box<str>, Pool)>,
    routes: Vec<Route>,
    unknown_host: UnknownHost,
    limits: Limits,
    forwarded: Forwarded,
    strip: Vec<Box<str>>,
    via: Option<Option<Box<str>>>,
}

impl ProxyBuilder {
    /// An empty builder.
    pub fn new() -> ProxyBuilder {
        ProxyBuilder::default()
    }

    /// Adds a pool under `name` (1 to 64 ASCII letters, digits, `-`, `_`, `.`).
    pub fn pool(mut self, name: &str, pool: Pool) -> ProxyBuilder {
        self.pools.push((name.into(), pool));
        self
    }

    /// Adds a route; routes are tried in the order they were added.
    pub fn route(mut self, route: Route) -> ProxyBuilder {
        self.routes.push(route);
        self
    }

    /// The answer when no route serves the request's host. Default
    /// [`UnknownHost::NotFound`].
    pub fn unknown_host(mut self, answer: UnknownHost) -> ProxyBuilder {
        self.unknown_host = answer;
        self
    }

    /// Size and count limits.
    pub fn limits(mut self, limits: Limits) -> ProxyBuilder {
        self.limits = limits;
        self
    }

    /// Which forwarding fields the proxy writes. Default
    /// [`Forwarded::both`].
    pub fn forwarded(mut self, forwarded: Forwarded) -> ProxyBuilder {
        self.forwarded = forwarded;
        self
    }

    /// Adds a field to the set removed from every request before the proxy
    /// writes its own forwarding fields.
    ///
    /// ```
    /// let b = proxy_net::ProxyBuilder::new().strip_header("CF-Connecting-IP");
    /// # let _ = b;
    /// ```
    pub fn strip_header(mut self, name: &str) -> ProxyBuilder {
        self.strip.push(syntax::lower(name));
        self
    }

    /// The pseudonym this proxy writes in `Via` and looks for to detect loops.
    /// Default `proxy-net`.
    pub fn via(mut self, pseudonym: &str) -> ProxyBuilder {
        self.via = Some(Some(pseudonym.into()));
        self
    }

    /// Writes no `Via` and does no loop detection.
    pub fn no_via(mut self) -> ProxyBuilder {
        self.via = Some(None);
        self
    }

    /// Checks the configuration without keeping it.
    ///
    /// # Errors
    ///
    /// As [`config`](ProxyBuilder::config).
    pub fn check(&self) -> Result<(), ConfigError> {
        self.clone().config().map(|_| ())
    }

    /// Checks everything and returns the validated configuration.
    ///
    /// ```
    /// use proxy_net::{ConfigItem, ConfigReason, Pool, ProxyBuilder, Route};
    /// let err = ProxyBuilder::new()
    ///     .pool("app", Pool::new(["10.0.0.1:80"]))
    ///     .route(Route::any().to("other"))
    ///     .config()
    ///     .unwrap_err();
    /// assert_eq!(err.item(), &ConfigItem::Route(0));
    /// assert_eq!(err.reason(), &ConfigReason::UnknownPool);
    /// ```
    ///
    /// # Errors
    ///
    /// The first problem found, with the item it is in: a pool (name, an
    /// endpoint, a policy), a route (a pattern, a rule, its pool, shadowing),
    /// or a proxy-wide setting.
    pub fn config(self) -> Result<Config, ConfigError> {
        let proxy = |reason| ConfigError::new(ConfigItem::Proxy, reason);
        if self.pools.is_empty() || (self.pools.len() > 1 && self.routes.is_empty()) {
            return Err(proxy(ConfigReason::NoRoutes));
        }
        if self.routes.len() > MAX_ROUTES {
            return Err(proxy(ConfigReason::Limit { field: "routes" }));
        }
        self.limits.validate().map_err(proxy)?;
        self.forwarded.validate().map_err(proxy)?;
        let mut stripped: Vec<Box<str>> = FORWARDING_SET.iter().map(|&s| Box::from(s)).collect();
        for name in self.strip {
            if !syntax::is_token(name.as_bytes()) {
                return Err(proxy(ConfigReason::HeaderName));
            }
            if rewrite::RESERVED.contains(&&*name) {
                return Err(proxy(ConfigReason::ReservedHeader));
            }
            if !stripped.contains(&name) {
                stripped.push(name);
            }
        }
        let via = match self.via {
            None => Some(Box::from(DEFAULT_PSEUDONYM)),
            Some(None) => None,
            Some(Some(p)) => {
                if !syntax::is_token(p.as_bytes()) || p.len() > 64 {
                    return Err(proxy(ConfigReason::Pseudonym));
                }
                Some(p)
            }
        };
        let mut pools: Vec<PoolConfig> = Vec::with_capacity(self.pools.len());
        for (name, pool) in self.pools {
            if pools.iter().any(|p| p.name() == &*name) {
                return Err(ConfigError::new(
                    ConfigItem::Pool(name),
                    ConfigReason::DuplicatePool,
                ));
            }
            pools.push(pool.compile(&name)?);
        }
        let routes = if self.routes.is_empty() {
            alloc::vec![Route::any().to(pools[0].name())]
        } else {
            self.routes
        };
        let mut compiled = Vec::with_capacity(routes.len());
        for (index, route) in routes.iter().enumerate() {
            let at = |reason| ConfigError::new(ConfigItem::Route(index), reason);
            let pool_name = route
                .pool
                .as_deref()
                .ok_or_else(|| at(ConfigReason::MissingPool))?;
            let pool = pools
                .iter()
                .position(|p| p.name() == pool_name)
                .ok_or_else(|| at(ConfigReason::UnknownPool))?;
            compiled.push(route.compile(pool).map_err(at)?);
        }
        let routes = RouteTable::new(compiled)
            .map_err(|(index, reason)| ConfigError::new(ConfigItem::Route(index), reason))?;
        Ok(Config {
            routes,
            pools: pools.into_boxed_slice(),
            unknown_host: self.unknown_host,
            limits: self.limits,
            forwarded: self.forwarded,
            stripped: stripped.into_boxed_slice(),
            via,
        })
    }
}

/// A checked configuration: every pattern compiled, every endpoint parsed,
/// every limit in range. Immutable; a reload replaces it whole.
///
/// ```
/// use proxy_net::{Pool, ProxyBuilder};
/// let config = ProxyBuilder::new().pool("app", Pool::new(["10.0.0.1:80"])).config()?;
/// assert_eq!(config.routes().len(), 1); // one pool, no routes: everything goes there
/// assert!(config.stripped_headers().any(|h| h == "x-forwarded-for"));
/// assert_eq!(config.via(), Some("proxy-net"));
/// # Ok::<(), proxy_net::ConfigError>(())
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    routes: RouteTable,
    pools: Box<[PoolConfig]>,
    unknown_host: UnknownHost,
    limits: Limits,
    forwarded: Forwarded,
    stripped: Box<[Box<str>]>,
    via: Option<Box<str>>,
}

impl Config {
    /// The route table.
    pub fn routes(&self) -> &RouteTable {
        &self.routes
    }

    /// The pool named `name`.
    pub fn pool(&self, name: &str) -> Option<&PoolConfig> {
        self.pools.iter().find(|p| p.name() == name)
    }

    /// The pool at `index`, as a route's
    /// [`pool_index`](crate::core::CompiledRoute::pool_index) names it.
    pub fn pool_at(&self, index: usize) -> Option<&PoolConfig> {
        self.pools.get(index)
    }

    /// The pools, in the order they were added.
    pub fn pools(&self) -> &[PoolConfig] {
        &self.pools
    }

    /// The answer for an unknown host.
    pub fn unknown_host(&self) -> UnknownHost {
        self.unknown_host
    }

    /// The limits.
    pub fn limits(&self) -> &Limits {
        &self.limits
    }

    /// The forwarding fields written.
    pub fn forwarded(&self) -> &Forwarded {
        &self.forwarded
    }

    /// Every field removed from requests before the forwarding fields are
    /// written (lowercase).
    pub fn stripped_headers(&self) -> impl Iterator<Item = &str> {
        self.stripped.iter().map(|s| &**s)
    }

    /// The `Via` pseudonym, or `None` when `Via` is off.
    pub fn via(&self) -> Option<&str> {
        self.via.as_deref()
    }
}

/// The answer when no route serves a request's host.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum UnknownHost {
    /// 404 Not Found (the default).
    #[default]
    NotFound,
    /// 421 Misdirected Request: the client reached a server that does not
    /// serve this host.
    Misdirected,
}

impl UnknownHost {
    /// The status code.
    ///
    /// ```
    /// assert_eq!(proxy_net::UnknownHost::Misdirected.status(), 421);
    /// ```
    pub fn status(self) -> u16 {
        match self {
            UnknownHost::NotFound => 404,
            UnknownHost::Misdirected => 421,
        }
    }
}

/// Size and count limits of the proxy. In-flight memory is bounded by
/// `max_exchanges x (request_buffer + relay_buffer)` plus head buffers.
///
/// ```
/// let limits = proxy_net::Limits::default()
///     .max_request_body(10 * 1024 * 1024)
///     .relay_buffer(32 * 1024);
/// assert_eq!(limits.relay_buffer_value(), 32 * 1024);
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
#[must_use]
pub struct Limits {
    max_request_body: u64,
    max_response_body: Option<u64>,
    request_buffer: u32,
    relay_buffer: u32,
    max_exchanges: u32,
    max_response_head: u32,
    max_response_fields: u16,
}

impl Default for Limits {
    fn default() -> Limits {
        Limits {
            max_request_body: 100 * 1024 * 1024,
            max_response_body: None,
            request_buffer: 64 * 1024,
            relay_buffer: 64 * 1024,
            max_exchanges: 4096,
            max_response_head: 16 * 1024,
            max_response_fields: 100,
        }
    }
}

impl Limits {
    /// The largest request body forwarded; 0 refuses every request body.
    /// Default 100 MiB.
    pub fn max_request_body(mut self, bytes: u64) -> Limits {
        self.max_request_body = bytes;
        self
    }

    /// The largest response body relayed. Default: no bound.
    pub fn max_response_body(mut self, bytes: u64) -> Limits {
        self.max_response_body = Some(bytes);
        self
    }

    /// Request body bytes held before an upstream connection is taken (so a
    /// slow client does not hold an upstream connection for a small body),
    /// and the most a retry can replay. 1 KiB to 16 MiB. Default 64 KiB.
    pub fn request_buffer(mut self, bytes: u32) -> Limits {
        self.request_buffer = bytes;
        self
    }

    /// Response body bytes read from the upstream per step. 1 KiB to 16 MiB.
    /// Default 64 KiB.
    pub fn relay_buffer(mut self, bytes: u32) -> Limits {
        self.relay_buffer = bytes;
        self
    }

    /// Exchanges in flight at once. Default 4,096.
    pub fn max_exchanges(mut self, n: u32) -> Limits {
        self.max_exchanges = n;
        self
    }

    /// The largest upstream response head, 1 KiB to 1 MiB. Default 16 KiB.
    pub fn max_response_head(mut self, bytes: u32) -> Limits {
        self.max_response_head = bytes;
        self
    }

    /// The most fields in an upstream response head, 1 to 1,000. Default 100.
    pub fn max_response_fields(mut self, n: u16) -> Limits {
        self.max_response_fields = n;
        self
    }

    /// The request body limit.
    pub fn max_request_body_value(&self) -> u64 {
        self.max_request_body
    }

    /// The response body limit.
    pub fn max_response_body_value(&self) -> Option<u64> {
        self.max_response_body
    }

    /// The request buffer size.
    pub fn request_buffer_value(&self) -> u32 {
        self.request_buffer
    }

    /// The relay buffer size.
    pub fn relay_buffer_value(&self) -> u32 {
        self.relay_buffer
    }

    /// The exchange cap.
    pub fn max_exchanges_value(&self) -> u32 {
        self.max_exchanges
    }

    /// The response head limit.
    pub fn max_response_head_value(&self) -> u32 {
        self.max_response_head
    }

    /// The response field limit.
    pub fn max_response_fields_value(&self) -> u16 {
        self.max_response_fields
    }

    fn validate(&self) -> Result<(), ConfigReason> {
        let limit = |field| Err(ConfigReason::Limit { field });
        let buffer = 1024..=16 * 1024 * 1024;
        if !buffer.contains(&self.request_buffer) {
            return limit("limits.request_buffer");
        }
        if !buffer.contains(&self.relay_buffer) {
            return limit("limits.relay_buffer");
        }
        if u64::from(self.request_buffer) > self.max_request_body && self.max_request_body > 0 {
            return limit("limits.request_buffer");
        }
        if self.max_exchanges == 0 {
            return limit("limits.max_exchanges");
        }
        if !(1024..=1024 * 1024).contains(&self.max_response_head) {
            return limit("limits.max_response_head");
        }
        if !(1..=1000).contains(&self.max_response_fields) {
            return limit("limits.max_response_fields");
        }
        Ok(())
    }
}

/// Which forwarding fields the proxy writes. Whatever the style,
/// every field of the forwarding set a client sent is removed first.
///
/// ```
/// use proxy_net::{ByNode, Forwarded};
/// let f = Forwarded::standard().by(ByNode::obfuscated("_edge1"));
/// # let _ = f;
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Default)]
#[must_use]
pub struct Forwarded {
    style: ForwardedStyle,
    append_trusted: bool,
    by: ByNode,
}

/// The fields written.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ForwardedStyle {
    /// `Forwarded` (RFC 7239) and `X-Forwarded-For`, `-Proto`, `-Host`
    /// (the default).
    #[default]
    Both,
    /// `Forwarded` only.
    Standard,
    /// `X-Forwarded-For`, `-Proto`, `-Host` only.
    XForwarded,
    /// None.
    Off,
}

impl Forwarded {
    /// Both styles (the default).
    pub fn both() -> Forwarded {
        Forwarded::with_style(ForwardedStyle::Both)
    }

    /// `Forwarded` only.
    pub fn standard() -> Forwarded {
        Forwarded::with_style(ForwardedStyle::Standard)
    }

    /// `X-Forwarded-*` only.
    pub fn x_forwarded() -> Forwarded {
        Forwarded::with_style(ForwardedStyle::XForwarded)
    }

    /// No forwarding fields (the client-sent ones are still removed).
    pub fn off() -> Forwarded {
        Forwarded::with_style(ForwardedStyle::Off)
    }

    fn with_style(style: ForwardedStyle) -> Forwarded {
        Forwarded {
            style,
            append_trusted: false,
            by: ByNode::Off,
        }
    }

    /// Keep the elements added by trusted proxies in front of this one and
    /// append this hop (from 0.5).
    pub fn append_trusted(mut self) -> Forwarded {
        self.append_trusted = true;
        self
    }

    /// Write a `by=` element naming this proxy.
    pub fn by(mut self, node: ByNode) -> Forwarded {
        self.by = node;
        self
    }

    /// The style.
    pub fn style(&self) -> ForwardedStyle {
        self.style
    }

    /// Whether trusted elements are kept.
    pub fn appends_trusted(&self) -> bool {
        self.append_trusted
    }

    /// The `by=` node.
    pub fn by_node(&self) -> &ByNode {
        &self.by
    }

    fn validate(&self) -> Result<(), ConfigReason> {
        if self.style == ForwardedStyle::Off && (self.append_trusted || self.by != ByNode::Off) {
            return Err(ConfigReason::Inconsistent {
                what: "forwarded: append_trusted and by need a style other than off",
            });
        }
        if self.by != ByNode::Off && self.style == ForwardedStyle::XForwarded {
            return Err(ConfigReason::Inconsistent {
                what: "forwarded: by is written only in Forwarded, not X-Forwarded-*",
            });
        }
        if let ByNode::Obfuscated(name) = &self.by {
            if !valid_obfuscated(name.as_bytes()) {
                return Err(ConfigReason::Pseudonym);
            }
        }
        Ok(())
    }
}

/// What `by=` says in `Forwarded`.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum ByNode {
    /// No `by=` (the default).
    #[default]
    Off,
    /// The address of the listener the request arrived on.
    Listener,
    /// An obfuscated name (`_` then letters, digits, `.`, `_`, `-`; RFC 7239
    /// 6.3), so the address stays private.
    Obfuscated(Box<str>),
}

impl ByNode {
    /// An obfuscated node name, checked when the configuration is built.
    pub fn obfuscated(name: &str) -> ByNode {
        ByNode::Obfuscated(name.into())
    }
}

/// RFC 7239 6.3 `obfnode`: `_` then one or more of ALPHA, DIGIT, `.`, `_`, `-`.
fn valid_obfuscated(s: &[u8]) -> bool {
    s.len() >= 2
        && s.len() <= 64
        && s[0] == b'_'
        && s[1..]
            .iter()
            .all(|&b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::HeaderRule;

    fn err(b: ProxyBuilder) -> ConfigError {
        b.config().expect_err("refused")
    }

    fn one() -> ProxyBuilder {
        ProxyBuilder::new().pool("app", Pool::new(["10.0.0.1:80"]))
    }

    #[test]
    fn test_one_pool_without_routes_gets_a_default_route() {
        let c = one().config().expect("ok");
        assert_eq!(c.routes().len(), 1);
        assert_eq!(c.routes().get(0).map(|r| r.pool_index()), Some(0));
        assert_eq!(c.pools().len(), 1);
        assert_eq!(c.pool("app").map(PoolConfig::name), Some("app"));
        assert!(c.pool("x").is_none());
        assert_eq!(c.unknown_host(), UnknownHost::NotFound);
        assert_eq!(c.limits(), &Limits::default());
        assert_eq!(c.forwarded(), &Forwarded::both());
    }

    #[test]
    fn test_pools_and_routes_are_required() {
        assert_eq!(err(ProxyBuilder::new()).reason(), &ConfigReason::NoRoutes);
        let two = one().pool("b", Pool::new(["10.0.0.2:80"]));
        assert_eq!(err(two.clone()).reason(), &ConfigReason::NoRoutes);
        assert!(two.route(Route::any().to("b")).config().is_ok());
        let dup = one()
            .pool("app", Pool::new(["10.0.0.2:80"]))
            .route(Route::any().to("app"));
        let e = err(dup);
        assert_eq!(e.item(), &ConfigItem::Pool("app".into()));
        assert_eq!(e.reason(), &ConfigReason::DuplicatePool);
    }

    #[test]
    fn test_route_errors_name_the_route() {
        let e = err(one()
            .route(Route::any().to("app"))
            .route(Route::any().path("/a")));
        assert_eq!(e.item(), &ConfigItem::Route(1));
        assert_eq!(e.reason(), &ConfigReason::MissingPool);
        let e = err(one()
            .route(Route::any().path("/a").to("app"))
            .route(Route::any().path("/a/b").to("app")));
        assert_eq!(e.item(), &ConfigItem::Route(1));
        assert_eq!(e.reason(), &ConfigReason::Shadowed { by: 0 });
        let e = err(one().route(
            Route::any()
                .to("app")
                .request_header(HeaderRule::set("via", "x")),
        ));
        assert_eq!(e.item(), &ConfigItem::Route(0));
        assert_eq!(e.reason(), &ConfigReason::ReservedHeader);
    }

    #[test]
    fn test_strip_and_via() {
        let c = one()
            .strip_header("CF-Connecting-IP")
            .strip_header("x-real-ip")
            .config()
            .expect("ok");
        let names: Vec<&str> = c.stripped_headers().collect();
        assert!(names.contains(&"cf-connecting-ip"));
        assert_eq!(names.iter().filter(|n| **n == "x-real-ip").count(), 1);
        assert_eq!(
            err(one().strip_header("a b")).reason(),
            &ConfigReason::HeaderName
        );
        assert_eq!(
            err(one().strip_header("Host")).reason(),
            &ConfigReason::ReservedHeader
        );
        assert_eq!(one().no_via().config().expect("ok").via(), None);
        assert_eq!(
            one().via("edge-1").config().expect("ok").via(),
            Some("edge-1")
        );
        assert_eq!(err(one().via("a b")).reason(), &ConfigReason::Pseudonym);
        assert_eq!(err(one().via("")).reason(), &ConfigReason::Pseudonym);
        assert_eq!(
            err(one().via(&"a".repeat(65))).reason(),
            &ConfigReason::Pseudonym
        );
    }

    #[test]
    fn test_limits_validation() {
        let l = |limits: Limits| {
            one()
                .limits(limits)
                .config()
                .map(|_| ())
                .map_err(|e| e.reason().clone())
        };
        assert!(l(Limits::default()).is_ok());
        assert!(l(Limits::default().max_response_body(1)).is_ok());
        let field = |f| Err(ConfigReason::Limit { field: f });
        assert_eq!(
            l(Limits::default().request_buffer(1023)),
            field("limits.request_buffer")
        );
        assert_eq!(
            l(Limits::default().relay_buffer(17 * 1024 * 1024)),
            field("limits.relay_buffer")
        );
        assert_eq!(
            l(Limits::default().max_request_body(1024)),
            field("limits.request_buffer")
        );
        assert!(l(Limits::default().max_request_body(0)).is_ok());
        assert_eq!(
            l(Limits::default().max_exchanges(0)),
            field("limits.max_exchanges")
        );
        assert_eq!(
            l(Limits::default().max_response_head(1023)),
            field("limits.max_response_head")
        );
        assert_eq!(
            l(Limits::default().max_response_fields(0)),
            field("limits.max_response_fields")
        );
        assert_eq!(
            l(Limits::default().max_response_fields(1001)),
            field("limits.max_response_fields")
        );
        let d = Limits::default();
        assert_eq!(d.max_request_body_value(), 100 * 1024 * 1024);
        assert_eq!(d.max_response_body_value(), None);
        assert_eq!(d.request_buffer_value(), 64 * 1024);
        assert_eq!(d.max_exchanges_value(), 4096);
        assert_eq!(d.max_response_head_value(), 16 * 1024);
        assert_eq!(d.max_response_fields_value(), 100);
        let many = (0..=MAX_ROUTES).fold(one(), |b, i| {
            b.route(Route::any().exact(&alloc::format!("/{i}")).to("app"))
        });
        assert_eq!(err(many).reason(), &ConfigReason::Limit { field: "routes" });
    }

    #[test]
    fn test_forwarded_validation() {
        let f = |fw: Forwarded| {
            one()
                .forwarded(fw)
                .config()
                .map(|_| ())
                .map_err(|e| e.reason().clone())
        };
        assert!(f(Forwarded::standard().by(ByNode::Listener)).is_ok());
        assert!(f(Forwarded::both().by(ByNode::obfuscated("_edge.1-a"))).is_ok());
        assert!(f(Forwarded::both().append_trusted()).is_ok());
        assert!(matches!(
            f(Forwarded::off().append_trusted()),
            Err(ConfigReason::Inconsistent { .. })
        ));
        assert!(matches!(
            f(Forwarded::off().by(ByNode::Listener)),
            Err(ConfigReason::Inconsistent { .. })
        ));
        assert!(matches!(
            f(Forwarded::x_forwarded().by(ByNode::Listener)),
            Err(ConfigReason::Inconsistent { .. })
        ));
        for bad in ["edge", "_", "_a b", "_a/"] {
            assert_eq!(
                f(Forwarded::standard().by(ByNode::obfuscated(bad))),
                Err(ConfigReason::Pseudonym),
                "{bad}"
            );
        }
        let fw = Forwarded::x_forwarded().append_trusted();
        assert_eq!(fw.style(), ForwardedStyle::XForwarded);
        assert!(fw.appends_trusted());
        assert_eq!(fw.by_node(), &ByNode::Off);
        assert_eq!(UnknownHost::NotFound.status(), 404);
        assert_eq!(
            one()
                .unknown_host(UnknownHost::Misdirected)
                .config()
                .expect("ok")
                .unknown_host()
                .status(),
            421
        );
    }

    #[test]
    fn test_check_does_not_consume() {
        let b = one();
        assert!(b.check().is_ok());
        assert!(b.config().is_ok());
        assert!(ProxyBuilder::new().check().is_err());
    }
}
