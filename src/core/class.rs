//! What ended an exchange, and what each kind of ending means for the
//! answer, retries, and health.

use core::fmt;

/// Which timeout expired.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TimeoutKind {
    /// Connecting (TLS included).
    Connect,
    /// Waiting for the response head after the request was sent.
    ResponseHead,
    /// Waiting between two reads from the upstream.
    ReadIdle,
    /// A write to the upstream that made no progress.
    WriteIdle,
    /// The whole exchange.
    Total,
}

/// Which limit was passed.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LimitKind {
    /// The request body is larger than `Limits::max_request_body`.
    RequestBody,
    /// The response body is larger than `Limits::max_response_body`.
    ResponseBody,
    /// The upstream's response head is too large or has too many fields.
    ResponseHead,
}

/// Why the proxy refused a request before forwarding it.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RefusalKind {
    /// The path is one a proxy and an origin could read differently.
    AmbiguousPath,
    /// A body on a method that gives it no meaning (`GET`, `HEAD`, `OPTIONS`,
    /// `TRACE`, `DELETE`).
    BodyNotAllowed,
    /// `TRACE` or `CONNECT`.
    Method,
    /// No route serves the host and path.
    NoRoute,
    /// The request's host is not served here (answered 404 or 421).
    UnknownHost,
    /// A field value the proxy would forward is not a valid field value.
    InvalidField,
}

/// What ended an exchange.
///
/// The class decides the status the client gets (`status`), whether a retry
/// is possible (`is_retryable`), and whether the endpoint's health counts it
/// (`counts_against_health`).
///
/// ```
/// use proxy_net::core::{ErrorClass, TimeoutKind};
/// assert_eq!(ErrorClass::Connect.status(), Some(502));
/// assert_eq!(ErrorClass::Timeout(TimeoutKind::ResponseHead).status(), Some(504));
/// assert!(ErrorClass::Connect.is_retryable());
/// assert!(!ErrorClass::Timeout(TimeoutKind::ResponseHead).is_retryable());
/// assert_eq!(ErrorClass::ClientGone.status(), None);
/// ```
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ErrorClass {
    /// The connection to the upstream failed.
    Connect,
    /// The TLS handshake with the upstream failed.
    Tls,
    /// A timeout expired.
    Timeout(TimeoutKind),
    /// The upstream's response could not be parsed.
    UpstreamProtocol,
    /// The upstream reset or closed the connection before the response ended.
    UpstreamReset,
    /// The upstream answered with a status the pool retries (`Retry::on_status`).
    UpstreamStatus,
    /// The client went away.
    ClientGone,
    /// A limit was passed.
    Limit(LimitKind),
    /// No endpoint of the pool can take the request.
    NoEndpoint,
    /// An endpoint cap or the pending queue is full.
    Breaker,
    /// The request came back to this proxy (its `Via` names this proxy).
    Loop,
    /// The proxy refused the request.
    Refused(RefusalKind),
    /// The upstream left bytes after a complete response: it is
    /// desynchronised, or not speaking HTTP/1.1 as framed.
    Desync,
}

impl ErrorClass {
    /// The status the client gets when no response byte has been relayed yet,
    /// or `None` when no answer is sent (the client is gone).
    pub fn status(self) -> Option<u16> {
        Some(match self {
            ErrorClass::Connect
            | ErrorClass::Tls
            | ErrorClass::UpstreamProtocol
            | ErrorClass::UpstreamReset
            | ErrorClass::UpstreamStatus
            | ErrorClass::Desync => 502,
            ErrorClass::Timeout(_) => 504,
            ErrorClass::Limit(LimitKind::RequestBody) => 413,
            ErrorClass::Limit(_) => 502,
            ErrorClass::NoEndpoint | ErrorClass::Breaker => 503,
            ErrorClass::Loop => 508,
            ErrorClass::Refused(RefusalKind::Method) => 405,
            ErrorClass::Refused(RefusalKind::NoRoute | RefusalKind::UnknownHost) => 404,
            ErrorClass::Refused(_) => 400,
            ErrorClass::ClientGone => return None,
        })
    }

    /// True for the failures a retry may fix: the request certainly did not
    /// reach an application (connect, TLS), or the connection ended before
    /// any response byte (reset), or the upstream asked for it by status.
    /// Timeouts after the request was sent are not retried: the upstream may
    /// still be working on it.
    pub fn is_retryable(self) -> bool {
        matches!(
            self,
            ErrorClass::Connect
                | ErrorClass::Tls
                | ErrorClass::UpstreamReset
                | ErrorClass::UpstreamStatus
                | ErrorClass::Timeout(TimeoutKind::Connect)
        )
    }

    /// True for the failures that count against the endpoint's passive
    /// health: the endpoint failed, not the client or the proxy's policy.
    pub fn counts_against_health(self) -> bool {
        matches!(
            self,
            ErrorClass::Connect
                | ErrorClass::Tls
                | ErrorClass::UpstreamProtocol
                | ErrorClass::UpstreamReset
                | ErrorClass::UpstreamStatus
                | ErrorClass::Desync
                | ErrorClass::Timeout(_)
        )
    }
}

