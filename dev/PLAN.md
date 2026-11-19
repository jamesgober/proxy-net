# proxy-net - Project Plan

> The design, strategy, and decision record for `proxy-net`. A living document: every design decision, idea, or open question about proxy-net is written here or in [`ROADMAP.md`](./ROADMAP.md) before it is acted on.
>
> **Status:** `0.5.0`, feature complete (2026-11-19). The API contract is [`../docs/API.md`](../docs/API.md).

---

## 1. Purpose

A reverse proxy and load balancer for services on serve-net. It takes requests that serve-net has admitted (with the connection gate and every inbound limit in place), picks an upstream endpoint from a pool, and forwards the request and the response. It completes the "every option" goal: run direct, run behind a proxy, or be the proxy. In a larger deployment it is the forwarding service behind the proxy, gateway, and load-balancer roles.

## 2. Scope

- **Routing**: host (exact, leftmost wildcard), path (exact, segment prefix, prefix), methods; first match wins; shadowed routes refused when the configuration is built.
- **Pools**: endpoints by address or host name, weights, priority tiers, per-endpoint connection caps, slow start.
- **Balancing**: smooth weighted round robin, weighted least connections, power of two choices, Maglev consistent hashing with bounded load, random, first healthy.
- **Health**: active checks (HTTP or TCP), passive outlier detection, panic threshold, circuit-breaker caps, draining.
- **Forwarding**: requests and responses re-serialized by http-net (never raw bytes), streamed bodies with backpressure, small request bodies buffered before an upstream connection is taken, connection reuse with a probe before every use, fresh connections for non-idempotent requests.
- **Timeouts and retries**: connect, response head, read and write idle, total; retries only for idempotent methods with a replayable body and no response byte sent, under a retry budget.
- **Forwarding headers**: client-sent `Forwarded`, `X-Forwarded-*`, and similar fields always removed and rebuilt from the client address serve-net resolved; `Via` with loop detection; an append mode for a trusted chain; the PROXY protocol to upstreams.
- **Rewriting**: header rules, path prefix strip and replace, `Host` handling.
- **Upgrades**: WebSocket bridged message by message (ws-net on both sides); other upgrades refused.
- **TLS to upstreams** through tls-net; **live reload** of the whole configuration; **discovery** of endpoint sets.
- **Not here**: caching (cache-net, later), forward proxying and CONNECT (never: an SSRF engine by design), HTTP/2 to upstreams (after 1.0, through h2-net).

## 3. Architecture

- `core`: sans-IO, `alloc` only. The route table (0.2); balancers, health state, retry accounting, head rewriting, path safety, loop detection, and the exchange state machine (0.3). It takes facts and time (`Tick`) and returns decisions; it reads no clock and opens no socket.
- `client` (feature, 0.4): the upstream side over net-io: connector, connection pool sharded per thread, the connection driving http-net's `ClientConn`, the active health checker.
- `serve` (feature, 0.4): `ProxyService`, a serve-net `HttpService`; Tier 1 `proxy`.
- `tls` (feature, 0.5): `TlsConnector`, TLS over net-io connections through tls-net.
- `ws` (feature, 0.5): the WebSocket bridge; serve-net terminates the client's WebSocket and ws-net opens one to the upstream.

The running state (`Runtime`) sits behind one `RwLock<Arc<_>>`: a request takes a clone of the `Arc` once, and a live change builds a new runtime beside the old one and swaps it in. Requests in flight finish on the runtime they started with.

Threads: serve-net's workers run the exchanges; one health-checker thread per proxy, which also refreshes discovery and moves slow-start ramps; one reader thread per bridged WebSocket on the blocking driver.

## 4. Threats

