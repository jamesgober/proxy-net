//! Idea I-6 in CI: 20,000 generated requests through the forwarder, each
//! request's upstream bytes read by http-net and by an independent strict
//! reader, which must agree (see `support/differential.rs`). The
//! `differential` fuzz target runs the same check on fuzzed input.

#![cfg(feature = "client")]

#[path = "support/differential.rs"]
mod differential;

#[test]
fn test_the_upstream_bytes_read_the_same_to_two_parsers() {
    let mut seed = 0x1234_5678_9abc_def0u64;
    let mut input = [0u8; 160];
    let mut compared = 0;
    for case in 0..20_000 {
        for b in &mut input {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            *b = seed as u8;
        }
        match differential::check(&input) {
            Ok(true) => compared += 1,
            Ok(false) => {}
            Err(e) => panic!("case {case}: {e}"),
        }
    }
    println!("compared {compared} of 20000");
    assert!(
        compared > 10_000,
        "only {compared} requests reached the upstream"
    );
}

#[test]
fn test_fuzz_findings() {
    // A field of the input's own making named `expect` with an empty value:
    // only `Expect: 100-continue` is the inbound side's to answer, so this
    // one goes upstream (the oracle once counted every `Expect` as removed).
    let expect_empty = [
        0x88, 0x04, 0x00, 0x02, 0x6c, 0x65, 0x50, 0x63, 0x35, 0x04, 0x02, 0x85,
    ];
    assert_eq!(differential::check(&expect_empty), Ok(true));
}

#[test]
fn test_the_reference_reader_refuses_what_it_should() {
    use differential::reference;
    assert!(reference(b"GET / HTTP/1.1\r\nHost: a\r\n\r\n").is_ok());
    for bad in [
        &b"GET / HTTP/1.0\r\nHost: a\r\n\r\n"[..],
        b"GET  / HTTP/1.1\r\nHost: a\r\n\r\n",
        b"GET / HTTP/1.1\r\nHost: a\r\n folded\r\n\r\n",
        b"GET / HTTP/1.1\r\nBad Name: a\r\n\r\n",
        b"POST / HTTP/1.1\r\nContent-Length: 2\r\nTransfer-Encoding: chunked\r\n\r\n",
        b"POST / HTTP/1.1\r\nTransfer-Encoding: gzip\r\n\r\n",
        b"POST / HTTP/1.1\r\nContent-Length: 1\r\nContent-Length: 1\r\n\r\na",
        b"POST / HTTP/1.1\r\nContent-Length: 3\r\n\r\nab",
        b"GET / HTTP/1.1\r\nHost: a\r\n\r\nextra",
    ] {
        assert!(reference(bad).is_err(), "{}", String::from_utf8_lossy(bad));
    }
    let chunked = reference(
        b"POST /u HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n3;x=y\r\nabc\r\n0\r\nT: 1\r\n\r\n",
    )
    .expect("chunked");
    assert_eq!(chunked.body, b"abc");
}