impl fmt::Display for ErrorClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            ErrorClass::Connect => "the upstream could not be reached",
            ErrorClass::Tls => "the TLS handshake with the upstream failed",
            ErrorClass::Timeout(TimeoutKind::Connect) => "connecting to the upstream timed out",
            ErrorClass::Timeout(TimeoutKind::ResponseHead) => "the upstream did not answer in time",
            ErrorClass::Timeout(TimeoutKind::ReadIdle) => "the upstream stopped sending",
            ErrorClass::Timeout(TimeoutKind::WriteIdle) => "the upstream stopped reading",
            ErrorClass::Timeout(TimeoutKind::Total) => "the exchange took too long",
            ErrorClass::UpstreamProtocol => "the upstream sent a malformed response",
            ErrorClass::UpstreamReset => "the upstream closed the connection",
            ErrorClass::UpstreamStatus => "the upstream answered with an error",
            ErrorClass::ClientGone => "the client went away",
            ErrorClass::Limit(LimitKind::RequestBody) => "the request body is too large",
            ErrorClass::Limit(LimitKind::ResponseBody) => "the response body is too large",
            ErrorClass::Limit(LimitKind::ResponseHead) => {
                "the upstream's response head is too large"
            }
            ErrorClass::NoEndpoint => "no upstream is available",
            ErrorClass::Breaker => "the upstreams are at capacity",
            ErrorClass::Loop => "loop detected",
            ErrorClass::Refused(RefusalKind::AmbiguousPath) => "the request path is ambiguous",
            ErrorClass::Refused(RefusalKind::BodyNotAllowed) => "this method does not take a body",
            ErrorClass::Refused(RefusalKind::Method) => "method not allowed",
            ErrorClass::Refused(RefusalKind::NoRoute) => "not found",
            ErrorClass::Refused(RefusalKind::UnknownHost) => "unknown host",
            ErrorClass::Refused(RefusalKind::InvalidField) => "a request field is not valid",
            ErrorClass::Desync => "the upstream sent bytes after its response",
        })
    }
}

/// The result of one attempt, as health and balancing see it.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Outcome {
    /// The endpoint answered (any status not retried on).
    Success,
    /// The endpoint failed in a way that counts against its health.
    Failure,
    /// Something not the endpoint's fault ended the attempt (the client left,
    /// a policy refused it).
    Neutral,
}

impl From<ErrorClass> for Outcome {
    fn from(class: ErrorClass) -> Outcome {
        if class.counts_against_health() {
            Outcome::Failure
        } else {
            Outcome::Neutral
        }
    }
}

#[cfg(test)]
mod tests {
    use alloc::string::ToString;

    use super::*;

    const ALL: [ErrorClass; 22] = [
        ErrorClass::Connect,
        ErrorClass::Tls,
        ErrorClass::Timeout(TimeoutKind::Connect),
        ErrorClass::Timeout(TimeoutKind::ResponseHead),
        ErrorClass::Timeout(TimeoutKind::ReadIdle),
        ErrorClass::Timeout(TimeoutKind::WriteIdle),
        ErrorClass::Timeout(TimeoutKind::Total),
        ErrorClass::UpstreamProtocol,
        ErrorClass::UpstreamReset,
        ErrorClass::UpstreamStatus,
        ErrorClass::ClientGone,
        ErrorClass::Limit(LimitKind::RequestBody),
        ErrorClass::Limit(LimitKind::ResponseBody),
        ErrorClass::Limit(LimitKind::ResponseHead),
        ErrorClass::NoEndpoint,
        ErrorClass::Breaker,
        ErrorClass::Loop,
        ErrorClass::Refused(RefusalKind::AmbiguousPath),
        ErrorClass::Refused(RefusalKind::BodyNotAllowed),
        ErrorClass::Refused(RefusalKind::Method),
        ErrorClass::Refused(RefusalKind::UnknownHost),
        ErrorClass::Desync,
    ];

    #[test]
    fn test_statuses() {
        assert_eq!(ErrorClass::Connect.status(), Some(502));
        assert_eq!(ErrorClass::Desync.status(), Some(502));
        assert_eq!(ErrorClass::Timeout(TimeoutKind::Total).status(), Some(504));
        assert_eq!(
            ErrorClass::Limit(LimitKind::RequestBody).status(),
            Some(413)
        );
        assert_eq!(
            ErrorClass::Limit(LimitKind::ResponseHead).status(),
            Some(502)
        );
        assert_eq!(ErrorClass::NoEndpoint.status(), Some(503));
        assert_eq!(ErrorClass::Breaker.status(), Some(503));
        assert_eq!(ErrorClass::Loop.status(), Some(508));
        assert_eq!(ErrorClass::Refused(RefusalKind::Method).status(), Some(405));
        assert_eq!(
            ErrorClass::Refused(RefusalKind::NoRoute).status(),
            Some(404)
        );
        assert_eq!(
            ErrorClass::Refused(RefusalKind::AmbiguousPath).status(),
            Some(400)
        );
        assert_eq!(
            ErrorClass::Refused(RefusalKind::InvalidField).status(),
            Some(400)
        );
        assert_eq!(ErrorClass::ClientGone.status(), None);
    }

    #[test]
    fn test_retry_and_health_classes() {
        for c in ALL {
            assert!(!c.to_string().is_empty(), "{c:?}");
            if c.is_retryable() {
                assert!(c.counts_against_health(), "{c:?}");
            }
        }
        assert!(!ErrorClass::ClientGone.counts_against_health());
        assert!(!ErrorClass::Refused(RefusalKind::Method).counts_against_health());
        assert_eq!(Outcome::from(ErrorClass::Connect), Outcome::Failure);
        assert_eq!(Outcome::from(ErrorClass::ClientGone), Outcome::Neutral);
    }
}
