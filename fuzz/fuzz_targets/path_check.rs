//! `check_path` on arbitrary bytes under arbitrary policies. Oracle: it never
//! panics; a path accepted under the strict policy, decoded independently,
//! has no `.` or `..` segment, no encoded slash, backslash, or NUL, and no
//! `;`; and allowing a check never makes a passing path fail.

#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use proxy_net::core::check_path;
use proxy_net::{PathCheck, PathPolicy};

#[derive(Arbitrary, Debug)]
struct Input<'a> {
    path: &'a [u8],
    allow: u8,
    prefix: bool,
}

const CHECKS: [PathCheck; 5] = [
    PathCheck::DotSegment,
    PathCheck::EncodedSlash,
    PathCheck::Backslash,
    PathCheck::EncodedNul,
    PathCheck::Semicolon,
];

fn decode(s: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < s.len() {
        // Two hex digits exactly (`from_str_radix` alone would take "+0").
        if s[i] == b'%'
            && i + 3 <= s.len()
            && s[i + 1].is_ascii_hexdigit()
            && s[i + 2].is_ascii_hexdigit()
        {
            if let Some(v) = std::str::from_utf8(&s[i + 1..i + 3])
                .ok()
                .and_then(|h| u8::from_str_radix(h, 16).ok())
            {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(s[i]);
        i += 1;
    }
    out
}

fuzz_target!(|input: Input<'_>| {
    let mut policy = PathPolicy::STRICT;
    for (bit, check) in CHECKS.iter().enumerate() {
        if input.allow & (1 << bit) != 0 {
            policy = policy.allow(*check);
        }
    }
    let _ = check_path(input.path, policy, input.prefix);
    let path = match input.path.iter().position(|&b| b == b'?') {
        Some(q) => &input.path[..q],
        None => input.path,
    };
    if check_path(input.path, PathPolicy::STRICT, true).is_ok() {
        assert!(!path.contains(&b'\\'));
        assert!(!path.contains(&b';'));
        for seg in path.split(|&b| b == b'/') {
            let d = decode(seg);
            assert!(d != b"." && d != b"..", "{:?}", input.path);
            assert!(!d.contains(&b'/') && !d.contains(&b'\\') && !d.contains(&0), "{:?}", input.path);
        }
        assert!(check_path(input.path, policy, input.prefix).is_ok());
    }
});
