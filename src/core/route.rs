//! The compiled route table: host, path, and method matching without
//! allocation.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use core::net::IpAddr;

use crate::error::ConfigReason;
use crate::rewrite::{HeaderRule, HostHeader, PathPolicy};
use crate::syntax;

/// A compiled host pattern.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum HostPattern {
    Any,
    /// A host name, lowercase, without a trailing dot.
    Exact(Box<str>),
    /// `.example.com` for `*.example.com`.
    Suffix(Box<str>),
    Ip(IpAddr),
}

impl HostPattern {
    pub(crate) fn parse(p: &str) -> Result<HostPattern, ConfigReason> {
        let b = p.as_bytes();
        if b == b"*" {
            return Ok(HostPattern::Any);
        }
        if let Some(rest) = b.strip_prefix(b"*.") {
            let name = syntax::host_name(rest).ok_or(ConfigReason::HostPattern)?;
            let mut s = String::with_capacity(name.len() + 1);
            s.push('.');
            s.push_str(&name);
            return Ok(HostPattern::Suffix(s.into_boxed_str()));
        }
        if let Some(inner) = b.strip_prefix(b"[").and_then(|r| r.strip_suffix(b"]")) {
            return syntax::ipv6(inner)
                .map(|ip| HostPattern::Ip(IpAddr::V6(ip)))
                .ok_or(ConfigReason::HostPattern);
        }
        if let Some(ip) = syntax::ipv4(b) {
            return Ok(HostPattern::Ip(IpAddr::V4(ip)));
        }
        syntax::host_name(b)
            .map(HostPattern::Exact)
            .ok_or(ConfigReason::HostPattern)
    }

    /// Matches the host part of a request's `Host` (port and one trailing dot
    /// already removed).
    fn matches(&self, host: RequestHost<'_>) -> bool {
        match (self, host) {
            (HostPattern::Any, _) => true,
            (HostPattern::Exact(e), RequestHost::Name(n)) => n.eq_ignore_ascii_case(e.as_bytes()),
            (HostPattern::Suffix(s), RequestHost::Name(n)) => {
                let s = s.as_bytes();
                n.len() > s.len()
                    && n[n.len() - s.len()..].eq_ignore_ascii_case(s)
                    && n[n.len() - s.len() - 1] != b'.'
            }
            (HostPattern::Ip(a), RequestHost::Ip(b)) => *a == b,
            _ => false,
        }
    }

    /// True when `self` matches every host `other` does.
    fn covers(&self, other: &HostPattern) -> bool {
        match (self, other) {
            (HostPattern::Any, _) => true,
            (HostPattern::Suffix(s), HostPattern::Suffix(t)) => t.ends_with(&**s),
            (HostPattern::Suffix(s), HostPattern::Exact(e)) => {
                e.len() > s.len() && e.ends_with(&**s)
            }
            (HostPattern::Exact(a), HostPattern::Exact(b)) => a == b,
            (HostPattern::Ip(a), HostPattern::Ip(b)) => a == b,
            _ => false,
        }
    }
}

/// The host of a request, as matching sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RequestHost<'a> {
    Name(&'a [u8]),
    Ip(IpAddr),
}

/// Removes the port and one trailing dot from a `Host` value and reads an IP
/// literal. No allocation.
fn request_host(h: &[u8]) -> RequestHost<'_> {
    if let Some(rest) = h.strip_prefix(b"[") {
        if let Some(close) = rest.iter().position(|&b| b == b']') {
            if let Some(ip) = syntax::ipv6(&rest[..close]) {
                return RequestHost::Ip(IpAddr::V6(ip));
            }
        }
        return RequestHost::Name(h);
    }
    let name = match h.iter().rposition(|&b| b == b':') {
        Some(i) if h[i + 1..].iter().all(u8::is_ascii_digit) => &h[..i],
        _ => h,
    };
    let name = name.strip_suffix(b".").unwrap_or(name);
    match syntax::ipv4(name) {
        Some(ip) => RequestHost::Ip(IpAddr::V4(ip)),
        None => RequestHost::Name(name),
    }
}

