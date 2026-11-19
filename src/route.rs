//! Routes: which requests go to which pool, and how they are rewritten.

use alloc::boxed::Box;
use alloc::vec::Vec;

use crate::core::route::{CompiledRoute, HostPattern, PathPattern, PathRewrite};
use crate::error::ConfigReason;
use crate::rewrite::{HeaderRule, HostHeader, PathPolicy};
use crate::syntax;

/// A route: the requests it matches (host, path, method) and the pool they go
/// to. Routes are tried in the order they were added; the first match wins.
///
/// Patterns are checked when the configuration is built, so a mistake is a
/// [`ConfigError`](crate::ConfigError) naming the route, never a request that
/// silently goes elsewhere.
///
/// ```
/// use proxy_net::{Pool, ProxyBuilder, Route};
/// let config = ProxyBuilder::new()
///     .pool("api", Pool::new(["10.0.0.1:8080"]))
///     .pool("web", Pool::new(["10.0.0.2:8080"]))
///     .route(Route::host("api.example.com").path("/v1").to("api").strip_prefix("/v1"))
///     .route(Route::any().to("web"))
///     .config()?;
/// assert_eq!(config.routes().len(), 2);
/// # Ok::<(), proxy_net::ConfigError>(())
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
#[must_use]
pub struct Route {
    pub(crate) host: Box<str>,
    pub(crate) path: Option<(Box<str>, bool)>,
    pub(crate) methods: Option<Vec<Box<str>>>,
    pub(crate) pool: Option<Box<str>>,
    pub(crate) rewrite: RewriteSpec,
    pub(crate) host_header: HostHeader,
    pub(crate) request_rules: Vec<HeaderRule>,
    pub(crate) response_rules: Vec<HeaderRule>,
    pub(crate) websocket: bool,
    pub(crate) path_policy: PathPolicy,
    pub(crate) body_methods: Vec<Box<str>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RewriteSpec {
    None,
    Strip(Box<str>),
    Replace(Box<str>, Box<str>),
}

impl Route {
    /// Every host and every path.
    ///
    /// ```
    /// let fallback = proxy_net::Route::any().to("web");
    /// # let _ = fallback;
    /// ```
    pub fn any() -> Route {
        Route::host("*")
    }

    /// Requests for `pattern`: a host name or address (`api.example.com`,
    /// `10.0.0.1`, `[::1]`), `*.example.com` for any name under it (one label
    /// or more, never `example.com` itself), or `*` for every host. Matching
    /// ignores case, a trailing dot, and the port.
    ///
    /// ```
    /// let r = proxy_net::Route::host("*.example.com").to("tenants");
    /// # let _ = r;
    /// ```
    pub fn host(pattern: &str) -> Route {
        Route {
            host: pattern.into(),
            path: None,
            methods: None,
            pool: None,
            rewrite: RewriteSpec::None,
            host_header: HostHeader::Preserve,
            request_rules: Vec::new(),
            response_rules: Vec::new(),
            websocket: false,
            path_policy: PathPolicy::STRICT,
            body_methods: Vec::new(),
        }
    }

    /// Paths under `pattern`, matched on the path as received (percent-encoded,
    /// never decoded). Without a trailing slash it is a segment prefix:
    /// `/api` matches `/api` and `/api/users`, never `/apix`. With one, `/api/`
    /// matches only what is below it. `/` matches every path.
    ///
    /// ```
    /// let r = proxy_net::Route::any().path("/api").to("api");
    /// # let _ = r;
    /// ```
    pub fn path(mut self, pattern: &str) -> Route {
        self.path = Some((pattern.into(), false));
        self
    }

    /// Exactly the path `pattern` (`/health` matches `/health`, not
    /// `/health/`).
    ///
    /// ```
    /// let r = proxy_net::Route::any().exact("/health").to("checks");
    /// # let _ = r;
    /// ```
    pub fn exact(mut self, pattern: &str) -> Route {
        self.path = Some((pattern.into(), true));
        self
    }

    /// Only these methods (case-sensitive, as RFC 9110 says). Default: every
    /// method.
    ///
    /// ```
    /// let r = proxy_net::Route::any().methods(&["GET", "HEAD"]).to("static");
    /// # let _ = r;
    /// ```
    pub fn methods(mut self, methods: &[&str]) -> Route {
        self.methods = Some(methods.iter().map(|&m| Box::from(m)).collect());
        self
    }

