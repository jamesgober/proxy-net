//! Request and response head rewriting as data, with the section of RFC
//! 9110 or RFC 7239 each case comes from.

use proxy_net::core::{FieldList, ForwardFacts, request_fields, response_fields};
use proxy_net::{Config, Forwarded, HeaderRule, Pool, ProxyBuilder, Route};

fn config(b: ProxyBuilder) -> Config {
    b.config().expect("config")
}

fn forward(c: &Config, fields: &[(&str, &[u8])], client: &str) -> FieldList {
    let e = &c.pools()[0].endpoints()[0];
    let facts = ForwardFacts::new(b"GET", b"/", b"example.com", e).client(client.parse().ok());
    let (mut out, mut target) = (FieldList::new(), Vec::new());
    request_fields(
        fields,
        &facts,
        c.routes().get(0).expect("route"),
        c,
        &mut out,
        &mut target,
    )
    .expect("forward");
    out
}

fn base() -> ProxyBuilder {
    ProxyBuilder::new().pool("p", Pool::new(["10.0.0.1:80"]))
}

#[test]
fn test_rfc_9110_7_6_1_connection_options_are_removed() {
    let c = config(base());
    let out = forward(
        &c,
        &[
            ("Connection", b"x-a, X-B"),
            ("Connection", b"x-c"),
            ("X-A", b"1"),
            ("x-b", b"2"),
            ("X-C", b"3"),
            ("X-D", b"4"),
        ],
        "192.0.2.1:1",
    );
    assert_eq!(out.get("x-a"), None);
    assert_eq!(out.get("x-b"), None);
    assert_eq!(out.get("x-c"), None);
    assert_eq!(out.get("x-d"), Some(&b"4"[..]));
}

#[test]
fn test_rfc_9110_7_6_3_via_is_appended() {
    let c = config(base().via("edge"));
    let out = forward(
        &c,
        &[("Via", b"1.0 fred, 1.1 p.example.net")],
        "192.0.2.1:1",
    );
    assert_eq!(
        out.get_all("via").collect::<Vec<_>>(),
        [&b"1.0 fred, 1.1 p.example.net"[..], b"1.1 edge"]
    );
}

#[test]
fn test_rfc_7239_elements() {
    let c = config(base().forwarded(Forwarded::standard()));
    for (client, want) in [
        (
            "192.0.2.43:47011",
            &b"for=192.0.2.43;proto=http;host=example.com"[..],
        ),
        (
            "[2001:db8:cafe::17]:4711",
            b"for=\"[2001:db8:cafe::17]\";proto=http;host=example.com",
        ),
        ("not an address", b"for=unknown;proto=http;host=example.com"),
    ] {
        let out = forward(&c, &[], client);
        assert_eq!(out.get("forwarded"), Some(want), "{client}");
    }
}

#[test]
fn test_order_of_kept_fields_is_preserved() {
    let c = config(base().forwarded(Forwarded::off()).no_via());
    let incoming: Vec<(String, Vec<u8>)> = (0..30)
        .map(|i| (format!("x-{i}"), format!("{i}").into_bytes()))
        .collect();
    let fields: Vec<(&str, &[u8])> = incoming
        .iter()
        .map(|(n, v)| (n.as_str(), v.as_slice()))
        .collect();
    let out = forward(&c, &fields, "192.0.2.1:1");
    let names: Vec<&str> = out.iter().map(|(n, _)| n).skip(1).collect();
    let want: Vec<&str> = incoming.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, want);
}

#[test]
fn test_response_rules_and_framing() {
    let c = config(
        ProxyBuilder::new()
            .pool("p", Pool::new(["10.0.0.1:80"]))
            .route(
                Route::any()
                    .to("p")
                    .response_header(HeaderRule::remove("x-powered-by"))
                    .response_header(HeaderRule::set("server", "edge")),
            )
            .no_via(),
    );
    let mut out = FieldList::new();
    response_fields(
        &[
            ("Server", b"nginx"),
            ("X-Powered-By", b"PHP/8"),
            ("Content-Length", b"10"),
            ("ETag", b"\"x\""),
        ],
        c.routes().get(0).expect("route"),
        &c,
        &mut out,
    )
    .expect("forward");
    assert_eq!(
        out.iter().collect::<Vec<_>>(),
        [("etag", &b"\"x\""[..]), ("server", b"edge")]
    );
}