/// A compiled path pattern.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PathPattern {
    Any,
    Exact(Box<str>),
    /// Ends with `/`: everything that starts with it.
    Under(Box<str>),
    /// No trailing `/`: itself, and everything below it.
    Segment(Box<str>),
}

impl PathPattern {
    pub(crate) fn parse(p: &str, exact: bool) -> Result<PathPattern, ConfigReason> {
        if !syntax::check_path(p.as_bytes()) {
            return Err(ConfigReason::PathPattern);
        }
        Ok(if exact {
            PathPattern::Exact(p.into())
        } else if p == "/" {
            PathPattern::Any
        } else if p.ends_with('/') {
            PathPattern::Under(p.into())
        } else {
            PathPattern::Segment(p.into())
        })
    }

    fn matches(&self, path: &[u8]) -> bool {
        match self {
            PathPattern::Any => true,
            PathPattern::Exact(e) => path == e.as_bytes(),
            PathPattern::Under(u) => path.starts_with(u.as_bytes()),
            PathPattern::Segment(s) => segment_match(s.as_bytes(), path),
        }
    }

    /// True when `self` matches every path `other` does.
    fn covers(&self, other: &PathPattern) -> bool {
        match (self, other) {
            (PathPattern::Any, _) => true,
            (_, PathPattern::Any) => false,
            (PathPattern::Exact(a), PathPattern::Exact(b)) => a == b,
            (PathPattern::Exact(_), _) => false,
            (
                PathPattern::Under(u),
                PathPattern::Exact(e) | PathPattern::Under(e) | PathPattern::Segment(e),
            ) => e.starts_with(&**u),
            (PathPattern::Segment(s), PathPattern::Exact(e) | PathPattern::Segment(e)) => {
                segment_match(s.as_bytes(), e.as_bytes())
            }
            (PathPattern::Segment(s), PathPattern::Under(e)) => {
                let (s, e) = (s.as_bytes(), e.as_bytes());
                e.len() > s.len() && e.starts_with(s) && e[s.len()] == b'/'
            }
        }
    }

    /// True for the prefix kinds, where `;` inside a segment is checked (T-7).
    pub(crate) fn is_prefix(&self) -> bool {
        matches!(
            self,
            PathPattern::Any | PathPattern::Under(_) | PathPattern::Segment(_)
        )
    }
}

fn segment_match(s: &[u8], path: &[u8]) -> bool {
    path == s || (path.len() > s.len() && path.starts_with(s) && path[s.len()] == b'/')
}

/// How a route rewrites the path before forwarding.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PathRewrite {
    /// The path is forwarded as received.
    None,
    /// The prefix is removed; an empty result becomes `/`.
    Strip(Box<str>),
    /// The first prefix is replaced with the second.
    Replace(Box<str>, Box<str>),
}

/// A route after its patterns and rules were checked.
///
/// ```
/// use proxy_net::{Pool, ProxyBuilder, Route};
/// let config = ProxyBuilder::new()
///     .pool("app", Pool::new(["10.0.0.1:80"]))
///     .route(Route::any().path("/api").to("app").websocket(true))
///     .config()?;
/// let route = config.routes().get(0).expect("route 0");
/// assert!(route.websocket());
/// assert_eq!(config.pool_at(route.pool_index()).map(|p| p.name()), Some("app"));
/// # Ok::<(), proxy_net::ConfigError>(())
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompiledRoute {
    pub(crate) host: HostPattern,
    pub(crate) path: PathPattern,
    pub(crate) methods: Option<Box<[Box<str>]>>,
    pub(crate) pool: usize,
    pub(crate) rewrite: PathRewrite,
    pub(crate) host_header: HostHeader,
    pub(crate) request_rules: Box<[HeaderRule]>,
    pub(crate) response_rules: Box<[HeaderRule]>,
    pub(crate) websocket: bool,
    pub(crate) path_policy: PathPolicy,
    pub(crate) body_methods: Box<[Box<str>]>,
}

