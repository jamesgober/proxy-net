//! Small grammar checks shared by the configuration types: tokens, field
//! values, host names, ports, and path bytes.

use alloc::boxed::Box;
use alloc::string::String;
use core::net::{Ipv4Addr, Ipv6Addr};

/// `tchar` of RFC 9110 5.6.2.
pub(crate) fn is_tchar(b: u8) -> bool {
    b.is_ascii_alphanumeric()
        || matches!(
            b,
            b'!' | b'#'
                | b'$'
                | b'%'
                | b'&'
                | b'\''
                | b'*'
                | b'+'
                | b'-'
                | b'.'
                | b'^'
                | b'_'
                | b'`'
                | b'|'
                | b'~'
        )
}

/// A non-empty token (RFC 9110 5.6.2).
pub(crate) fn is_token(s: &[u8]) -> bool {
    !s.is_empty() && s.iter().all(|&b| is_tchar(b))
}

/// A field value (RFC 9110 5.5): visible ASCII, space, tab, and obs-text, with
/// no leading or trailing whitespace. Empty is allowed.
pub(crate) fn is_field_value(s: &[u8]) -> bool {
    let ok = s
        .iter()
        .all(|&b| b == b'\t' || b == b' ' || (0x21..=0x7e).contains(&b) || b >= 0x80);
    let edge = |b: Option<&u8>| matches!(b, Some(b' ' | b'\t'));
    ok && !edge(s.first()) && !edge(s.last())
}

/// A port written as 1 to 65,535, without a sign or a leading zero.
pub(crate) fn parse_port(s: &[u8]) -> Option<u16> {
    if s.is_empty() || s.len() > 5 || s[0] == b'0' {
        return None;
    }
    let mut n: u32 = 0;
    for &b in s {
        if !b.is_ascii_digit() {
            return None;
        }
        n = n * 10 + u32::from(b - b'0');
    }
    u16::try_from(n).ok().filter(|&p| p != 0)
}

/// A host name (RFC 1123 labels): 1 to 253 bytes once one trailing dot is
/// removed, labels of 1 to 63 letters, digits, and hyphens that neither start
/// nor end with a hyphen, and a last label that is not all digits (so it cannot
/// be mistaken for an IPv4 address). Returned in lowercase without the dot.
pub(crate) fn host_name(s: &[u8]) -> Option<Box<str>> {
    let s = s.strip_suffix(b".").unwrap_or(s);
    if s.is_empty() || s.len() > 253 {
        return None;
    }
    let mut last_numeric = false;
    for label in s.split(|&b| b == b'.') {
        if label.is_empty() || label.len() > 63 {
            return None;
        }
        if label[0] == b'-' || label[label.len() - 1] == b'-' {
            return None;
        }
        if !label
            .iter()
            .all(|&b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return None;
        }
        last_numeric = label.iter().all(u8::is_ascii_digit);
    }
    if last_numeric {
        return None;
    }
    let mut out = String::with_capacity(s.len());
    for &b in s {
        out.push(char::from(b.to_ascii_lowercase()));
    }
    Some(out.into_boxed_str())
}

/// An IPv4 address in dotted decimal (the standard library refuses leading
/// zeros, so `010.0.0.1` is not read as octal by one side and decimal by
/// another).
pub(crate) fn ipv4(s: &[u8]) -> Option<Ipv4Addr> {
    core::str::from_utf8(s).ok()?.parse().ok()
}

/// An IPv6 address without brackets and without a zone.
pub(crate) fn ipv6(s: &[u8]) -> Option<Ipv6Addr> {
    if s.contains(&b'%') {
        return None;
    }
    core::str::from_utf8(s).ok()?.parse().ok()
}

/// `pchar` of RFC 3986 3.3 except `%`, which [`check_path`] handles.
fn is_path_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric()
        || matches!(
            b,
            b'-' | b'.'
                | b'_'
                | b'~'
                | b'!'
                | b'$'
                | b'&'
                | b'\''
                | b'('
                | b')'
                | b'*'
                | b'+'
                | b','
                | b';'
                | b'='
                | b':'
                | b'@'
                | b'/'
        )
}

/// A path for patterns and rewrites: starts with `/`, only path bytes and
/// well-formed `%XX` escapes, no `.` or `..` segment raw or escaped.
pub(crate) fn check_path(s: &[u8]) -> bool {
    if s.first() != Some(&b'/') {
        return false;
    }
    let mut i = 0;
    while i < s.len() {
        let b = s[i];
        if b == b'%' {
            match (s.get(i + 1), s.get(i + 2)) {
                (Some(h), Some(l)) if h.is_ascii_hexdigit() && l.is_ascii_hexdigit() => i += 3,
                _ => return false,
            }
        } else if is_path_byte(b) {
            i += 1;
        } else {
            return false;
        }
    }
    s.split(|&b| b == b'/').all(|seg| !is_dot_segment(seg))
}

