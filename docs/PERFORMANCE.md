<h1 align="center">
        <img width="99" alt="Rust logo" src="https://raw.githubusercontent.com/jamesgober/rust-collection/72baabd71f00e14aa9184efcb16fa3deddda3a0a/assets/rust-logo.svg">
    <br><b>proxy-net</b><br>
    <sub><sup>PERFORMANCE</sup></sub>
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

> What proxy-net costs, how it was measured, and how it compares with other
> proxies. Version `0.7.0`. Every number here comes from a benchmark or test
> in this repository; how to run each one is below its table.

## Against other proxies

nginx, HAProxy, and Envoy in front of the same origin as proxy-net, under the
same load, on one hosted Linux runner (4 CPUs, Ubuntu). Each row is 10 seconds
after a 1-second warm-up. The load generator keeps that many keep-alive
connections, each sending a `GET` and reading the whole response before the
next; the origin answers with 13 bytes. The load generator, the proxy, and the
origin share the 4 CPUs, so the numbers compare the proxies with each other on
that machine and say little about any one of them alone.

| target | requests/s | p50 | p99 | p99.9 |
|---|---:|---:|---:|---:|
| direct, 1 conn | 101,389 | 6 us | 33 us | 44 us |
| proxy-net, 1 conn | 19,657 | 48 us | 84 us | 158 us |
| nginx, 1 conn | 19,689 | 45 us | 82 us | 109 us |
| HAProxy, 1 conn | 24,390 | 25 us | 83 us | 90 us |
| Envoy, 1 conn | 15,123 | 65 us | 119 us | 135 us |
| direct, 16 conn | 449,693 | 31 us | 73 us | 251 us |
| proxy-net, 16 conn | 107,750 | 142 us | 310 us | 768 us |
| nginx, 16 conn | 164,405 | 92 us | 213 us | 558 us |
| HAProxy, 16 conn | 144,205 | 105 us | 225 us | 685 us |
| Envoy, 16 conn | 56,298 | 255 us | 534 us | 611 us |
| direct, 64 conn | 400,811 | 158 us | 296 us | 499 us |
| proxy-net, 64 conn | 96,336 | 615 us | 1.73 ms | 4.43 ms |
| nginx, 64 conn | 166,517 | 353 us | 1.06 ms | 2.76 ms |
| HAProxy, 64 conn | 147,465 | 409 us | 976 us | 1.83 ms |
| Envoy, 64 conn | 62,274 | 908 us | 1.77 ms | 2.50 ms |

No request failed on any target. A second run of the same code, on a runner
about three times slower (31,077 requests per second direct on one
connection), put proxy-net at 94% of nginx on one connection and 80% and 75%
at 16 and 64 connections, and at about twice Envoy throughout. Pingora is
set up by the same workflow and joins the table once its build passes there.

What the table says:

- **One connection:** proxy-net is level with nginx (48 us against 45 us at
  p50); HAProxy is faster, Envoy slower.
- **Under concurrency** proxy-net serves about 65% of nginx and 73% of
  HAProxy, and 1.5 to 1.9 times Envoy. Its p99 at 64 connections is near
  Envoy's and above nginx's.
- **Why:** proxy-net runs one blocking thread per client connection (D-005),
  so with 64 connections on 4 CPUs it is switching threads where nginx and
  HAProxy run one event loop per CPU. The reactor path is the change that
  closes this, and it waits on serve-net and net-io (PLAN G-1, G-11, D-065).

The workflow is `.github/workflows/bench.yml`; it runs on changes to
`benches/` and by hand. Locally, with the proxies installed:

```text
bash benches/compare/run.sh 10 1 16 64
```

## proxy-net against the origin, in one process

`cargo bench --bench compare` starts the origin and proxy-net in one process
and runs the same load directly and through the proxy. On a Windows 11
desktop (32 threads), 3 seconds per row:

| target | requests/s | p50 | p99 | p99.9 |
|---|---:|---:|---:|---:|
| direct, 1 conn | 58,256 | 14 us | 49 us | 77 us |
| proxy-net, 1 conn | 15,865 | 39 us | 142 us | 484 us |
| direct, 16 conn | 539,987 | 30 us | 55 us | 82 us |
| proxy-net, 16 conn | 174,227 | 77 us | 133 us | 400 us |
| direct, 64 conn | 628,046 | 59 us | 503 us | 9.88 ms |
| proxy-net, 64 conn | 308,129 | 115 us | 650 us | 21.58 ms |

At one connection the proxy adds 25 us at p50 here: two more socket round
trips and two thread wake-ups, and the work in between. On Linux under WSL 2
on the same machine, thread wake-ups cost far more and the one-connection
rows are not representative (direct: 74 us at p50); at 64 connections
proxy-net served 431,000 requests per second there.

## What 0.6 changed

Measured before and after on the same machine, 64 connections:

| Change | Before | After |
|---|---:|---:|
| The connection pool's lock without system calls (D-060), Linux | 64,000/s | 409,000/s |
| The same, Windows | 115,000/s | 308,000/s |
| Buffers kept per worker (D-059): allocations per proxied `GET` | 42 (3.1 KB) | 10 (0.6 KB) |
| Route index by exact host: one match among 32 routes | 86 ns | 39 ns |

Before the pool change, every worker forwarding to one endpoint queued
behind each other's socket probe and futex wake, which capped the proxy at
about 60,000 requests per second on Linux at any concurrency.