impl CompiledRoute {
    /// The index of the route's pool in [`Config::pool_at`](crate::Config::pool_at).
    pub fn pool_index(&self) -> usize {
        self.pool
    }

    /// The path rewrite.
    pub fn rewrite(&self) -> &PathRewrite {
        &self.rewrite
    }

    /// What the upstream request's `Host` holds.
    pub fn host_header(&self) -> &HostHeader {
        &self.host_header
    }

    /// The request header rules, in order.
    pub fn request_rules(&self) -> &[HeaderRule] {
        &self.request_rules
    }

    /// The response header rules, in order.
    pub fn response_rules(&self) -> &[HeaderRule] {
        &self.response_rules
    }

    /// Whether WebSocket upgrades are allowed.
    pub fn websocket(&self) -> bool {
        self.websocket
    }

    /// The path safety policy.
    pub fn path_policy(&self) -> PathPolicy {
        self.path_policy
    }

    /// True when the route's path pattern is a prefix (so `;` in a segment is
    /// checked, T-7).
    pub fn is_prefix(&self) -> bool {
        self.path.is_prefix()
    }

    /// True when a request body is allowed on `method`: every method except
    /// `GET`, `HEAD`, `OPTIONS`, `TRACE`, and `DELETE`, which need
    /// [`Route::allow_body_on`](crate::Route::allow_body_on).
    ///
    /// ```
    /// use proxy_net::{Pool, ProxyBuilder, Route};
    /// let config = ProxyBuilder::new()
    ///     .pool("app", Pool::new(["10.0.0.1:80"]))
    ///     .route(Route::any().to("app").allow_body_on(&["DELETE"]))
    ///     .config()?;
    /// let route = config.routes().get(0).expect("route");
    /// assert!(route.allows_body_on(b"POST"));
    /// assert!(route.allows_body_on(b"DELETE"));
    /// assert!(!route.allows_body_on(b"GET"));
    /// # Ok::<(), proxy_net::ConfigError>(())
    /// ```
    pub fn allows_body_on(&self, method: &[u8]) -> bool {
        const NO_BODY: [&[u8]; 5] = [b"GET", b"HEAD", b"OPTIONS", b"TRACE", b"DELETE"];
        !NO_BODY.contains(&method) || self.body_methods.iter().any(|m| m.as_bytes() == method)
    }

    /// True when the route names `method` in its own method list (a route
    /// without a list names none).
    pub fn lists_method(&self, method: &[u8]) -> bool {
        self.methods
            .as_ref()
            .is_some_and(|list| list.iter().any(|m| m.as_bytes() == method))
    }

    fn matches_method(&self, method: &[u8]) -> bool {
        match &self.methods {
            None => true,
            Some(list) => list.iter().any(|m| m.as_bytes() == method),
        }
    }

    /// True when `self` matches every request `later` does.
    fn covers(&self, later: &CompiledRoute) -> bool {
        let methods = match (&self.methods, &later.methods) {
            (None, _) => true,
            (Some(_), None) => false,
            (Some(a), Some(b)) => b.iter().all(|m| a.contains(m)),
        };
        methods && self.host.covers(&later.host) && self.path.covers(&later.path)
    }
}

/// What [`RouteTable::find`] found.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RouteMatch<'a> {
    /// The first matching route and its position.
    Route {
        /// The route's position, from 0.
        index: usize,
        /// The route.
        route: &'a CompiledRoute,
    },
    /// A route serves this host, but none matches the path or method: 404.
    NoRoute,
    /// No route serves this host: 404, or 421 with
    /// [`UnknownHost::Misdirected`](crate::UnknownHost::Misdirected).
    UnknownHost,
}

