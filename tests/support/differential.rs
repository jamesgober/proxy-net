//! Idea I-6: what the proxy writes upstream, read by two parsers.
//!
//! A request built from arbitrary bytes goes through the forwarder with a
//! connector that captures exactly the bytes sent upstream. Those bytes are
//! parsed by http-net and by `reference`, a strict RFC 9112 reader written
//! here independently; both must accept them and agree on the method, the
//! target, every field, and the body. Checked as well: no hop-by-hop or
//! client framing field survives, one `Host`, and one way of framing the
//! body, matching the body sent.
//!
//! Shared by `tests/differential.rs` (a deterministic run in CI) and the
//! `differential` fuzz target.

#![allow(dead_code)]

use std::cell::RefCell;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use http_net::{ServerConn, ServerEvent};
use proxy_net::client::{Connector, RequestParts, UpstreamIo};
use proxy_net::core::{BodyFacts, ErrorClass, FieldList, TimeoutKind};
use proxy_net::{EndpointAddr, Pool, PoolLimits, Proxy, ProxyBuilder, Retry, Timeouts};

/// A request as a parser saw it.
#[derive(Debug, PartialEq, Eq)]
pub struct Parsed {
    pub method: String,
    pub target: String,
    /// Names lowercased, values without surrounding whitespace, in order.
    pub fields: Vec<(String, Vec<u8>)>,
    pub body: Vec<u8>,
}

fn is_tchar(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b)
}

fn trim(v: &[u8]) -> &[u8] {
    let start = v
        .iter()
        .position(|b| *b != b' ' && *b != b'\t')
        .unwrap_or(v.len());
    let end = v
        .iter()
        .rposition(|b| *b != b' ' && *b != b'\t')
        .map_or(start, |i| i + 1);
    &v[start..end.max(start)]
}

/// The independent reader: strict request line, fields without obs-fold,
/// and framing by `Transfer-Encoding: chunked` (alone) or one
/// `Content-Length`. Errors name what was wrong.
pub fn reference(bytes: &[u8]) -> Result<Parsed, String> {
    let line_end = |from: usize| -> Result<usize, String> {
        bytes[from..]
            .windows(2)
            .position(|w| w == b"\r\n")
            .map(|i| from + i)
            .ok_or_else(|| "a line without CRLF".to_string())
    };
    let first = line_end(0)?;
    let line = &bytes[..first];
    let mut parts = line.split(|b| *b == b' ');
    let method = parts.next().ok_or("no method")?;
    let target = parts.next().ok_or("no target")?;
    let version = parts.next().ok_or("no version")?;
    if parts.next().is_some() || method.is_empty() || !method.iter().all(|b| is_tchar(*b)) {
        return Err(format!(
            "bad request line {:?}",
            String::from_utf8_lossy(line)
        ));
    }
    if version != b"HTTP/1.1" {
        return Err("not HTTP/1.1".into());
    }
    if target.is_empty() || !target.iter().all(|b| (0x21..0x7f).contains(b)) {
        return Err(format!("bad target {:?}", String::from_utf8_lossy(target)));
    }
    let mut at = first + 2;
    let mut fields = Vec::new();
    loop {
        let end = line_end(at)?;
        if end == at {
            at += 2;
            break;
        }
        let f = &bytes[at..end];
        if f[0] == b' ' || f[0] == b'\t' {
            return Err("obs-fold".into());
        }
        let colon = f
            .iter()
            .position(|b| *b == b':')
            .ok_or("a field without ':'")?;
        let name = &f[..colon];
        if name.is_empty() || !name.iter().all(|b| is_tchar(*b)) {
            return Err(format!(
                "bad field name {:?}",
                String::from_utf8_lossy(name)
            ));
        }
        let value = trim(&f[colon + 1..]);
        if value.iter().any(|b| {
            *b == b'\r' || *b == b'\n' || *b == 0 || (*b < 0x20 && *b != b'\t') || *b == 0x7f
        }) {
            return Err("a control byte in a value".into());
        }
        fields.push((
            String::from_utf8_lossy(name).to_ascii_lowercase(),
            value.to_vec(),
        ));
        at = end + 2;
    }
    let te: Vec<&Vec<u8>> = fields
        .iter()
        .filter(|(n, _)| n == "transfer-encoding")
        .map(|(_, v)| v)
        .collect();
    let cl: Vec<&Vec<u8>> = fields
        .iter()
        .filter(|(n, _)| n == "content-length")
        .map(|(_, v)| v)
        .collect();
    if !te.is_empty() && !cl.is_empty() {
        return Err("both Transfer-Encoding and Content-Length".into());
    }
    let mut body = Vec::new();
    if !te.is_empty() {
        if te.len() != 1 || !te[0].eq_ignore_ascii_case(b"chunked") {
            return Err("a Transfer-Encoding other than chunked alone".into());
        }
        loop {
            let end = line_end(at)?;
            let size_part = bytes[at..end].split(|b| *b == b';').next().unwrap_or(&[]);
            if size_part.is_empty() || !size_part.iter().all(u8::is_ascii_hexdigit) {
                return Err("a bad chunk size".into());
            }
            let size = usize::from_str_radix(
                std::str::from_utf8(size_part).map_err(|e| e.to_string())?,
                16,
            )
            .map_err(|e| e.to_string())?;
            at = end + 2;
            if size == 0 {
                // Trailers, then the empty line.
                loop {
                    let end = line_end(at)?;
                    let done = end == at;
                    at = end + 2;
                    if done {
                        break;
                    }
                }
                break;
            }
            if bytes.len() < at + size + 2 || &bytes[at + size..at + size + 2] != b"\r\n" {
                return Err("a chunk without its CRLF".into());
            }
            body.extend_from_slice(&bytes[at..at + size]);
            at += size + 2;
        }
    } else if !cl.is_empty() {
        if cl.len() != 1 || cl[0].is_empty() || !cl[0].iter().all(u8::is_ascii_digit) {
            return Err("a bad Content-Length".into());
        }
        let n: usize = std::str::from_utf8(cl[0])
            .map_err(|e| e.to_string())?
            .parse()
            .map_err(|_| "a huge Content-Length")?;
        if bytes.len() < at + n {
            return Err("a short body".into());
        }
        body.extend_from_slice(&bytes[at..at + n]);
        at += n;
    }
    if at != bytes.len() {
        return Err(format!("{} bytes after the request", bytes.len() - at));
    }
    Ok(Parsed {
        method: String::from_utf8_lossy(method).into_owned(),
        target: String::from_utf8_lossy(target).into_owned(),
        fields,
        body,
    })
}

