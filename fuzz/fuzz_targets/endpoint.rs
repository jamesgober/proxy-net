//! `Endpoint::parse` on arbitrary text. Oracle: it never panics, and an
//! accepted endpoint's address writes back (`Display`) to text that parses to
//! the same address.

#![no_main]

use libfuzzer_sys::fuzz_target;
use proxy_net::Endpoint;

fuzz_target!(|data: &[u8]| {
    let Ok(text) = core::str::from_utf8(data) else {
        return;
    };
    if let Ok(e) = Endpoint::parse(text) {
        let written = e.addr().to_string();
        let again = Endpoint::parse(&written).expect("a written endpoint parses");
        assert_eq!(again.addr(), e.addr(), "{text:?} -> {written:?}");
        assert!(e.addr().port() != 0);
    }
});