/// The routes of a configuration, in order, ready to match requests.
///
/// ```
/// use proxy_net::{Pool, ProxyBuilder, Route};
/// use proxy_net::core::RouteMatch;
/// let config = ProxyBuilder::new()
///     .pool("api", Pool::new(["10.0.0.1:80"]))
///     .pool("web", Pool::new(["10.0.0.2:80"]))
///     .route(Route::host("example.com").path("/api").to("api"))
///     .route(Route::host("example.com").to("web"))
///     .config()?;
/// let table = config.routes();
/// assert!(matches!(table.find(b"Example.com:443", b"/api/users", b"GET"), RouteMatch::Route { index: 0, .. }));
/// assert!(matches!(table.find(b"example.com", b"/apix", b"GET"), RouteMatch::Route { index: 1, .. }));
/// assert_eq!(table.find(b"other.org", b"/", b"GET"), RouteMatch::UnknownHost);
/// # Ok::<(), proxy_net::ConfigError>(())
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct RouteTable {
    routes: Box<[CompiledRoute]>,
    /// For each host name an exact route names, sorted by the name's hash:
    /// the routes that can match it (those exact routes and every route that
    /// is not exact), in table order.
    by_host: Box<[HostEntry]>,
    /// The positions of the routes that are not exact, in table order: the
    /// candidates for any other host.
    others: Box<[u32]>,
}

/// One exact host name in the route index.
#[derive(Clone, Debug, PartialEq, Eq)]
struct HostEntry {
    hash: u64,
    name: Box<[u8]>,
    /// The routes that can match the name, in table order.
    routes: Box<[u32]>,
}

/// A hash of a host name that ignores ASCII case, eight bytes at a time.
/// Setting bit 5 of every byte folds letters to lowercase and leaves the
/// other host-name bytes (digits, `-`, `.`) as they are; for any other byte
/// it can only make two names share a hash, and the name is compared after.
fn host_hash(name: &[u8]) -> u64 {
    const FOLD: u64 = 0x2020_2020_2020_2020;
    const K: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut h = (name.len() as u64).wrapping_mul(K);
    let mut chunks = name.chunks_exact(8);
    for c in &mut chunks {
        let mut w = [0u8; 8];
        w.copy_from_slice(c);
        h = (h ^ (u64::from_le_bytes(w) | FOLD))
            .wrapping_mul(K)
            .rotate_left(29);
    }
    let rest = chunks.remainder();
    if !rest.is_empty() {
        let mut w = [0u8; 8];
        w[..rest.len()].copy_from_slice(rest);
        h = (h ^ (u64::from_le_bytes(w) | FOLD))
            .wrapping_mul(K)
            .rotate_left(29);
    }
    h ^ (h >> 32)
}

/// Set on a candidate whose host is already known to match: its exact
/// pattern is the name the index found.
const HOST_MATCHED: u32 = 1 << 31;

impl RouteTable {
    /// Builds a table, refusing a route that an earlier one shadows entirely
    /// (it could never match): the error carries the later route's index and
    /// the earlier one's.
    pub(crate) fn new(routes: Vec<CompiledRoute>) -> Result<RouteTable, (usize, ConfigReason)> {
        debug_assert!(routes.len() < HOST_MATCHED as usize);
        for (j, later) in routes.iter().enumerate() {
            if let Some(i) = routes[..j].iter().position(|r| r.covers(later)) {
                return Err((j, ConfigReason::Shadowed { by: i }));
            }
        }
        let others: Vec<u32> = routes
            .iter()
            .enumerate()
            .filter(|(_, r)| !matches!(r.host, HostPattern::Exact(_)))
            .map(|(i, _)| i as u32)
            .collect();
        let mut by_host: Vec<HostEntry> = Vec::new();
        for r in &routes {
            let HostPattern::Exact(name) = &r.host else {
                continue;
            };
            if by_host.iter().any(|e| *e.name == *name.as_bytes()) {
                continue;
            }
            let candidates: Vec<u32> = routes
                .iter()
                .enumerate()
                .filter_map(|(i, c)| match &c.host {
                    HostPattern::Exact(other) if other == name => Some(i as u32 | HOST_MATCHED),
                    HostPattern::Exact(_) => None,
                    _ => Some(i as u32),
                })
                .collect();
            by_host.push(HostEntry {
                hash: host_hash(name.as_bytes()),
                name: name.as_bytes().into(),
                routes: candidates.into_boxed_slice(),
            });
        }
        by_host.sort_by_key(|e| e.hash);
        Ok(RouteTable {
            routes: routes.into_boxed_slice(),
            by_host: by_host.into_boxed_slice(),
            others: others.into_boxed_slice(),
        })
    }