/// The same bytes through http-net's server side.
pub fn http_net(bytes: &[u8]) -> Result<Parsed, String> {
    let mut conn = ServerConn::new();
    let mut fed = 0;
    let mut parsed: Option<Parsed> = None;
    loop {
        match conn.poll().map_err(|e| format!("http-net: {e:?}"))? {
            ServerEvent::Request(head) => {
                parsed = Some(Parsed {
                    method: head.method_str().to_string(),
                    target: head.target().to_string(),
                    fields: head
                        .headers()
                        .map(|(n, v)| (n.to_ascii_lowercase(), trim(v).to_vec()))
                        .collect(),
                    body: Vec::new(),
                });
            }
            ServerEvent::Body(b) => {
                if let Some(p) = parsed.as_mut() {
                    p.body.extend_from_slice(b);
                }
            }
            ServerEvent::RequestEnd => break,
            ServerEvent::NeedInput => {
                if fed == bytes.len() {
                    return Err("http-net wants more input".into());
                }
                let n = conn.feed(&bytes[fed..]);
                if n == 0 {
                    return Err("http-net takes no input".into());
                }
                fed += n;
            }
            _ => {}
        }
    }
    parsed.ok_or_else(|| "no request".into())
}

thread_local! {
    static SENT: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

struct Capture;

impl UpstreamIo for Capture {
    fn send(&mut self, bytes: &[u8], _deadline: Instant) -> Result<(), ErrorClass> {
        SENT.with(|s| s.borrow_mut().extend_from_slice(bytes));
        Ok(())
    }
    fn recv(&mut self, buf: &mut [u8], _d: Instant, _k: TimeoutKind) -> Result<usize, ErrorClass> {
        const ANSWER: &[u8] = b"HTTP/1.1 204 No Content\r\n\r\n";
        let n = ANSWER.len().min(buf.len());
        buf[..n].copy_from_slice(&ANSWER[..n]);
        Ok(n)
    }
    fn is_reusable(&mut self) -> bool {
        false
    }
    fn close(&mut self) {}
}

struct Capturing;

impl Connector for Capturing {
    fn connect(
        &self,
        _e: &EndpointAddr,
        _p: &[u8],
        _d: Instant,
    ) -> Result<Box<dyn UpstreamIo>, ErrorClass> {
        Ok(Box::new(Capture))
    }
}

fn proxy() -> &'static Proxy {
    static P: OnceLock<Proxy> = OnceLock::new();
    P.get_or_init(|| {
        ProxyBuilder::new()
            .via("edge")
            .pool(
                "p",
                Pool::new(["10.0.0.1:80"])
                    .retry(Retry::off())
                    .limits(PoolLimits::default().max_idle(0))
                    .timeouts(Timeouts::default().response_head(Duration::from_secs(1))),
            )
            .connector(Capturing)
            .build()
            .expect("the fixed configuration builds")
    })
}

