//! Append mode for the forwarding headers, and the request-id rule, on
//! arbitrary incoming chains. Oracle: it never panics; exactly one
//! `Forwarded` and one `X-Forwarded-For` go upstream; the `Forwarded` value
//! parses; neither holds more than `hops + 1` elements; with a trusted hop
//! the last `X-Forwarded-For` entry is the socket peer, and without one it
//! is the client alone; the request id is a token of 1 to 64 bytes.

#![no_main]

use std::sync::OnceLock;

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use proxy_net::core::{FieldList, ForwardFacts, request_fields};
use proxy_net::{Config, Forwarded, Pool, ProxyBuilder};

fn config() -> &'static Config {
    static C: OnceLock<Config> = OnceLock::new();
    C.get_or_init(|| {
        ProxyBuilder::new()
            .pool("p", Pool::new(["10.0.0.1:80"]))
            .forwarded(Forwarded::both().append_trusted())
            .request_id("x-request-id")
            .config()
            .expect("the fixed configuration builds")
    })
}

#[derive(Arbitrary, Debug)]
struct Input<'a> {
    forwarded: Vec<&'a [u8]>,
    xff: Vec<&'a [u8]>,
    request_id: Option<&'a [u8]>,
    client: [u8; 4],
    peer: [u8; 4],
    hops: u8,
}

fn is_token(v: &[u8]) -> bool {
    (1..=64).contains(&v.len())
        && v.iter().all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(b))
}

fuzz_target!(|input: Input<'_>| {
    let c = config();
    let e = &c.pools()[0].endpoints()[0];
    let mut fields: Vec<(&str, &[u8])> = Vec::new();
    fields.extend(input.forwarded.iter().map(|v| ("Forwarded", *v)));
    fields.extend(input.xff.iter().map(|v| ("X-Forwarded-For", *v)));
    if let Some(id) = input.request_id {
        fields.push(("X-Request-Id", id));
    }
    let client = std::net::SocketAddr::from((input.client, 1));
    let peer = std::net::SocketAddr::from((input.peer, 2));
    let ours = b"0123456789abcdef0123456789abcdef";
    let facts = ForwardFacts::new(b"GET", b"/", b"example.com", e)
        .client(Some(client))
        .chain(Some(peer), input.hops)
        .request_id(Some(ours));
    let route = c.routes().get(0).expect("route");
    let (mut out, mut target) = (FieldList::new(), Vec::new());
    if request_fields(&fields, &facts, route, c, &mut out, &mut target).is_err() {
        return;
    }
    let limit = usize::from(input.hops) + 1;
    assert_eq!(out.get_all("forwarded").count(), 1);
    let fwd = out.get("forwarded").expect("forwarded");
    let lines = [fwd];
    let limits = parse_net::Limits::DEFAULT;
    let parsed = parse_net::forwarded::parse(&lines, &limits).expect("the Forwarded value parses");
    assert!(parsed.iter().count() <= limit);
    assert_eq!(out.get_all("x-forwarded-for").count(), 1);
    let xff = String::from_utf8(out.get("x-forwarded-for").expect("xff").to_vec()).expect("ascii");
    let entries: Vec<&str> = xff.split(", ").collect();
    assert!(entries.len() <= limit, "{xff}");
    if input.hops == 0 {
        assert_eq!(xff, client.ip().to_string());
    } else {
        let last = entries.last().copied();
        let peer_ip = peer.ip().to_string();
        let client_ip = client.ip().to_string();
        // A chain that did not parse is replaced: the client alone.
        assert!(last == Some(peer_ip.as_str()) || xff == client_ip, "{xff}");
    }
    assert_eq!(out.get_all("x-request-id").count(), 1);
    let id = out.get("x-request-id").expect("id");
    assert!(is_token(id), "{id:?}");
    if input.request_id.is_some_and(is_token) {
        assert_eq!(Some(id), input.request_id);
    } else {
        assert_eq!(id, ours);
    }
});