    /// The positions of the routes that can match `host`, in table order.
    fn candidates(&self, host: RequestHost<'_>) -> &[u32] {
        let RequestHost::Name(n) = host else {
            // An exact pattern never matches an address.
            return &self.others;
        };
        let h = host_hash(n);
        let start = self.by_host.partition_point(|e| e.hash < h);
        self.by_host[start..]
            .iter()
            .take_while(|e| e.hash == h)
            .find(|e| e.name.eq_ignore_ascii_case(n))
            .map_or(&*self.others, |e| &*e.routes)
    }

    /// The first route that matches `host` (a `Host` value: case, a port, and a
    /// trailing dot are ignored), `path` (as received; anything from `?` on is
    /// ignored), and `method`. Allocates nothing. Only the routes that can
    /// match the host are tried: an index built with the table maps each
    /// exact host name to its routes and every route that is not exact, in
    /// table order.
    pub fn find(&self, host: &[u8], path: &[u8], method: &[u8]) -> RouteMatch<'_> {
        let path = match path.iter().position(|&b| b == b'?') {
            Some(q) => &path[..q],
            None => path,
        };
        let host = request_host(host);
        let mut host_served = false;
        for &i in self.candidates(host) {
            let index = (i & !HOST_MATCHED) as usize;
            let Some(route) = self.routes.get(index) else {
                continue;
            };
            if i & HOST_MATCHED == 0 && !route.host.matches(host) {
                continue;
            }
            host_served = true;
            if route.path.matches(path) && route.matches_method(method) {
                return RouteMatch::Route { index, route };
            }
        }
        if host_served {
            RouteMatch::NoRoute
        } else {
            RouteMatch::UnknownHost
        }
    }

    /// The number of routes.
    pub fn len(&self) -> usize {
        self.routes.len()
    }

    /// True when the table has no routes.
    pub fn is_empty(&self) -> bool {
        self.routes.is_empty()
    }

    /// The route at `index`.
    pub fn get(&self, index: usize) -> Option<&CompiledRoute> {
        self.routes.get(index)
    }

    /// The routes in order.
    pub fn iter(&self) -> core::slice::Iter<'_, CompiledRoute> {
        self.routes.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Route;

    fn table(routes: &[Route]) -> RouteTable {
        let compiled = routes
            .iter()
            .map(|r| r.compile(0).expect("compile"))
            .collect();
        RouteTable::new(compiled).expect("table")
    }

    #[test]
    fn test_the_host_index_keeps_order_case_and_exact_names() {
        let t = table(&[
            Route::host("*.example.com").path("/wild"),
            Route::host("a-long-name.with-many.parts.example.com").path("/x"),
            Route::host("api.example.com"),
            Route::any().path("/any"),
        ]);
        // Case and length past one word of the hash.
        assert_eq!(
            index(&t, "A-LONG-NAME.With-Many.PARTS.example.com", "/x", "GET"),
            Some(1)
        );
        // A wildcard before an exact route still comes first.
        assert_eq!(
            index(
                &t,
                "a-long-name.with-many.parts.example.com",
                "/wild",
                "GET"
            ),
            Some(0)
        );
        // Bytes the hash folds together are told apart by the name.
        assert_eq!(index(&t, "api.example.com", "/", "GET"), Some(2));
        assert_eq!(index(&t, "api\x0eexample.com", "/", "GET"), None);
        // A host with no exact route falls through to the others.
        assert_eq!(index(&t, "other.org", "/any", "GET"), Some(3));
        assert_eq!(index(&t, "10.0.0.1", "/any", "GET"), Some(3));
    }

    fn index(t: &RouteTable, host: &str, path: &str, method: &str) -> Option<usize> {
        match t.find(host.as_bytes(), path.as_bytes(), method.as_bytes()) {
            RouteMatch::Route { index, .. } => Some(index),
            _ => None,
        }
    }

    #[test]
    fn test_host_patterns_parse() {
        assert_eq!(HostPattern::parse("*"), Ok(HostPattern::Any));
        assert_eq!(
            HostPattern::parse("API.example.com."),
            Ok(HostPattern::Exact("api.example.com".into()))
        );
        assert_eq!(
            HostPattern::parse("*.Example.com"),
            Ok(HostPattern::Suffix(".example.com".into()))
        );
        assert!(matches!(
            HostPattern::parse("10.0.0.1"),
            Ok(HostPattern::Ip(_))
        ));
        assert!(matches!(
            HostPattern::parse("[::1]"),
            Ok(HostPattern::Ip(_))
        ));
        for bad in [
            "", "a*.b", "*.*.b", "**", "*.", "a b", "a:80", "[::1", "[zz]", "1.2.3",
        ] {
            assert_eq!(
                HostPattern::parse(bad),
                Err(ConfigReason::HostPattern),
                "{bad}"
            );
        }
    }

    #[test]
    fn test_host_matching() {
        let t = table(&[
            Route::host("api.example.com"),
            Route::host("*.example.com").path("/w"),
            Route::host("10.0.0.1").path("/ip"),
            Route::host("[::1]").path("/v6"),
        ]);
        assert_eq!(index(&t, "API.Example.COM", "/", "GET"), Some(0));
        assert_eq!(index(&t, "api.example.com.", "/", "GET"), Some(0));
        assert_eq!(index(&t, "api.example.com:8443", "/", "GET"), Some(0));
        assert_eq!(index(&t, "a.example.com", "/w", "GET"), Some(1));
        assert_eq!(index(&t, "a.b.example.com", "/w", "GET"), Some(1));
        assert_eq!(
            t.find(b"example.com", b"/w", b"GET"),
            RouteMatch::UnknownHost
        );
        assert_eq!(
            t.find(b".example.com", b"/w", b"GET"),
            RouteMatch::UnknownHost
        );
        assert_eq!(
            t.find(b"a..example.com", b"/w", b"GET"),
            RouteMatch::UnknownHost
        );
        assert_eq!(index(&t, "10.0.0.1:80", "/ip", "GET"), Some(2));
        assert_eq!(index(&t, "[::1]:80", "/v6", "GET"), Some(3));
        assert_eq!(index(&t, "[0:0::1]", "/v6", "GET"), Some(3));
        assert_eq!(t.find(b"", b"/", b"GET"), RouteMatch::UnknownHost);
        assert_eq!(t.find(b"a.example.com", b"/x", b"GET"), RouteMatch::NoRoute);
    }

    #[test]
    fn test_path_matching() {
        let t = table(&[
            Route::any().exact("/health"),
            Route::any().path("/api"),
            Route::any().path("/static/"),
            Route::any().path("/"),
        ]);
        assert_eq!(index(&t, "h", "/health", "GET"), Some(0));
        assert_eq!(index(&t, "h", "/health/", "GET"), Some(3));
        assert_eq!(index(&t, "h", "/api", "GET"), Some(1));
        assert_eq!(index(&t, "h", "/api/", "GET"), Some(1));
        assert_eq!(index(&t, "h", "/api/x", "GET"), Some(1));
        assert_eq!(index(&t, "h", "/api?x=1", "GET"), Some(1));
        assert_eq!(index(&t, "h", "/apix", "GET"), Some(3));
        assert_eq!(index(&t, "h", "/%61pi", "GET"), Some(3));
        assert_eq!(index(&t, "h", "/static", "GET"), Some(3));
        assert_eq!(index(&t, "h", "/static/a.css", "GET"), Some(2));
    }

    #[test]
    fn test_method_matching() {
        let t = table(&[
            Route::any().path("/a").methods(&["GET", "HEAD"]),
            Route::any().path("/a"),
        ]);
        assert_eq!(index(&t, "h", "/a", "GET"), Some(0));
        assert_eq!(index(&t, "h", "/a", "get"), Some(1));
        assert_eq!(index(&t, "h", "/a", "POST"), Some(1));
    }

    #[test]
    fn test_shadowing_is_refused() {
        let compile = |routes: &[Route]| {
            RouteTable::new(routes.iter().map(|r| r.compile(0).expect("c")).collect())
        };
        let shadow = |routes: &[Route]| match compile(routes) {
            Err((j, ConfigReason::Shadowed { by })) => Some((j, by)),
            _ => None,
        };
        assert_eq!(
            shadow(&[Route::any(), Route::any().path("/a")]),
            Some((1, 0))
        );
        assert_eq!(
            shadow(&[Route::any().path("/a"), Route::any().exact("/a/b")]),
            Some((1, 0))
        );
        assert_eq!(
            shadow(&[Route::any().path("/a"), Route::any().path("/a/")]),
            Some((1, 0))
        );
        assert_eq!(
            shadow(&[Route::any().path("/a/"), Route::any().path("/a/b")]),
            Some((1, 0))
        );
        assert_eq!(
            shadow(&[Route::host("*.x.com"), Route::host("a.x.com")]),
            Some((1, 0))
        );
        assert_eq!(
            shadow(&[Route::host("*.x.com"), Route::host("*.a.x.com")]),
            Some((1, 0))
        );
        assert_eq!(
            shadow(&[
                Route::any().methods(&["GET", "POST"]),
                Route::any().methods(&["GET"])
            ]),
            Some((1, 0))
        );
        // Not shadowed: narrower first, or disjoint.
        assert!(compile(&[Route::any().path("/a"), Route::any()]).is_ok());
        assert!(compile(&[Route::any().path("/a/"), Route::any().path("/a")]).is_ok());
        assert!(compile(&[Route::host("a.x.com"), Route::host("*.x.com")]).is_ok());
        assert!(compile(&[Route::host("*.x.com"), Route::host("x.com")]).is_ok());
        assert!(compile(&[Route::any().methods(&["GET"]), Route::any()]).is_ok());
        assert!(compile(&[Route::any().exact("/a"), Route::any().exact("/a/")]).is_ok());
    }

    #[test]
    fn test_body_methods_and_accessors() {
        let r = Route::any().allow_body_on(&["GET"]).compile(3).expect("c");
        assert!(r.allows_body_on(b"GET"));
        assert!(!r.allows_body_on(b"HEAD"));
        assert!(r.allows_body_on(b"PUT"));
        assert_eq!(r.pool_index(), 3);
        assert_eq!(r.rewrite(), &PathRewrite::None);
        assert!(r.is_prefix());
        assert!(!Route::any().exact("/a").compile(0).expect("c").is_prefix());
        let t = table(&[Route::any()]);
        assert_eq!(t.len(), 1);
        assert!(!t.is_empty());
        assert_eq!(t.iter().count(), 1);
        assert!(t.get(1).is_none());
        assert!(RouteTable::default().is_empty());
    }
}