fn forward_with(c: &Config, fields: &[(&str, &[u8])], facts: ForwardFacts<'_>) -> FieldList {
    let (mut out, mut target) = (FieldList::new(), Vec::new());
    request_fields(
        fields,
        &facts,
        c.routes().get(0).expect("route"),
        c,
        &mut out,
        &mut target,
    )
    .expect("forward");
    out
}

#[test]
fn test_append_mode_keeps_the_trusted_hops_and_adds_the_peer() {
    let c = config(base().forwarded(Forwarded::both().append_trusted()));
    let e = &c.pools()[0].endpoints()[0];
    let client = "192.0.2.7:1".parse().ok();
    let peer = "10.1.1.1:5".parse().ok();
    // One trusted hop: the element the trusted proxy added stays, anything
    // the client wrote before it goes, and the peer is this hop's client.
    let fields: &[(&str, &[u8])] = &[
        ("Forwarded", b"for=203.0.113.9, for=192.0.2.7"),
        ("X-Forwarded-For", b"203.0.113.9, 192.0.2.7"),
    ];
    let facts = ForwardFacts::new(b"GET", b"/", b"example.com", e)
        .client(client)
        .chain(peer, 1);
    let out = forward_with(&c, fields, facts);
    assert_eq!(
        out.get("forwarded"),
        Some(&b"for=192.0.2.7, for=10.1.1.1;proto=http;host=example.com"[..])
    );
    assert_eq!(
        out.get("x-forwarded-for"),
        Some(&b"192.0.2.7, 10.1.1.1"[..])
    );
    // A direct client (no trusted hop) replaces the chain as before.
    let facts = ForwardFacts::new(b"GET", b"/", b"example.com", e)
        .client(client)
        .chain(client, 0);
    let out = forward_with(&c, fields, facts);
    assert_eq!(
        out.get("forwarded"),
        Some(&b"for=192.0.2.7;proto=http;host=example.com"[..])
    );
    assert_eq!(out.get("x-forwarded-for"), Some(&b"192.0.2.7"[..]));
}

#[test]
fn test_append_mode_with_a_chain_that_does_not_parse_replaces_it() {
    let c = config(base().forwarded(Forwarded::standard().append_trusted()));
    let e = &c.pools()[0].endpoints()[0];
    let facts = ForwardFacts::new(b"GET", b"/", b"example.com", e)
        .client("192.0.2.7:1".parse().ok())
        .chain("10.1.1.1:5".parse().ok(), 1);
    let out = forward_with(&c, &[("Forwarded", b"for=\"unclosed")], facts);
    assert_eq!(
        out.get("forwarded"),
        Some(&b"for=192.0.2.7;proto=http;host=example.com"[..])
    );
}

#[test]
fn test_request_ids_keep_a_sane_client_value_and_replace_others() {
    let c = config(base().request_id("x-request-id"));
    let e = &c.pools()[0].endpoints()[0];
    let ours = b"0123456789abcdef0123456789abcdef";
    let long = [b'a'; 65];
    for (given, want) in [
        (None, &ours[..]),
        (Some(&b"client-token_1"[..]), &b"client-token_1"[..]),
        (Some(&b"has space"[..]), &ours[..]),
        (Some(&b""[..]), &ours[..]),
        (Some(&long[..]), &ours[..]),
    ] {
        let fields: Vec<(&str, &[u8])> = given.map(|g| ("X-Request-Id", g)).into_iter().collect();
        let facts = ForwardFacts::new(b"GET", b"/", b"example.com", e).request_id(Some(ours));
        let out = forward_with(&c, &fields, facts);
        assert_eq!(out.get_all("x-request-id").collect::<Vec<_>>(), [want]);
    }
    // Without the setting the field passes as any other.
    let c = config(base());
    let facts = ForwardFacts::new(b"GET", b"/", b"example.com", e).request_id(Some(ours));
    let out = forward_with(&c, &[], facts);
    assert_eq!(out.get("x-request-id"), None);
}