A per-source connection cap of 16 (net-io's default for blocking workers)
also closed every connection above the 16th from one address. That is a
defence worth keeping on an open listener, but it closes a load balancer's
traffic, which arrives from a few addresses; `Proxy::serve_with` and
`Inbound::max_per_source` now set it (D-061). The benchmarks raise it to the
connection cap, as the other proxies have no such cap by default.

## Allocations

`tests/alloc.rs` counts heap allocations with a global allocator. The origin
and client in the test allocate nothing per request, so everything counted
is the proxy's: serve-net, http-net, and proxy-net together.

| What | Allocations | Bytes |
|---|---:|---:|
| One proxied keep-alive `GET`, steady state | 10 | 598 |
| One route match among 32 routes | 0 | 0 |

Of the 10, 7 build http-net's `Request`, which cannot be reused (PLAN G-17),
and 3 are the method, target, and `Host` copies in `RequestParts`. serve-net
and http-net's response parsing make none. The test fails above 12 per
request, and on any allocation in route matching, on every platform CI runs.

## Memory per active exchange

`tests/memory.rs` holds 64 requests at the origin at once and counts the
proxy's live heap (serve-net, http-net, net-io, and proxy-net) against the
idle proxy's; the origin's and the clients' threads are not counted. Thread
stacks are not heap.

| Client read buffer | Heap per active exchange | Of which |
|---:|---:|---|
| 16 KiB (default) | 27.3 KiB | the client connection's read buffer 16 KiB, http-net's buffers 4 KiB each side, serve-net's connection 1 KiB, the rest under 1 KiB each |
| 4 KiB | 15.3 KiB | the same with a 4 KiB read buffer |

The default stays at 16 KiB (D-067): 4 KiB reads took a 1 MiB upload from
about 1.1 GB/s to 500 MiB/s per connection, and blocking workers hold at
most a few hundred connections, so the saving is a few MB per proxy. The
test fails above 32 KiB by default and above 20 KiB with 4 KiB reads.

## The core's hot paths

`cargo bench --bench core`: the median of 15 timed loops, per operation, on
the Windows desktop.

| Operation | Time |
|---|---:|
| Route match, 32 routes (8 hosts, 4 paths each), the last one matching | 39 ns |
| Endpoint pick, round robin, 4 endpoints | 6 ns |
| Endpoint pick, least connections | 9 ns |
| Endpoint pick, power of two choices | 8 ns |
| Rewriting a request head of 6 fields (forwarding headers, `Via`, target) | 240 ns |

## WebSocket compression (permessage-deflate)

Not offered through the bridge (PLAN D-064). For the record, what
decompressing on one side and compressing again on the other would cost per
message, measured with compress-lib 1.1 (level 1, window 15, no context
takeover) on JSON-like messages:

| Message | Ratio | Compress | Decompress | Both |
|---:|---:|---:|---:|---:|
| 256 B | 1.9 | 2.6 us | 1.9 us | 4.6 us |
| 1 KiB | 3.3 | 4.0 us | 2.6 us | 6.7 us |
| 16 KiB | 4.3 | 34.5 us | 17.2 us | 51.7 us |
| 64 KiB | 4.4 | 133.7 us | 57.0 us | 190.7 us |

A compressor holds about 331 KiB and a decompressor about 12 KiB, so a tunnel
compressing in both directions would hold about 0.7 MB. Passing compressed
messages through untouched is ruled out: the proxy could then check neither
their text nor their size.

## Adaptive concurrency (idea I-5)

A gradient concurrency limit per endpoint, `PoolLimits::adaptive_concurrency`
(experimental, off by default). First a simulation against an endpoint that
queues past 32 concurrent requests (about 32,000 requests per second at
1 ms each), clients waiting 1 ms after a refusal:

| Clients | Default cap (256): p99 | Gradient: throughput | Gradient: p99 |
|---:|---:|---:|---:|
| 16 | 1.20 ms | 16,000/s (all) | 1.20 ms |
| 32 | 1.20 ms | 31,950/s (all) | 1.20 ms |
| 64 | 2.26 ms | 31,580/s | 1.58 ms |
| 128 | 4.28 ms | 31,800/s | 2.01 ms |
| 256 | 8.31 ms | 31,960/s | 4.73 ms |
| 256, the endpoint at half speed | 16.6 ms | 15,980/s (all) | 5.47 ms |

A cap set exactly at the knee gives 1.2 ms at every load; the gradient limit
does not need to know where the knee is, but it lets the queue creep. The
simulation is `src/core/adaptive.rs`.

Then over sockets (`cargo bench --bench compare -- knee`): an origin that
serves 32 requests at once for about 1 ms each and queues the rest inside
itself, proxy-net with the default cap and with the adaptive limit, on Linux
(WSL 2, a busy 32-thread machine), 4 seconds per row:

| Clients | Default cap: req/s, p99 | Adaptive, clients wait 1 ms after a 503 | Adaptive, clients retry at once |
|---:|---:|---:|---:|
| 16 | 11,861, 1.49 ms | 11,198, 1.62 ms | 11,966, 1.49 ms |
| 32 | 22,229, 1.90 ms | 22,303, 1.81 ms | 22,365, 1.78 ms |
| 64 | 27,008, 5.33 ms | 27,229, 2.96 ms | 25,601, 3.64 ms |
| 128 | 27,166, 17.71 ms | 25,727, 4.43 ms | 17,578, 7.20 ms |
| 192 | 27,050, 29.10 ms | 26,014, 4.14 ms | 10,465, 10.47 ms |

With clients that back off, the sockets agree with the simulation: the
capacity kept and p99 cut by up to 86% past the knee. With clients that
retry a 503 at once, every refusal becomes another request, and on a busy
machine the refusals cost up to 60% of throughput. Hence opt-in, off by
default (D-068): it suits a deployment whose clients honour a 503 (proxy-net
sends `Retry-After: 1` with it).

## The targets for 1.0

| Target | Now |
|---|---|
| Route match under 50 ns for 32 routes, no allocation | 39 ns, none |
| Added latency under 30 us at p50 for a small kept-alive request | 25 us on the Windows desktop; 42 us on the 4-CPU Linux runner (nginx adds 39 us there, HAProxy 19 us) |
| Memory under 20 KiB per active exchange | 15.3 KiB with `Inbound::read_buffer(4096)`; 27.3 KiB with the default 16 KiB reads (below) |