    /// The pool these requests go to.
    pub fn to(mut self, pool: &str) -> Route {
        self.pool = Some(pool.into());
        self
    }

    /// Removes `prefix` from the start of the path before forwarding; an empty
    /// result becomes `/`. The prefix must start the route's own path and end
    /// at a segment boundary.
    ///
    /// ```
    /// // "/v1/users" reaches the upstream as "/users".
    /// let r = proxy_net::Route::any().path("/v1").to("api").strip_prefix("/v1");
    /// # let _ = r;
    /// ```
    pub fn strip_prefix(mut self, prefix: &str) -> Route {
        self.rewrite = RewriteSpec::Strip(prefix.into());
        self
    }

    /// Replaces `from` at the start of the path with `to` before forwarding.
    /// `from` follows the rule of [`strip_prefix`](Route::strip_prefix); `to`
    /// is a path.
    ///
    /// ```
    /// let r = proxy_net::Route::any().path("/old").to("app").replace_prefix("/old", "/new");
    /// # let _ = r;
    /// ```
    pub fn replace_prefix(mut self, from: &str, to: &str) -> Route {
        self.rewrite = RewriteSpec::Replace(from.into(), to.into());
        self
    }

    /// What the upstream request's `Host` holds. Default: the client's.
    pub fn host_header(mut self, mode: HostHeader) -> Route {
        self.host_header = mode;
        self
    }

    /// A rule applied to the request's fields, after the proxy removed the
    /// hop-by-hop and forwarding fields and before it adds its own.
    pub fn request_header(mut self, rule: HeaderRule) -> Route {
        self.request_rules.push(rule);
        self
    }

    /// A rule applied to the response's fields.
    pub fn response_header(mut self, rule: HeaderRule) -> Route {
        self.response_rules.push(rule);
        self
    }

    /// Allow WebSocket upgrades on this route (bridged from 0.5). Default: off.
    pub fn websocket(mut self, allow: bool) -> Route {
        self.websocket = allow;
        self
    }

    /// Which ambiguous paths this route refuses. Default
    /// [`PathPolicy::STRICT`].
    ///
    /// ```
    /// use proxy_net::{PathCheck, PathPolicy, Route};
    /// let r = Route::any().to("app").path_policy(PathPolicy::STRICT.allow(PathCheck::EncodedSlash));
    /// # let _ = r;
    /// ```
    pub fn path_policy(mut self, policy: PathPolicy) -> Route {
        self.path_policy = policy;
        self
    }

    /// Methods allowed a request body although RFC 9110 gives it no meaning
    /// (`GET`, `HEAD`, `OPTIONS`, `TRACE`, `DELETE`); refused by default (T-4).
    ///
    /// ```
    /// let r = proxy_net::Route::any().to("api").allow_body_on(&["DELETE"]);
    /// # let _ = r;
    /// ```
    pub fn allow_body_on(mut self, methods: &[&str]) -> Route {
        self.body_methods = methods.iter().map(|&m| Box::from(m)).collect();
        self
    }

