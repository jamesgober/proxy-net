<h1 align="center">
        <img width="99" alt="Rust logo" src="https://raw.githubusercontent.com/jamesgober/rust-collection/72baabd71f00e14aa9184efcb16fa3deddda3a0a/assets/rust-logo.svg">
    <br><b>proxy-net</b><br>
    <sub><sup>ARCHITECTURE</sup></sub>
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

> How proxy-net is built: the layers, where state lives, which threads run,
> how one request moves through the proxy, and how a live change replaces
> the running state without dropping a request. Version `0.6.0`.

## Layers

| Layer | Feature | What it does |
|---|---|---|
| `core` | always | Decisions only, with no socket and no clock: the route table, balancers, the health table, the retry budget, request checks, head rewriting, loop detection, and the exchange state machine. It needs `alloc` and 64-bit atomics, and builds for `no_std`. |
| `client` | `client` | The upstream side: connectors, the connection pool, the forwarder that drives the core's state machine against real connections, the health checker, discovery, and the swappable running state. |
| `serve` | `serve` | `ProxyService`, the serve-net `HttpService`, plus `Proxy::serve` and `proxy`. |
| `client::TlsConnector` | `tls` | TLS over net-io connections through tls-net. |
| the bridge | `ws` | WebSocket: serve-net on the client side, ws-net on the upstream side. |

The core takes facts and a `Tick` (nanoseconds since the proxy started) and returns decisions. Everything that touches a socket or reads the clock is in `client` or `serve`, so the core can be tested, fuzzed, and model-checked on its own.

## Where state lives

```text
Proxy / ProxyHandle
  └─ Shared
       ├─ RwLock<Arc<Runtime>>       the current running state
       ├─ builder                     what the current state was built from
       ├─ drained endpoints           kept across reloads
       ├─ observer, connector         kept across reloads
       └─ counters                    kept across reloads

Runtime (immutable once built, except its atomics)
  ├─ Config                          the validated configuration
  ├─ generation                      bumped by every live change
  ├─ listener address
  ├─ per route: response-size mean   (I-4)
  └─ per pool: PoolRuntime
       ├─ Balancer                   lock-free picks; per-endpoint counters, ramps
       ├─ HealthTable                one packed atomic word per endpoint
       ├─ RetryBudget                atomics
       ├─ Arc<EndpointConns> each    idle connections and caps: one mutex and condvar
       └─ connector                  TCP, TLS, or the one you gave
```

A request takes one read lock to clone the `Arc<Runtime>` and works on that runtime to the end, so a live change never pulls state from under it. Health and balancing are atomics. The only lock on the request path besides that read lock is the endpoint's connection lock, held to push or pop a connection and never across I/O.

## Threads

- **serve-net workers** run the exchanges. They are blocking workers: the service waits on the upstream inside serve-net's call until serve-net can park a streamed reply (serve-net gap G-1). `Proxy::serve` starts 256; `Proxy::serve_with` takes another number, and the per-source cap, through `Inbound`.
- **Each worker keeps a few buffers between exchanges**: up to three field lists and three byte buffers, each at most 16 KiB, for the client's fields, the upstream request, the bytes sent, the response's fields, and the relay buffer. A warm worker allocates none of them again (10 allocations remain per request, 7 of them http-net's `Request`).
- **One health checker thread per proxy.** It sleeps until the next job and then runs it: an active check, the end of an ejection, closing idle connections past their limit, moving slow-start ramps, or refreshing a discovery source. It follows the current runtime, so it picks up live changes on its own.
- **One pump thread per bridged WebSocket**, reading the upstream and sending to the client.

## One request

1. **Head.** serve-net parses and admits the request, then calls `ProxyService::head`. The service matches a route (404 or 421 when none serves the host), refuses a loop (508), runs the request checks (400 or 405), and stores the route and the runtime's generation in serve-net's per-request word. Nothing upstream has been touched yet, so a refused request costs no connection.
2. **Body.** `call` holds the first `request_buffer` bytes of the body. A body that ends within that is replayable; a larger one streams. If the configuration changed since the head arrived, the request is matched again under the new one.
3. **Pick and connect.** The balancer picks an eligible endpoint (tiers, panic threshold, slow-start ramps, the policy). A pooled connection is reused only after a probe says it is open with nothing waiting and it is within its age and use limits; a non-idempotent request takes only a recently used one; a pool with the PROXY protocol never reuses one. A new connection gets the PROXY header first when the pool sends one, and TLS when the pool has it.
4. **Request out.** The head is rebuilt from the client's fields: hop-by-hop fields and the whole forwarding set removed, header rules applied, `Host` set, then the proxy's own `Forwarded`, `X-Forwarded-*`, `Via`, and request id written. http-net writes it with the framing the proxy chose; the body follows.
5. **Response in.** http-net parses the upstream's response strictly. The response head is rewritten the same way and handed to serve-net, and the body streams back in parts of up to `relay_buffer`, each asked for only once the client took the last.
6. **Finish.** The outcome goes to the health table (and may eject the endpoint), the retry budget, the counters, the route's size mean, and the observer. A failure before any byte reached the client may retry on another endpoint within the budget; after that the client connection is closed. Bytes left after a complete response close the connection and count as a desync.

## Live changes

`reload`, `set_endpoints`, discovery refreshes, and recording the listener address after the bind all build a new `Runtime` beside the current one and swap the `Arc`:

- An endpoint present before and after (same pool name, same address) keeps its health word, its `EndpointConns` (so warm connections stay), and its slow-start ramp.
- A new endpoint in a running pool starts a slow-start ramp when the pool has one.
- An endpoint that is gone takes nothing new; requests already on it finish on the old runtime, which is freed when the last of them ends.
- The generation goes up, so a request whose head arrived before the swap is routed again when its body is forwarded.
- `reload` asks discovery sources again; the other changes do not, so a refresh cannot undo itself.

## The WebSocket bridge

```text
client ──ws──▶ serve-net ──▶ BridgeService ──▶ ws-net Connection ──ws──▶ upstream
client ◀─ws─── serve-net ◀── WsSender ◀─────── pump thread ◀──────ws─── upstream
```

On an upgrade, the service opens the upstream connection first and sends a new handshake (the client's subprotocols offered, no extensions, the request's fields rewritten as for any request). Only when the upstream answers a genuine 101 with the accept key for the key the proxy sent does the service ask serve-net to answer the client's 101. After that, each message is parsed on one side and written again on the other: client messages go through `BridgeService::message` to the upstream connection, and the pump thread reads the upstream and sends each message to the client. No frame passes through as raw bytes.

Toward a pool with TLS (`wss`, 0.6) the TLS session belongs to neither thread. tls-net's `ClientSession` does no I/O of its own, so it sits under the same lock as the upstream WebSocket state. The pump thread reads ciphertext from its own clone of the socket without the lock, then hands it to the session under the lock; every write to the socket also happens under the lock, so records leave in the order they were sealed. When the session cannot seal until the rest of a message from the upstream arrives, the sending side waits for the pump thread, within the write timeout.

## Why it is shaped this way

The reasons are in the decision log of [`dev/PLAN.md`](../dev/PLAN.md). The main ones: re-serializing every message (D-002), building on serve-net instead of a second inbound stack (D-004), blocking workers until serve-net can park a reply (D-005), bridging WebSocket message by message (D-006), and the live-change rules (D-044 to D-047).
