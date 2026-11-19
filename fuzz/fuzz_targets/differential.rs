//! Idea I-6: a request built from the input goes through the forwarder, and
//! the bytes it sends upstream are read by http-net and by an independent
//! strict reader. Oracle: both accept them and agree on everything, with no
//! hop-by-hop or client framing field forwarded (see
//! `tests/support/differential.rs`, shared with the CI test).

#![no_main]

use libfuzzer_sys::fuzz_target;

#[path = "../../tests/support/differential.rs"]
mod differential;

fuzz_target!(|data: &[u8]| {
    if let Err(e) = differential::check(data) {
        panic!("{e}");
    }
});
