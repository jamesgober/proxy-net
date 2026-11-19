<h1 align="center">
        <img width="99" alt="Rust logo" src="https://raw.githubusercontent.com/jamesgober/rust-collection/72baabd71f00e14aa9184efcb16fa3deddda3a0a/assets/rust-logo.svg">
    <br><b>proxy-net</b><br>
    <sub><sup>SECURITY</sup></sub>
</h1>
<div align="center">
    <sup>
        <a href="../README.md" title="Project Home"><b>HOME</b></a>
        <span>&nbsp;│&nbsp;</span>
        <a href="./README.md" title="Documentation"><b>DOCS</b></a>
        <span>&nbsp;│&nbsp;</span>
        <a href="./API.md" title="API Reference"><b>API</b></a>
    </sup>
</div>
<br>

> The threat model, every threat with its defence, its test, and its cost,
> where proxy-net is stricter than the RFCs, the settings that matter, the
> 0.7 hardening record, and what proxy-net does not defend against. Version
> `0.7.0`. Every threat T-1 to T-22 has an attack test over real sockets or
> on the code that decides it. The reasons are in the decision log of
> [`../dev/PLAN.md`](../dev/PLAN.md).

## Contents

- [Reporting a vulnerability](#reporting-a-vulnerability)
- [Threat model](#threat-model)
- [Threats, defences, tests, costs](#threats-defences-tests-costs)
- [Where proxy-net is stricter than the RFCs](#where-proxy-net-is-stricter-than-the-rfcs)
- [Settings that matter](#settings-that-matter)
- [Hardening record (0.7)](#hardening-record-07)
- [What proxy-net does not defend against](#what-proxy-net-does-not-defend-against)

---

## Reporting a vulnerability

Email **me@jamesgober.com** with "proxy-net security" in the subject. Please include the version, a description of the issue, and a way to reproduce it. Do not open a public issue for a vulnerability.

You will get an answer within three working days. A confirmed issue gets a fixed release, a GitHub security advisory, and a credit in the release notes if you want one.

proxy-net is pre-1.0: fixes go into the newest release only. A flaw in HTTP parsing belongs to http-net, in the inbound side to serve-net, in TLS to tls-net; proxy-net forwards such reports and moves to the fixed version promptly.

---

## Threat model

**Assets:** the integrity of every request an upstream acts on (its method, target, `Host`, body, and the forwarding headers that name the client); the upstreams themselves, which trust that the proxy sends them only well-formed requests for their own routes; the client's responses; the proxy's threads, memory, and connections.

**Attackers:** any client, who controls every byte it sends, can open many connections, and can stall; an upstream that is compromised or confused, which controls every byte it answers with; a party on the network between the proxy and a remote upstream; a discovery source that has been tampered with.

**Trust boundary:** every byte from a client or an upstream is hostile. Configuration (routes, pools, rules, limits, TLS roots) is trusted to mean what the operator wants, but it is checked when it is built, so a route that could never match, a rule that would write an invalid field, or a limit that cannot hold is refused instead of served. Forwarding headers from clients are never trusted; the chain of trusted proxies in front is the inbound side's configuration (serve-net ingress).

**Out of scope here, in place elsewhere:** HTTP/1.1 parsing on both sides (http-net, through serve-net inbound), connection admission, rates, and deadlines (serve-net and net-io), TLS (tls-net, rustls underneath), authentication and authorisation (the services behind the proxy).

---

## Threats, defences, tests, costs

Costs are per request unless stated; measured values are from `docs/PERFORMANCE.md`, estimates are marked [est.]. Every test below runs in CI on Linux, macOS, and Windows on stable and 1.85.

| Id | Threat | Defence | Test | Cost |
|---|---|---|---|---|
| T-1 | Request smuggling between the proxy and an upstream | requests are parsed strictly (http-net) and written again with framing the proxy chose: client `Content-Length`, `Transfer-Encoding`, and every hop-by-hop field are removed; the upstream bytes read the same to two independent parsers | `tests/attacks_sockets.rs` (`test_t1_smuggling_vectors_never_reach_the_upstream`, `test_t1_a_request_in_a_body_stays_a_body`); `tests/differential.rs` (20,000 generated requests; idea I-6); the `differential` fuzz target | writing the head again: about 240 ns for 6 fields |
| T-2 | Response desync from an upstream | strict client-side parsing; responses written again; bytes after a complete response close the connection and are reported (idea I-2); ambiguous framing answers 502 | `tests/proxying.rs` (`test_bytes_after_a_response_count_as_desync`, `test_ambiguous_upstream_framing_is_502`); the `upstream_response` fuzz target | none on the good path |
| T-3 | Spoofed forwarding headers | the forwarding set (`Forwarded`, `X-Forwarded-*`, `X-Real-IP`, and the rest) is always removed and rebuilt from the real client; append mode keeps only what the inbound side's trusted proxies added | `tests/attacks.rs` (`test_t3_spoofed_forwarding_headers_are_rebuilt`); `tests/attacks_suite.rs` (`test_t22_*`); the `forwarded_append` fuzz target | part of the head rewrite |
| T-4 | Desync through a body on `GET`, `HEAD`, `TRACE`, `CONNECT` | refused by default (400) | `tests/attacks.rs` (`test_t4_body_on_get_is_refused`) | 0 |
| T-5 | Header injection through rules or values | rules are checked when the configuration is built; values are checked again when written | `tests/attacks.rs` (`test_t5_header_injection_through_rules_is_refused_at_build`) | 0 per request |
| T-6 | `Connection` naming the proxy's own fields | the proxy's fields are added after hop-by-hop removal, so a client cannot strip them | `tests/attacks.rs` (`test_t6_connection_cannot_strip_the_proxys_fields`) | 0 |
| T-7 | Path confusion (`..`, `%2f`, `..;/`, NUL) | a strict path policy by default: ambiguous paths answer 400, upgrades included; routes match the path as received, never decoded | `tests/attacks.rs` (`test_t7_ambiguous_paths_are_refused`); `tests/websocket.rs` (`test_upgrades_get_the_path_checks`); the `path_check` fuzz target | one pass over the path |
| T-8 | Host routing abuse | routes match one reading of the host (case, port, and trailing dot ignored); two `Host` fields are refused; unknown hosts get 404 (or 421) | `tests/attacks_suite.rs` (`test_t8_host_routing_follows_one_reading_of_the_host`); the `route_match` fuzz target | the route index: 39 ns for 32 routes |
| T-9 | SSRF through routing | endpoints come only from configuration and discovery; no part of a request becomes an address | `tests/attacks_suite.rs` (`test_t9_no_request_names_the_address_the_proxy_dials`) | 0 |
| T-10 | h2c smuggling | `Upgrade` is hop-by-hop; any upgrade other than a WebSocket on a WebSocket route is refused | `tests/attacks.rs` (`test_t10_h2c_upgrade_is_not_forwarded`) | 0 |
| T-11 | WebSocket smuggling | the bridge needs the upstream's genuine 101 with the accept key for the key the proxy sent before the client gets its 101; every message is parsed and written again | `tests/websocket.rs` (`test_an_upstream_without_a_valid_101_gets_no_tunnel`) | one parse and one write per message |
| T-12 | Connection pool poisoning | a connection is reused only after a probe says it is open with nothing waiting, and never after leftover bytes, an announced close, a timeout, or a PROXY header | `tests/attacks_suite.rs` (`test_t12_a_connection_with_any_irregularity_is_not_reused`) | one probe (a system call) per reuse |
| T-13 | Upstream exhaustion by slow clients | the first `request_buffer` bytes of a body are held before an upstream connection is taken; endpoint caps with a bounded wait | `tests/attacks_sockets.rs` (`test_t13_a_small_body_is_held_before_the_upstream_sees_anything`) | the held bytes, at most `request_buffer` |
| T-14 | Lost or duplicated non-idempotent requests | a `POST` takes only a connection used within `fresh_for` and is never retried once sent | `tests/attacks.rs` (`test_t14_post_is_never_sent_twice`) | an extra connect for some `POST`s |
| T-15 | Retry storms | a retry budget, passive ejection with back-off, caps; the adaptive limit (opt-in) sheds load before an endpoint queues | `tests/attacks.rs` (`test_t15_retries_stay_within_the_budget`, `test_t15_failing_endpoints_are_ejected_and_avoided`); `tests/health.rs` | a few atomics per exchange |
| T-16 | Memory exhaustion | budgets on response heads and fields, request and response bodies, relay buffers; inbound head limits (serve-net); buffers kept per worker capped at 16 KiB each | `tests/attacks_suite.rs` (`test_t16_every_buffer_has_a_bound`); `tests/memory.rs` (27 KiB per active exchange, 15 KiB with 4 KiB reads) | the limits themselves |
| T-17 | Proxy loops | `Via` with this proxy's pseudonym refuses the request (508) | `tests/attacks.rs` (`test_t17_a_loop_is_refused`); `tests/multi_node.rs` (`test_a_loop_between_two_proxies_ends_with_508`) | one scan of `Via` |
| T-18 | Topology leaks | error answers name a class only, never an address; response rules can remove `Server` and the like | `tests/attacks_suite.rs` (`test_t18_answers_do_not_name_the_topology`) | 0 |
| T-19 | Interception toward remote upstreams | TLS with the certificate always verified, against public roots or only a private CA; no request before the handshake verifies | `tests/attacks_suite.rs` (`test_t19_an_upstream_with_an_untrusted_certificate_gets_no_request`); `tests/tls.rs` (`test_an_upstream_without_tls_answers_502`); `tests/websocket_tls.rs` (`test_a_wss_upstream_with_an_untrusted_certificate_gets_no_upgrade`, `test_a_wss_upstream_for_another_name_gets_no_upgrade`) | a TLS handshake per new connection |
| T-20 | A compromised discovery source | discovery is configuration; a CIDR allowlist; without one, loopback, link-local (cloud metadata), unspecified, multicast, and broadcast addresses are refused | `tests/live.rs` (`test_discovery_outside_the_allowlist_is_refused`) | per refresh only |
| T-21 | Trailer injection | trailers are dropped; never merged into the head | `tests/attacks_suite.rs` (`test_t21_trailers_never_reach_the_head_or_the_client`) | 0 |
| T-22 | Cache poisoning in front of the proxy | proxy-net does not cache; unkeyed forwarding fields from clients are replaced; response rules can remove unkeyed headers | `tests/attacks_suite.rs` (`test_t22_unkeyed_client_headers_do_not_reach_the_upstream_and_nothing_is_cached`) | 0 |

---

## Where proxy-net is stricter than the RFCs

- **Paths.** `..` segments, encoded slashes and dots, `;` path parameters, and NUL are refused (RFC 3986 allows most of them); an origin and a proxy that read them differently is how path confusion starts.
- **Bodies on `GET`, `HEAD`, `TRACE`, `CONNECT`.** Refused (RFC 9110 gives them no meaning, and some servers read them as the next request).
- **Upgrades.** Only WebSocket, only on routes that allow it, and only after the upstream's genuine 101.
- **Forwarding headers.** Never taken from a client, whatever the client says (RFC 7239 lets a proxy decide; proxy-net always replaces).
- **Reuse.** Any irregularity on an upstream connection, including bytes left after a complete response, ends its reuse.
- **Discovery.** Special addresses are refused without an allowlist.

---

## Settings that matter

Every security setting has a secure default; these are the ones to know.

| Setting | Default | When to change it |
|---|---|---|
| `Route::path_policy` | strict | Only for an origin that needs `;` parameters or encoded slashes, and only on that route. |
| `Forwarded::append_trusted` | off: the chain is replaced | When trusted proxies in front add `Forwarded` or `X-Forwarded-For`; the trust itself is the inbound side's configuration. |
| `UpstreamTls` roots | the Mozilla roots | `roots_pem` with a private CA to trust only it (the way to pin until tls-net has SPKI pins). |
| `Pool::discovery_allow` | special addresses refused | Set the ranges your endpoints live in; with ranges, only addresses inside them are kept. |
| `Inbound::max_per_source` | 16 connections per address | Raise it behind a load balancer, whose clients all arrive from a few addresses; keep it on an open listener. |
| `Limits` | bounded heads, fields, and bodies | Lower `max_request_body` and `max_response_body` to what your routes need. |
| `PoolLimits::adaptive_concurrency` | off | Experimental: sheds load with 503 before an endpoint queues; helps when clients back off on 503 (see `PERFORMANCE.md`). |
| `ProxyBuilder::via` | a pseudonym, loop detection on | Give each proxy in a chain its own pseudonym. |

---

## Hardening record (0.7)

- **Attack suite.** Every threat above has its test; the new ones in 0.7 are in `tests/attacks_suite.rs` (T-8, T-9, T-12, T-16, T-18, T-19, T-21, T-22), and `tests/multi_node.rs` runs chains, loops, and failover between proxies.
- **Differential checking (idea I-6).** The bytes the proxy writes upstream for 20,000 generated requests, and for every fuzzed input of the `differential` target, read the same to http-net and to an independent strict reader, with no hop-by-hop or client framing field forwarded.
- **Fuzzing.** Nine targets (`endpoint`, `route_pattern`, `route_match`, `path_check`, `request_fields`, `exchange`, `upstream_response`, `forwarded_append`, `differential`), each run for 24 hours: see the results below. Every target also runs for 60 seconds in CI.
- **Soak.** `benches/soak.rs` for 24 hours on Linux and on Windows: mixed traffic, a flapping origin, live endpoint changes every 2 minutes, a reload every 10; checked for exchanges that never end, upstream exchanges left active, unexpected statuses, and heap growth. Results below.
- **Memory.** 27 KiB of heap per active exchange by default, 15 KiB with `Inbound::read_buffer(4096)` (`tests/memory.rs`).

Results of the 24-hour runs:

| Run | Result |
|---|---|
| Fuzz, 9 targets, 24 hours each | in progress |
| Soak, Linux, 24 hours | in progress |
| Soak, Windows, 24 hours | in progress |

---

## What proxy-net does not defend against

- **Attacks inside a valid request.** SQL injection, cross-site scripting, and other application-level attacks reach the upstream as the client wrote them; the request is well-formed, so the proxy forwards it. A web application firewall or the application itself handles these.
- **Volumetric attacks.** Connection floods and bandwidth exhaustion are for the network and the inbound side (serve-net and net-io admission, per-source caps, rates); proxy-net bounds what each exchange can hold.
- **A compromised upstream's answers.** A response that is well-formed is relayed. proxy-net parses and bounds it, refuses ambiguous framing, and reports unusually large responses (idea I-4), but it does not judge content.
- **Certificate pinning by key.** tls-net has no SPKI pins yet; trust only a private CA instead.