/// Reads choices from the input bytes; past the end every choice is 0.
struct Choices<'a>(&'a [u8]);

impl Choices<'_> {
    fn byte(&mut self) -> u8 {
        match self.0.split_first() {
            Some((b, rest)) => {
                self.0 = rest;
                *b
            }
            None => 0,
        }
    }
    fn below(&mut self, n: usize) -> usize {
        usize::from(self.byte()) % n.max(1)
    }
    fn bytes(&mut self, max: usize) -> Vec<u8> {
        let n = self.below(max + 1).min(self.0.len());
        let (b, rest) = self.0.split_at(n);
        self.0 = rest;
        b.to_vec()
    }
}

const METHODS: [&str; 8] = [
    "GET", "POST", "PUT", "DELETE", "PATCH", "OPTIONS", "HEAD", "PURGE",
];
const SEGMENTS: [&str; 10] = [
    "a", "b%20c", "x", "..", "%2e", "~u", "a;b", "", "q?y=1", "%2F",
];
const FIELDS: [(&str, &str); 24] = [
    ("Content-Length", "5"),
    ("Content-Length", "0"),
    ("Content-Length", " 7 "),
    ("Transfer-Encoding", "chunked"),
    ("Transfer-Encoding", "gzip, chunked"),
    ("Connection", "close, x-a"),
    ("Connection", "keep-alive"),
    ("X-A", "1"),
    ("TE", "trailers"),
    ("Upgrade", "h2c"),
    ("Expect", "100-continue"),
    ("Via", "1.1 other"),
    ("Forwarded", "for=1.2.3.4;host=evil"),
    ("X-Forwarded-For", "9.9.9.9"),
    ("X-Forwarded-Host", "evil"),
    ("Host", "other.example"),
    ("Proxy-Frame", "length=1"),
    ("Keep-Alive", "timeout=5"),
    ("Proxy-Connection", "keep-alive"),
    ("Accept", "*/*"),
    ("Cookie", "a=b; c=d"),
    ("Trailer", "X-A"),
    ("X-Empty", ""),
    ("x-MiXeD", "v\tw"),
];
/// Fields that never reach the upstream: hop-by-hop ones and the proxy's own
/// frame field. `X-A` is added when a `Connection` field names it, and
/// `Expect` when its value is `100-continue` (answered by the inbound side);
/// other expectations are end to end and go through.
const HOP: [&str; 7] = [
    "connection",
    "keep-alive",
    "proxy-connection",
    "te",
    "trailer",
    "upgrade",
    "proxy-frame",
];