| Id | Threat | Defence |
|---|---|---|
| T-1 | request smuggling between proxy and upstream | strict parsing; requests re-serialized with framing the proxy chose (D-002) |
| T-2 | response desync from the upstream | strict client-side parsing; responses re-serialized; leftover bytes close the connection and are reported |
| T-3 | spoofed forwarding headers | the forwarding set is always removed and rebuilt |
| T-4 | desync through a body on `GET` and similar | refused by default (D-014) |
| T-5 | header injection through rules or values | rules checked at build; values checked again on write |
| T-6 | `Connection` naming the proxy's own fields | the proxy adds its fields after hop-by-hop removal |
| T-7 | path confusion (`..`, `%2f`, `..;/`) | strict path policy by default (D-013) |
| T-8 | host routing abuse | explicit host matching; unknown hosts get 404 or 421 |
| T-9 | SSRF through routing | endpoints only from configuration and discovery; no request data becomes an address |
| T-10 | h2c smuggling | `Upgrade` is hop-by-hop; non-WebSocket upgrades refused |
| T-11 | WebSocket smuggling | the bridge needs a genuine 101 with a correct accept key; frames re-written |
| T-12 | connection pool poisoning | no reuse after any irregularity |
| T-13 | upstream exhaustion by slow clients | small bodies buffered first; per-endpoint caps |
| T-14 | lost or duplicated non-idempotent requests | fresh connections for them; never retried |
| T-15 | retry storms | retry budget, ejection, caps |
| T-16 | memory exhaustion | budgets on upstream heads; bounded relay buffers |
| T-17 | proxy loops | `Via` pseudonym loop detection, 508 |
| T-18 | topology leaks | error bodies name a class only; response rules can remove `Server` |
| T-19 | interception toward remote upstreams | TLS with verification against public roots or a private CA (pinning through a private CA until tls-net has SPKI pins, G-13) |
| T-20 | a compromised discovery source | discovery is configuration; a CIDR allowlist; without one, loopback, link-local (cloud metadata), unspecified, multicast, and broadcast addresses are refused (D-053) |
| T-21 | trailer injection | forbidden trailer fields dropped; trailers never merged into the head |
| T-22 | cache poisoning in front | proxy-net does not cache; rules can remove unkeyed headers |

Every threat gets an attack test named after it, in the version that ships its defence.

## 5. Dependencies

| Crate | Version | Why | From |
|---|---|---|---|
| http-net | 0.10 | upstream request writing and response parsing (`ClientConn`) | 0.4 (added) |
| parse-net | 0.7 (the version http-net 0.10 pins) | `Forwarded` writing and parsing, list parsing (`Connection`, `Via`) | 0.3 (added) |
| net-io | 0.8.1 | outbound connections and their deadlines, PROXY encoders (from 0.5) | 0.4 (added) |
| serve-net | 0.8.2 | the inbound side | 0.4 (added) |
| tls-net | 0.10 | TLS to upstreams (feature `tls`) | 0.5 |
| ws-net | 0.10 | the WebSocket bridge (feature `ws`) | 0.5 |

Each dependency is added with the first code that needs it: parse-net in 0.3, the others in 0.4 and 0.5.

## 6. What proxy-net needs from other crates

Each gap has a fallback here, so proxy-net never waits on it.

| Id | Crate | Gap | Fallback until closed |
|---|---|---|---|
| G-1 | serve-net | a streamed reply cannot wait for upstream data without blocking the worker | blocking workers required |
| G-2 | serve-net | a service cannot send a 1xx (103) | 103 dropped |
| G-3 | serve-net | no request trailers reported, no reply trailers written | trailers dropped |
| G-4 | serve-net | at most 64 reply fields | larger responses answered 502 and reported |
| G-5 | serve-net | a streamed reply is always chunked; no length on `HEAD` replies | known lengths sent chunked |
| G-6 | serve-net | no raw-connection hand-off after 101 | WebSocket bridged; other upgrades refused |
| G-7 | serve-net | built on http-net 0.9, net-io 0.7, and (with `ws`) ws-net 0.9 | two copies in the build; no type crosses (the bridge hands serve-net its own message type) |
| G-8 | http-net | the planned `quirks` facts are not shipped | proxy-net's own agreement checks |
| G-9 | http-net | `Request::forward_from` takes only an http-net head | proxy-net builds the request from a field list |
| G-10 | net-io | `encode_v1` allocates per header | accepted: one per new upstream connection |
| G-11 | net-io | no non-blocking outbound connect kept on a reactor worker | blocking workers required |
| G-12 | serve-net | the running server does not report the address it bound | `Proxy::serve` binds, then records the address with a configuration swap (D-044) |
| G-13 | tls-net | no SPKI pinning on the client | pin by trusting only a private CA (`UpstreamTls::roots_pem`) |
| G-14 | serve-net | a `WsSender` cannot close the client's WebSocket | an upstream close is passed on at the client's next message; the tunnel idle timeout ends the rest |
| G-15 | tls-net | a `TlsStream` cannot be read on one thread and written on another | `wss` upstreams are not bridged (D-048) |

