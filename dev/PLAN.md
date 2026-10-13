# proxy-net - Project Plan

> The design, strategy, and decision record for `proxy-net`. A living document: every design decision, idea, or open question about proxy-net is written here or in [`ROADMAP.md`](./ROADMAP.md) before it is acted on.
>
> **Status:** scaffold, `0.1.0`. **Scheduled later** (see `_strategy/WEB-STACK-DELIVERABLES.md`).

---

## 1. Purpose

Our own reverse proxy: a serve-net service that receives requests, with the connection gate and every limit in place, and forwards them to upstream servers. It completes the "every option" goal: run direct, run behind a proxy, or be the proxy.

## 2. Scope

- Upstream pools with health checks (active and passive), load balancing (round robin, least connections, consistent hashing for session affinity).
- Forwarding: request and response streaming, connection reuse to upstreams, timeouts and retries for idempotent requests only.
- Forwarded headers: `Forwarded` / `X-Forwarded-*` set from the real client (never passed through from untrusted clients), PROXY protocol to upstreams as an option.
- Header rewriting, path rewriting, host routing.
- WebSocket and upgrade pass-through.
- TLS termination (tls-net) and re-encryption to upstreams.
- Later: response caching (RFC 9111) as a separate layer.

## 3. Threats

| Threat | Defence |
|---|---|
| request smuggling between proxy and upstream | http-net strict parsing; requests re-serialized, never passed through as raw bytes |
| spoofed forwarding headers | client-supplied forwarding headers stripped and rebuilt |
| server-side request forgery through routing rules | upstreams only from configuration |
| upstream exhaustion | per-upstream connection caps, circuit breaking |

## 4. Prerequisites

serve-net, http-net client side, tls-net, net-io outbound connections (with shield-sec's outbound allowlist).

## 5. Decision log

| Id | Date | Decision | Reason |
|---|---|---|---|
| D-001 | 2026-10-13 | Scheduled later | the first deployment sits behind Apache; being the proxy comes after |
| D-002 | 2026-10-13 | Requests are re-serialized, never forwarded as raw bytes | removes the main request-smuggling class |