    /// Checks the route and builds its matcher. `pool` is the index of the
    /// named pool, found by the caller.
    pub(crate) fn compile(&self, pool: usize) -> Result<CompiledRoute, ConfigReason> {
        let host = HostPattern::parse(&self.host)?;
        let path = match &self.path {
            None => PathPattern::Any,
            Some((p, exact)) => PathPattern::parse(p, *exact)?,
        };
        let methods = match &self.methods {
            None => None,
            Some(list) => {
                if list.is_empty() || list.iter().any(|m| !syntax::is_token(m.as_bytes())) {
                    return Err(ConfigReason::Method);
                }
                Some(list.clone().into_boxed_slice())
            }
        };
        if self
            .body_methods
            .iter()
            .any(|m| !syntax::is_token(m.as_bytes()))
        {
            return Err(ConfigReason::Method);
        }
        let rewrite = match &self.rewrite {
            RewriteSpec::None => PathRewrite::None,
            RewriteSpec::Strip(prefix) => {
                check_rewrite_prefix(&path, prefix)?;
                PathRewrite::Strip(prefix.clone())
            }
            RewriteSpec::Replace(from, to) => {
                check_rewrite_prefix(&path, from)?;
                if !syntax::check_path(to.as_bytes()) {
                    return Err(ConfigReason::PathRewrite);
                }
                PathRewrite::Replace(from.clone(), to.clone())
            }
        };
        self.host_header.validate()?;
        for rule in self.request_rules.iter().chain(&self.response_rules) {
            rule.validate()?;
        }
        Ok(CompiledRoute {
            host,
            path,
            methods,
            pool,
            rewrite,
            host_header: self.host_header.clone(),
            request_rules: self.request_rules.clone().into_boxed_slice(),
            response_rules: self.response_rules.clone().into_boxed_slice(),
            websocket: self.websocket,
            path_policy: self.path_policy,
            body_methods: self.body_methods.clone().into_boxed_slice(),
        })
    }
}

/// A rewrite prefix starts the route's path pattern and ends at a segment
/// boundary of it.
fn check_rewrite_prefix(path: &PathPattern, prefix: &str) -> Result<(), ConfigReason> {
    let p = prefix.as_bytes();
    if !syntax::check_path(p) || p == b"/" {
        return Err(ConfigReason::PathRewrite);
    }
    let pattern: &[u8] = match path {
        PathPattern::Any => return Err(ConfigReason::PathRewrite),
        PathPattern::Exact(s) | PathPattern::Under(s) | PathPattern::Segment(s) => s.as_bytes(),
    };
    let Some(rest) = pattern.strip_prefix(p) else {
        return Err(ConfigReason::PathRewrite);
    };
    let boundary = rest.is_empty() || rest[0] == b'/' || p.ends_with(b"/");
    if boundary {
        Ok(())
    } else {
        Err(ConfigReason::PathRewrite)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compile_checks_each_part() {
        assert!(Route::any().compile(0).is_ok());
        assert_eq!(
            Route::host("a b").compile(0).err(),
            Some(ConfigReason::HostPattern)
        );
        assert_eq!(
            Route::any().path("api").compile(0).err(),
            Some(ConfigReason::PathPattern)
        );
        assert_eq!(
            Route::any().methods(&[]).compile(0).err(),
            Some(ConfigReason::Method)
        );
        assert_eq!(
            Route::any()
                .methods(&["GET", "BAD METHOD"])
                .compile(0)
                .err(),
            Some(ConfigReason::Method)
        );
        assert_eq!(
            Route::any().allow_body_on(&["G T"]).compile(0).err(),
            Some(ConfigReason::Method)
        );
        assert_eq!(
            Route::any()
                .request_header(HeaderRule::set("host", "x"))
                .compile(0)
                .err(),
            Some(ConfigReason::ReservedHeader)
        );
        assert_eq!(
            Route::any()
                .response_header(HeaderRule::set("x-a", "a\nb"))
                .compile(0)
                .err(),
            Some(ConfigReason::HeaderValue)
        );
        assert_eq!(
            Route::any()
                .host_header(HostHeader::fixed("bad host"))
                .compile(0)
                .err(),
            Some(ConfigReason::HeaderValue)
        );
    }

    #[test]
    fn test_rewrite_prefix_rules() {
        let ok = |path: &str, prefix: &str| {
            Route::any()
                .path(path)
                .strip_prefix(prefix)
                .compile(0)
                .is_ok()
        };
        assert!(ok("/v1", "/v1"));
        assert!(ok("/v1/users", "/v1"));
        assert!(ok("/v1/", "/v1/"));
        assert!(ok("/v1/", "/v1"));
        assert!(!ok("/v1x", "/v1"));
        assert!(!ok("/v1", "/v2"));
        assert!(!ok("/v1", "/"));
        assert!(!ok("/v1", "v1"));
        assert!(Route::any().strip_prefix("/v1").compile(0).is_err());
        assert!(
            Route::any()
                .exact("/old/a")
                .replace_prefix("/old", "/new")
                .compile(0)
                .is_ok()
        );
        assert_eq!(
            Route::any()
                .path("/old")
                .replace_prefix("/old", "new")
                .compile(0)
                .err(),
            Some(ConfigReason::PathRewrite)
        );
    }
}