## 7. Ideas (one new defence per version)

| Id | Version | Hypothesis | Method | Expected cost |
|---|---|---|---|---|
| I-1 | 0.3 | If an upstream can see how the proxy framed a request, a framing disagreement becomes detectable there. | Optional `Proxy-Frame` field with the framing facts the proxy wrote; an http-net upstream compares. | about 40 bytes and 30 ns per request [est.]; off by default |
| I-2 | 0.4 | Bytes left after a complete upstream response mean the upstream is desynchronised. | Close, report `desync`, and count it as an outlier failure. | none on the good path |
| I-3 | 0.4 | The keep-alive close race loses only requests sent on connections idle long enough to be closed upstream. | Non-idempotent requests reuse only connections used within `fresh_for`. | an extra connect for some POSTs |
| I-4 | 0.5 | A response far larger than usual for a route signals exfiltration or a confused upstream. | Per-route EWMA of response sizes (weight 1/32); after 100 responses, one over 8 times the mean and over 64 KiB goes to `Observer::response_outlier`. Reported, never blocked. | three relaxed atomic operations per exchange (about 2 ns [est.]); 16 bytes per route |
| I-5 | 0.6 | A gradient concurrency limit finds the overload knee better than a fixed cap. | Experimental adaptive limit per pool, compared under a load sweep. | a few atomics per exchange |
| I-6 | 0.7 | Differential checking catches what unit tests miss. | Fuzzed requests through the proxy to two upstream parsers; compare what each saw. | test time only |

## 8. Open decisions

| Id | Question | Recommendation |
|---|---|---|
| OD-1 | Approval of the design lock (`docs/API.md`) | Needs James: merging the 0.2 pull request is the approval. |
| OD-2 | When to publish to crates.io for the first time | Needs James. Recommendation: publish 0.2.0 so the name is held and docs.rs shows the locked API; it holds configuration code only. |
| OD-3 | permessage-deflate through the WebSocket bridge | Decide at 0.6 with numbers: decompress and recompress, or pass compressed payloads when both sides agree on parameters. Until then the offer is narrowed so neither side negotiates it. |

## 9. Decision log

