//! Head rewriting on arbitrary field lists, hosts, targets, and clients.
//! Oracle: it never panics; the output has exactly one `Host`, no hop-by-hop
//! field, no client-sent forwarding value, and no `proxy-frame` from the
//! client; the target starts with `/` when the input target did.

#![no_main]

use std::sync::OnceLock;

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use proxy_net::core::{FieldList, ForwardFacts, request_fields};
use proxy_net::{Config, HeaderRule, Pool, ProxyBuilder, Route};

fn config() -> &'static Config {
    static C: OnceLock<Config> = OnceLock::new();
    C.get_or_init(|| {
        ProxyBuilder::new()
            .pool("p", Pool::new(["10.0.0.1:80"]))
            .route(
                Route::any()
                    .path("/v1")
                    .to("p")
                    .strip_prefix("/v1")
                    .request_header(HeaderRule::set("x-env", "fuzz")),
            )
            .route(Route::any().to("p"))
            .frame_facts(true)
            .config()
            .expect("the fixed configuration builds")
    })
}

#[derive(Arbitrary, Debug)]
struct Input<'a> {
    fields: Vec<(&'a str, &'a [u8])>,
    target: &'a [u8],
    host: &'a [u8],
    client: Option<([u8; 4], u16)>,
    https: bool,
    route: bool,
}

const HOP: [&str; 9] = [
    "connection",
    "keep-alive",
    "proxy-connection",
    "te",
    "transfer-encoding",
    "upgrade",
    "trailer",
    "proxy-authorization",
    "content-length",
];

fuzz_target!(|input: Input<'_>| {
    let c = config();
    let e = &c.pools()[0].endpoints()[0];
    let client = input
        .client
        .map(|(ip, port)| std::net::SocketAddr::from((ip, port)));
    let facts = ForwardFacts::new(b"GET", input.target, input.host, e)
        .client(client)
        .https(input.https);
    let route = c.routes().get(usize::from(input.route)).expect("route");
    let (mut out, mut target) = (FieldList::new(), Vec::new());
    if request_fields(&input.fields, &facts, route, c, &mut out, &mut target).is_err() {
        return;
    }
    assert_eq!(out.get_all("host").count(), 1);
    for (name, value) in out.iter() {
        assert!(!HOP.contains(&name), "{name}");
        if name == "x-forwarded-for" {
            assert_eq!(Some(value.to_vec()), client.map(|c| c.ip().to_string().into_bytes()));
        }
    }
    assert_eq!(out.get_all("proxy-frame").count(), 1);
    if input.target.starts_with(b"/") {
        assert!(target.starts_with(b"/"), "{target:?}");
    }
});