/// One request from `data`: forwarded, captured, parsed twice, compared.
/// `Ok(true)` when bytes went upstream and were compared, `Ok(false)` when
/// the forwarder refused the request first; `Err` describes a disagreement
/// or a broken invariant.
pub fn check(data: &[u8]) -> Result<bool, String> {
    let mut c = Choices(data);
    let method = METHODS[c.below(METHODS.len())];
    let mut target = String::new();
    for _ in 0..1 + c.below(4) {
        target.push('/');
        target.push_str(SEGMENTS[c.below(SEGMENTS.len())]);
    }
    let mut fields = FieldList::new();
    let mut names_x_a = false;
    for _ in 0..c.below(10) {
        if c.below(4) == 0 {
            // A field of the input's own making: a token name and a
            // visible value.
            let name: String = c
                .bytes(12)
                .into_iter()
                .map(|b| b"abcdefghijklmnopqrstuvwxyz0123456789-_"[usize::from(b) % 38] as char)
                .collect();
            let value: Vec<u8> = c.bytes(16).into_iter().map(|b| 0x20 + b % 0x5f).collect();
            if !name.is_empty() {
                fields.push(&name, &value);
            }
        } else {
            let (n, v) = FIELDS[c.below(FIELDS.len())];
            names_x_a |= n == "Connection" && v.contains("x-a");
            fields.push(n, v.as_bytes());
        }
    }
    let body = c.bytes(64);
    let framing = match c.below(3) {
        0 => BodyFacts::None,
        1 => BodyFacts::Length(body.len() as u64),
        _ => BodyFacts::Chunked,
    };
    let body = if matches!(framing, BodyFacts::None) {
        Vec::new()
    } else {
        body
    };
    let parts = RequestParts {
        method: method.into(),
        target: target.clone().into(),
        host: b"site.example"[..].into(),
        fields,
        body: framing,
        client: Some("198.51.100.7:40000".parse().expect("addr")),
        ..RequestParts::default()
    };
    SENT.with(|s| s.borrow_mut().clear());
    let Some(mut forward) = proxy().forward(0, parts) else {
        return Err("no route 0".into());
    };
    // The body in uneven pieces.
    let mut at = 0;
    while at < body.len() {
        let n = (1 + c.below(16)).min(body.len() - at);
        if forward.push_body(&body[at..at + n], false).is_err() {
            return Ok(false);
        }
        at += n;
    }
    if forward.push_body(b"", true).is_err() {
        // Refused before anything went out (a target http-net will not write).
        return Ok(false);
    }
    let _ = forward.response();
    drop(forward);
    let sent = SENT.with(|s| std::mem::take(&mut *s.borrow_mut()));
    if sent.is_empty() {
        return Ok(false);
    }
    let ours = reference(&sent)
        .map_err(|e| format!("reference refused: {e}\n{}", String::from_utf8_lossy(&sent)))?;
    let theirs = http_net(&sent).map_err(|e| format!("{e}\n{}", String::from_utf8_lossy(&sent)))?;
    if ours != theirs {
        return Err(format!(
            "the parsers disagree:\n{ours:?}\n{theirs:?}\n{}",
            String::from_utf8_lossy(&sent)
        ));
    }
    let count = |name: &str| ours.fields.iter().filter(|(n, _)| n == name).count();
    if count("host") != 1 {
        return Err(format!("{} Host fields", count("host")));
    }
    for h in HOP {
        if count(h) != 0 {
            return Err(format!("{h} was forwarded"));
        }
    }
    if ours
        .fields
        .iter()
        .any(|(n, v)| n == "expect" && v.eq_ignore_ascii_case(b"100-continue"))
    {
        return Err("Expect: 100-continue was forwarded".into());
    }
    if names_x_a && count("x-a") != 0 {
        return Err("x-a was forwarded though Connection named it".into());
    }
    let framing_fields = count("content-length") + count("transfer-encoding");
    match framing {
        BodyFacts::None
            if framing_fields != 0 && ours.body.is_empty() && count("content-length") == 1 => {}
        BodyFacts::None if framing_fields != 0 => {
            return Err("framing on a request without a body".into());
        }
        BodyFacts::Length(_) | BodyFacts::Chunked if framing_fields != 1 => {
            return Err(format!("{framing_fields} framing fields"));
        }
        _ => {}
    }
    if ours.body != body {
        return Err(format!("body {:?} became {:?}", body, ours.body));
    }
    if ours.method != method {
        return Err(format!("method {method} became {}", ours.method));
    }
    // The forwarding fields are the proxy's own: the client's values for
    // them (the "evil" host above) never reach the upstream. A field of the
    // input's own making may say anything.
    for (n, v) in &ours.fields {
        let forwarding = proxy_net::FORWARDING_SET.contains(&n.as_str());
        if forwarding && v.windows(4).any(|w| w.eq_ignore_ascii_case(b"evil")) {
            return Err(format!(
                "{n}: a client's forwarding value reached the upstream"
            ));
        }
    }
    Ok(true)
}