| Id | Date | Decision | Reason |
|---|---|---|---|
| D-001 | 2026-10-13 | Scheduled later | the first deployment sits behind Apache; being the proxy comes after |
| D-002 | 2026-10-13 | Requests are re-serialized, never forwarded as raw bytes | removes the main request-smuggling class |
| D-003 | 2026-11-19 | Scheduled now; built version by version from 0.2 | James scheduled it for the proxy, gateway, and load-balancer roles |
| D-004 | 2026-11-19 | The inbound side is a serve-net `HttpService`, not proxy-net's own sessions | serve-net owns ingress, admission, limits, and drain; a second inbound stack would duplicate the security pipeline and drift from it |
| D-005 | 2026-11-19 | Blocking workers until serve-net can park a streamed reply (G-1) | streaming with backpressure is required and only the blocking driver gives it today; buffering whole responses on the reactor would break the memory bound |
| D-006 | 2026-11-19 | WebSocket is bridged message by message through ws-net; other upgrades are refused | serve-net has no raw hand-off; bridging fits D-002 and stops WebSocket smuggling by construction |
| D-007 | 2026-11-19 | 1xx (other than 100) and trailers are dropped until serve-net can send them | RFC 9110 lets an intermediary that re-frames a body drop trailers, and 103 is advisory; documented, and changed as soon as serve-net closes G-2 and G-3 |
| D-008 | 2026-11-19 | Consistent hashing is Maglev with a bounded load, keyed by a fixed hash with a configured key | O(1) lookup, minimal disruption, and the same table on every node (deterministic, NFR-8) |
| D-009 | 2026-11-19 | The bounded load is an integer percentage (100 to 1,000, 0 for none) | no floating point in a decision that must agree across nodes and platforms |
| D-010 | 2026-11-19 | A detected loop answers 508 | its meaning is this case and it is distinct in logs |
| D-011 | 2026-11-19 | Both forwarding styles are written by default | most origins read `X-Forwarded-For`; both come from the same resolved client, so they cannot disagree |
| D-012 | 2026-11-19 | Newest http-net (0.10), net-io (0.8), and the parse-net http-net pins (0.7), even though serve-net is older | proxy-net needs http-net 0.10's 1xx and trailer reporting and net-io 0.8's fixes; no type crosses between copies |
| D-013 | 2026-11-19 | Paths a proxy and an origin could read differently are refused by default (`PathPolicy::STRICT`), with per-route, per-check opt-outs | T-7; the refusal is visible to the operator, an ambiguity is not |
| D-014 | 2026-11-19 | A body on `GET`, `HEAD`, `OPTIONS`, `TRACE`, or `DELETE` is refused by default | T-4; the few APIs that need it opt in per route |
| D-015 | 2026-11-19 | Health is judged per node | the right behaviour during a partition; a shared feed is a later option |
| D-016 | 2026-11-19 | Caching lives in cache-net, in front of proxy-net | one job per crate |
| D-017 | 2026-11-19 | Routes match the path as received (never decoded); `path("/api")` is a segment prefix, `path("/api/")` a prefix, `exact` an exact match; a route shadowed entirely by an earlier one is refused | decoding to match invites the confusion T-7 refuses; a shadowed route is always a configuration mistake |
| D-018 | 2026-11-19 | Endpoint text: RFC 1123 host names (last label not all digits), IPv4 without leading zeros, bracketed IPv6 without zones, ports 1 to 65,535 without a sign or a leading zero | every accepted form has one reading |
| D-019 | 2026-11-19 | One pool and no routes routes everything to that pool; several pools need routes | the common case is one line, the ambiguous case is an error |
| D-020 | 2026-11-19 | `ProxyBuilder::config` returns the validated `Config` in 0.2; `build` returns a running-capable `Proxy` from 0.4 | both names are fixed now, so 0.4 adds without breaking |
| D-021 | 2026-11-19 | Header rules cannot touch framing, hop-by-hop, `Host`, `Via`, `Expect`, or the forwarding set | the proxy writes those itself; a rule there is either a no-op or a hole |
| D-022 | 2026-11-19 | Configuration getters on builder-style types end in `_value` (`weight_value()`) | the setter takes the plain name; one type serves as builder and value |
| D-023 | 2026-11-19 | Fuzz targets live in `fuzz/` (cargo-fuzz), with the corpus outside the repository and a 60 s smoke run per target in CI | standard 2.11; the fuzz crate's dependencies never reach the library |
| D-024 | 2026-11-19 | The core takes parse-net 0.7 in 0.3 and does its own hop-by-hop removal over parse-net's list parser; http-net joins in 0.4 with the upstream client | the core needs the list grammar and `Forwarded` writing now, and serve-net hands its services its own head view, so http-net's `RequestHead` helpers cannot be fed directly anyway (G-9) |
| D-025 | 2026-11-19 | The core needs 64-bit atomics; the `no_std` CI target is `aarch64-unknown-none` | lock-free balancers and health words need them, and every target a proxy runs on has them; a 32-bit microcontroller target proves nothing useful here |
| D-026 | 2026-11-19 | Weighted round robin is a schedule built once by stride scheduling (the k-th turn of an endpoint at (2k + 1) / 2w), walked with one atomic counter | exact counts per cycle and even spacing, with no lock on the pick path (the nginx loop needs a lock or a CAS on every endpoint per pick); build is O(S log S) with S at most 65,536 |
| D-027 | 2026-11-19 | An ineligible endpoint's Maglev slots fall to the next slots of the table instead of rebuilding the table | only the failed endpoint's keys move, the table is immutable (no swap on the pick path), and the bounded load already walks the same probe order |
| D-028 | 2026-11-19 | Timeouts after the request was sent are never retried; a connect timeout is | the upstream may still be working on the request; retrying it doubles the load at the worst moment |
| D-029 | 2026-11-19 | Compare-and-swap loops are written out (`core::atomic`) instead of `fetch_update` | `fetch_update` is deprecated on new toolchains in favour of a method the MSRV does not have |
| D-030 | 2026-11-19 | The consistent-hashing key is fixed (all zero) in 0.3; `ProxyBuilder::hash_key` comes in 0.4 | nodes must agree, which the fixed key gives; nothing in 0.3 takes traffic, so an unpredictable key only matters once requests arrive |
| D-031 | 2026-11-19 | Property tests use proptest (dev-dependency only) | standard 2.11; the resolver picks a release that builds on 1.85 |
| D-032 | 2026-11-19 | Idea I-1 ships as `frame_facts` (off by default), and a client's `proxy-frame` is always removed | a framing statement a client could forge would be worse than none |
| D-033 | 2026-11-19 | With the default ejection cap (50%), a pool of one endpoint is never ejected | ejecting the only endpoint refuses everything; the panic threshold would send traffic to it anyway |
| D-034 | 2026-11-19 | A failure while connecting is retried for any method whose body is held; after the request may have left, only idempotent methods retry | the request never reached the upstream, so a retry cannot duplicate it; without this, every POST to a dead endpoint fails until ejection |
| D-035 | 2026-11-19 | The serve-net service keeps each request's exchange in a thread-local slot keyed by connection and request number | blocking workers serve one connection per thread, so the slot needs no lock; it is cleared in `done` on every path |
| D-036 | 2026-11-19 | One mutex and condition variable per endpoint for its idle connections and caps, not sharded yet | the lock is held only to push or pop (never across I/O); sharding waits for 0.6 numbers |
| D-037 | 2026-11-19 | `Connector::connect` returns `Box<dyn UpstreamIo>` | one allocation per new connection, not per request, and the proxy type stays free of a connector parameter |
| D-038 | 2026-11-19 | The upstream's `Date` is dropped; serve-net writes its own | two `Date` fields would be invalid; the proxy is the sender the client sees |
| D-039 | 2026-11-19 | A response with more than 64 fields answers 502 | serve-net 0.8 writes at most 64 reply fields (G-4); an incomplete response would be worse |
| D-040 | 2026-11-19 | Model checks run under `--cfg proxy_net_loom`, with loom only as a cfg-gated dependency | the atomics come through one module (`core::atomic`), so loom swaps them without touching the code; a normal build never sees loom |
| D-041 | 2026-11-19 | `Proxy::serve` and `proxy` start serve-net with 256 blocking workers and the body limit raised to `max_request_body` | the service blocks on the upstream (G-1), and serve-net's default 1 MiB body limit would cut uploads short |
| D-042 | 2026-11-19 | Slow start moves from 0.4 to 0.5 | round robin walks a schedule built once; changing weights over time means rebuilding it, which the 0.5 reload work does anyway. It is off by default, so no behaviour changes in the meantime |
| D-043 | 2026-11-19 | A pool that sends the PROXY protocol never reuses a connection | the header names one client; a reused connection would attribute the next client's request to the first |
| D-044 | 2026-11-19 | The listener address is recorded after `serve` binds, through a configuration swap | serve-net does not report the bound address (G-12), and port 0 is only known after the bind; the swap keeps health and connections |
| D-045 | 2026-11-19 | `reload` asks discovery again; `swap` (used by `set_endpoints`, discovery refreshes, and the listener address) does not | a refresh that re-queried discovery would undo the result it was applying |
| D-046 | 2026-11-19 | An endpoint present before and after a live change (same pool name, same address) keeps its health, its pooled connections, and its slow-start ramp; one that is gone takes nothing new and finishes what it has | a reload must not re-admit an ejected endpoint or drop warm connections |
| D-047 | 2026-11-19 | A request whose head arrived under an older configuration is routed again under the current one (the configuration generation sits in serve-net's per-request word) | the head and the body of one request must not be judged by two configurations |
| D-048 | 2026-11-19 | The WebSocket bridge refuses `wss` upstreams in 0.5 (502); see ROADMAP | the bridge reads the upstream on one thread and writes it on another, and a TLS stream cannot be split (G-15); a single-threaded pump needs the reactor path (G-1) |
| D-049 | 2026-11-19 | Subprotocol names the bridge answers with are interned, at most 64 distinct names per process; beyond that the upgrade fails with 502 | serve-net takes the chosen name as `&'static str`; the cap bounds the memory an upstream can make the proxy keep |
| D-050 | 2026-11-19 | A TLS upstream connection is reused only when the probe reports it open with nothing waiting | buffered records (a close notify, a key update) would otherwise be read as the next response |
| D-051 | 2026-11-19 | Slow start is a per-endpoint share, 10% rising linearly to 100% over the ramp, applied as an admit test on the endpoint's turns; it replaces D-042's plan to rebuild the schedule | no schedule rebuild and no lock on the pick path, one atomic load per pick of a ramping endpoint, and it works for every policy; ramps start when an endpoint returns from ejection or an unhealthy state, and when a live change adds it to a running pool |
| D-052 | 2026-11-19 | `request_id` keeps a client's value when it is a token of 1 to 64 bytes and otherwise writes 32 hex characters from a per-process keyed counter | upstream logs can join on a caller's id, and a value that could carry an injection or flood logs is replaced; the id is unique per process, not secret |
| D-053 | 2026-11-19 | Discovery without an allowlist refuses loopback, link-local, unspecified, multicast, and broadcast addresses; with one, only addresses inside it (and no names) are kept; a refresh that leaves nothing keeps the last set and is reported | a discovery source that is spoofed or misconfigured must not point the proxy at a metadata service or at itself; a pool never empties itself on a bad refresh |
| D-054 | 2026-11-19 | Append mode keeps the last `hops` elements of the incoming `Forwarded` and `X-Forwarded-For` (the hops the inbound side trusted) and adds the socket peer; a chain that does not parse is replaced as in the default mode | the trusted proxies' entries are kept and nothing a client wrote before them survives |
| D-055 | 2026-11-19 | An address range with bits set past its prefix (`10.1.0.0/8`) is refused | it is almost always a typo for a narrower range, and accepting it would allow the wider one |
| D-056 | 2026-11-19 | A WebSocket upgrade runs the same request checks as any request (path policy, methods) before the bridge opens anything, and a request routed again after a live change runs the route, loop, and request checks again under the new configuration; a request already being forwarded is never routed again | an upgrade route must not be a way around T-7, and a stricter configuration must apply to every request it routes |
| D-057 | 2026-11-19 | cargo-deny allows CDLA-Permissive-2.0 for webpki-roots only, and bans other TLS stacks (`openssl`, `openssl-sys`, `native-tls`), async runtimes (`tokio`, `async-std`), and the archived `rustls-pemfile` | webpki-roots is the Mozilla root bundle (data) that tls-net uses, under a permissive data license; a per-crate exception keeps that license out of everything else. The bans hold tls-net's line here: one TLS stack, no async runtime, no unmaintained PEM parser |
