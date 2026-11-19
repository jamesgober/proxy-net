//! Request checks before forwarding: ambiguous paths and agreement checks
//! (bodies and methods a lenient hop could read differently).

use super::class::{ErrorClass, RefusalKind};
use super::route::CompiledRoute;
use crate::rewrite::{PathCheck, PathPolicy};
use crate::syntax;

/// Checks a request path (the query is ignored) against `policy`.
/// `prefix_route` says the route matched by a prefix, where `;` inside a
/// segment is checked (`/app/..;/admin` reads as `/admin` to some origins).
/// One pass over the bytes.
///
/// ```
/// use proxy_net::core::check_path;
/// use proxy_net::{PathCheck, PathPolicy};
/// assert_eq!(check_path(b"/a/b?x=..", PathPolicy::STRICT, true), Ok(()));
/// assert_eq!(check_path(b"/a/%2E%2e/b", PathPolicy::STRICT, true), Err(PathCheck::DotSegment));
/// assert_eq!(check_path(b"/a%2fb", PathPolicy::STRICT, true), Err(PathCheck::EncodedSlash));
/// assert_eq!(check_path(b"/a%2fb", PathPolicy::STRICT.allow(PathCheck::EncodedSlash), true), Ok(()));
/// ```
///
/// # Errors
///
/// The first refused [`PathCheck`] found.
pub fn check_path(path: &[u8], policy: PathPolicy, prefix_route: bool) -> Result<(), PathCheck> {
    let path = match path.iter().position(|&b| b == b'?') {
        Some(q) => &path[..q],
        None => path,
    };
    let refuse = |c: PathCheck| if policy.refuses(c) { Err(c) } else { Ok(()) };
    let mut i = 0;
    while i < path.len() {
        match path[i] {
            b'\\' => refuse(PathCheck::Backslash)?,
            0 => refuse(PathCheck::EncodedNul)?,
            b';' if prefix_route => refuse(PathCheck::Semicolon)?,
            b'%' => {
                let pair = (path.get(i + 1), path.get(i + 2));
                if let (Some(h), Some(l)) = pair {
                    let v = hex(*h).zip(hex(*l)).map(|(h, l)| h * 16 + l);
                    match v {
                        Some(0x2f | 0x5c) => refuse(PathCheck::EncodedSlash)?,
                        Some(0) => refuse(PathCheck::EncodedNul)?,
                        _ => {}
                    }
                }
            }
            _ => {}
        }
        i += 1;
    }
    if path.split(|&b| b == b'/').any(syntax::is_dot_segment) {
        refuse(PathCheck::DotSegment)?;
    }
    Ok(())
}

fn hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// The agreement checks on a request before it is forwarded: `CONNECT` is
/// refused (a reverse proxy has no tunnels), `TRACE` is refused unless the
/// route lists it in its methods, and a body on `GET`, `HEAD`, `OPTIONS`,
/// `TRACE`, or `DELETE` is refused unless the route allows it. Then the path
/// check of the route's policy.
///
/// ```
/// use proxy_net::core::{check_request, ErrorClass, RefusalKind};
/// use proxy_net::{Pool, ProxyBuilder};
/// let config = ProxyBuilder::new().pool("app", Pool::new(["10.0.0.1:80"])).config()?;
/// let route = config.routes().get(0).expect("route");
/// assert_eq!(check_request(b"POST", b"/x", true, route), Ok(()));
/// assert_eq!(
///     check_request(b"GET", b"/x", true, route),
///     Err(ErrorClass::Refused(RefusalKind::BodyNotAllowed)),
/// );
/// assert_eq!(
///     check_request(b"GET", b"/a/../b", false, route),
///     Err(ErrorClass::Refused(RefusalKind::AmbiguousPath)),
/// );
/// # Ok::<(), proxy_net::ConfigError>(())
/// ```
///
/// # Errors
///
/// `Refused(Method)`, `Refused(BodyNotAllowed)`, or `Refused(AmbiguousPath)`.
pub fn check_request(
    method: &[u8],
    path: &[u8],
    has_body: bool,
    route: &CompiledRoute,
) -> Result<(), ErrorClass> {
    if method == b"CONNECT" || (method == b"TRACE" && !route.lists_method(method)) {
        return Err(ErrorClass::Refused(RefusalKind::Method));
    }
    if has_body && !route.allows_body_on(method) {
        return Err(ErrorClass::Refused(RefusalKind::BodyNotAllowed));
    }
    check_path(path, route.path_policy(), route.is_prefix())
        .map_err(|_| ErrorClass::Refused(RefusalKind::AmbiguousPath))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strict(p: &str) -> Result<(), PathCheck> {
        check_path(p.as_bytes(), PathPolicy::STRICT, true)
    }

    #[test]
    fn test_path_cases() {
        // E-50 to E-62.
        assert_eq!(strict("/a/../b"), Err(PathCheck::DotSegment));
        assert_eq!(strict("/a/./b"), Err(PathCheck::DotSegment));
        assert_eq!(strict("/a/%2e%2e/b"), Err(PathCheck::DotSegment));
        assert_eq!(strict("/a/.%2E/b"), Err(PathCheck::DotSegment));
        assert_eq!(strict("/a/%2E./b"), Err(PathCheck::DotSegment));
        assert_eq!(strict("/a%2fb"), Err(PathCheck::EncodedSlash));
        assert_eq!(strict("/a%2Fb"), Err(PathCheck::EncodedSlash));
        assert_eq!(strict("/a%5cb"), Err(PathCheck::EncodedSlash));
        assert_eq!(strict("/a\\b"), Err(PathCheck::Backslash));
        assert_eq!(strict("/a%00b"), Err(PathCheck::EncodedNul));
        // Found by fuzzing: a raw NUL. serve-net refuses one in a target
        // first, but the core does not rely on that.
        assert_eq!(strict(")\0"), Err(PathCheck::EncodedNul));
        assert_eq!(strict("/a/..;/b"), Err(PathCheck::Semicolon));
        assert_eq!(strict("/a;x/b"), Err(PathCheck::Semicolon));
        assert_eq!(check_path(b"/a;x/b", PathPolicy::STRICT, false), Ok(()));
        assert_eq!(strict("/a/.."), Err(PathCheck::DotSegment));
        assert_eq!(strict("/a%2"), Ok(()));
        assert_eq!(strict("/"), Ok(()));
        assert_eq!(strict("/a/...b/.c/%2e%2e%2e"), Ok(()));
        assert_eq!(strict("/a?b=../%2f"), Ok(()));
        assert_eq!(check_path(b"/a/../%2f\\", PathPolicy::PASS, true), Ok(()));
        let p = PathPolicy::STRICT
            .allow(PathCheck::DotSegment)
            .allow(PathCheck::Backslash);
        assert_eq!(check_path(b"/a/../\\", p, true), Ok(()));
        assert_eq!(
            check_path(b"/a/../\\%00", p, true),
            Err(PathCheck::EncodedNul)
        );
    }
}
