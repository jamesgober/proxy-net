# proxy-net - Roadmap

> Path from scaffold to a stable 1.0. Hard parts are front-loaded; each phase has hard exit criteria.
> Design and decisions: [`PLAN.md`](./PLAN.md).
>
> **Scheduled later** in the stack build order (`_strategy/WEB-STACK-DELIVERABLES.md`). Work starts when its trigger in PLAN.md is reached.
>
> **Anti-deferral rule:** no listed hard task moves to a later phase unless this file records the move and the reason.

## v0.1.0 - Scaffold (DONE, 2026-10-13)
- [x] Manifest, README, CHANGELOG, REPS, dual license, CI, deny, clippy, rustfmt, DIRECTIVES, ROADMAP, PLAN.

## v0.2.0 - Design lock
- [ ] Upstream, pool, routing, and rewrite APIs.

## v0.3.0 - Forwarding
- [ ] Pools, health checks, balancing, forwarding with re-serialization, forwarded headers.

## v0.4.0 - Upgrades and TLS
- [ ] WebSocket pass-through; TLS termination and re-encryption.

## v1.0.0 - Stable