/// `.` or `..`, raw or with either dot percent-encoded, in any case.
pub(crate) fn is_dot_segment(seg: &[u8]) -> bool {
    let mut dots = 0;
    let mut i = 0;
    while i < seg.len() {
        if seg[i] == b'.' {
            i += 1;
        } else if seg[i] == b'%'
            && seg.get(i + 1) == Some(&b'2')
            && matches!(seg.get(i + 2), Some(b'e' | b'E'))
        {
            i += 3;
        } else {
            return false;
        }
        dots += 1;
        if dots > 2 {
            return false;
        }
    }
    dots == 1 || dots == 2
}

/// Lowercase copy of an ASCII name.
pub(crate) fn lower(s: &str) -> Box<str> {
    s.to_ascii_lowercase().into_boxed_str()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_token_and_field_value() {
        assert!(is_token(b"x-request-id"));
        assert!(!is_token(b""));
        assert!(!is_token(b"a b"));
        assert!(!is_token(b"a:b"));
        assert!(is_field_value(b""));
        assert!(is_field_value(b"a b\tc"));
        assert!(is_field_value("caf\u{e9}".as_bytes()));
        assert!(!is_field_value(b" a"));
        assert!(!is_field_value(b"a\t"));
        assert!(!is_field_value(b"a\r\nb"));
        assert!(!is_field_value(b"a\0"));
        assert!(!is_field_value(b"a\x7f"));
    }

    #[test]
    fn test_port_bounds() {
        assert_eq!(parse_port(b"1"), Some(1));
        assert_eq!(parse_port(b"65535"), Some(65535));
        for bad in [
            &b""[..],
            b"0",
            b"65536",
            b"080",
            b"+80",
            b"8a",
            b"123456",
            b"-1",
        ] {
            assert_eq!(parse_port(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn test_host_name_rules() {
        assert_eq!(
            host_name(b"API.Example.com.").as_deref(),
            Some("api.example.com")
        );
        assert_eq!(host_name(b"a").as_deref(), Some("a"));
        assert_eq!(host_name(b"x-1.y2").as_deref(), Some("x-1.y2"));
        let label = [b'a'; 63];
        assert!(host_name(&label).is_some());
        assert!(host_name(&[b'a'; 64]).is_none());
        let mut long = alloc::vec::Vec::new();
        while long.len() < 253 {
            long.extend_from_slice(b"a.");
        }
        long.truncate(253);
        if long.last() == Some(&b'.') {
            let _ = long.pop();
            long.push(b'b');
        }
        assert_eq!(long.len(), 253);
        assert!(host_name(&long).is_some());
        long.extend_from_slice(b"c");
        assert!(host_name(&long).is_none());
        for bad in [
            &b""[..],
            b".",
            b"-a",
            b"a-",
            b"a..b",
            b"a_b",
            b"1.2.3",
            b"a.123",
            b"caf\xc3\xa9",
            b"a b",
        ] {
            assert!(host_name(bad).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn test_ip_literals() {
        assert!(ipv4(b"10.0.0.1").is_some());
        assert!(ipv4(b"010.0.0.1").is_none());
        assert!(ipv4(b"1.2.3").is_none());
        assert!(ipv6(b"::1").is_some());
        assert!(ipv6(b"fe80::1%eth0").is_none());
        assert!(ipv6(b"not").is_none());
    }

    #[test]
    fn test_check_path_and_dot_segments() {
        for ok in [
            &b"/"[..],
            b"/api",
            b"/api/",
            b"/a%2Fb",
            b"/a;b=c",
            b"/~u/x.y",
            b"/..a",
        ] {
            assert!(check_path(ok), "{ok:?}");
        }
        for bad in [
            &b""[..],
            b"api",
            b"/a/../b",
            b"/a/./b",
            b"/a/%2e%2E",
            b"/a/.%2e/",
            b"/a?b",
            b"/a#b",
            b"/a b",
            b"/a%2",
            b"/a%zz",
            b"/a\\b",
        ] {
            assert!(!check_path(bad), "{bad:?}");
        }
        assert!(is_dot_segment(b"."));
        assert!(is_dot_segment(b"%2E%2e"));
        assert!(!is_dot_segment(b"..."));
        assert!(!is_dot_segment(b""));
        assert!(!is_dot_segment(b".a"));
    }
}
