# Design: ssrf-guard-hardening

## Context

The `http` guard (`http.rs` + `ssrf.rs`) already does the structurally hard things: a custom
`reqwest` `dns_resolver` (`SsrfResolver`) means the address reqwest connects to *is* the one the
SSRF classifier passed — no second, unfiltered resolution — and it fails closed when every
resolved address is private. `block_private_ip` in `validate_url` is a fast literal-IP pre-check;
the resolver is the authoritative backstop. Redirects re-run host + IP validation per hop. This
change closes the residual gaps that matter once the same guard becomes the framework's promise
for *every* `ScriptControlled` capability (post `composable-capability-core`). None of it changes
the pinning architecture — it extends coverage.

## Goals / Non-Goals

**Goals**
- Make "declare `ScriptControlled(SsrfPolicy)` ⇒ the SSRF hole cannot be built by an author" an
  honest, tested guarantee: scheme, IP-range, and connect-pinning coverage complete enough to
  stand behind.
- Keep the in-engine guard as the fast per-request layer; make the residual gaps closed defenses,
  not documentation.

**Non-Goals**
- Network-layer egress control (firewalled netns / egress proxy). It is the correct *second*
  independent layer and belongs in deployment/infra, not this crate change — noted as the
  defense-in-depth recommendation, out of scope here.
- Reworking the pinning architecture (already correct).
- Removing the `allow_private`/wildcard escape hatches (they are needed for local dev and explicit
  operator opt-in) — only gating them off a production bind.

## Decisions

### D1 — Explicit scheme allowlist in the shared validator, enforced per hop
Add an `http`/`https`-only scheme check to `validate_url` and `validate_redirect` (mirroring
`s3.rs::resolve_host`, which already rejects non-http schemes). Today cross-protocol is blocked
only because `reqwest` supports nothing else — an implicit guarantee that a client upgrade or a
custom connector could void. Making it explicit is defense-in-depth and turns a redirect to
`file://`/`gopher://` into a deterministic `HTTP_SSRF_BLOCKED`, per hop.
*Alternative*: keep relying on reqwest — rejected; a framework guarantee cannot rest on a
dependency's incidental behavior.

### D2 — Share one connect-time pinned resolver between `http` and `s3`
`s3` validates the endpoint host once then hands off, leaving a rebinding window on its outbound
list/delete/send paths. Extract the `SsrfResolver`/`resolve_filtered` pinning helper into `ssrf.rs`
and install it on the `s3` client too. Risk is lower than `http` (the endpoint is operator-supplied
via `config.s3`, not script-supplied), so this is defense-in-depth, not a live hole — but the
guard is now framework-wide and the cost is one shared helper. Presign stays pure crypto: the host
is validated at sign time; there is no connection to pin.
*Alternative*: leave `s3` validate-time only and document the residual — rejected; the pinning
helper already exists, sharing it is cheaper than carrying a documented gap.

### D3 — Complete the IP classifier: IPv4-compatible IPv6, multicast, reserved
Add to `is_private_v6`: the deprecated IPv4-compatible form `::a.b.c.d` (`::/96`, seg0..6 == 0,
seg6 != 0/1) — unwrap and re-check the embedded v4 exactly as `to_ipv4_mapped` does for the
`::ffff:` form. Add multicast (`ff00::/8`) and to `is_private_v4` add multicast (`224.0.0.0/4`)
and reserved/future (`240.0.0.0/4`). These are low-SSRF-value but required for an honest
"non-global is blocked" claim and cost a few segment checks.
*Alternative*: wait for `std`'s `IpAddr::is_global` to stabilize — rejected; it is still unstable
and we already hand-roll the ranges for exactly this reason (documented in `ssrf.rs`).

### D4 — Pin alt-encoding normalization with our own regression tests
Decimal/octal/hex/short-form IP literals (`2130706433`, `0x7f000001`, `127.1`) are canonicalized
to dotted-quad by the `url` crate's WHATWG parser *before* `host.parse::<IpAddr>()` classifies
them — so the defense is real but *inherited*. Add explicit `ssrf.rs`/`http.rs` tests asserting
each alt form resolves to a blocked address, so a future `url`-crate behavior change is caught by
our suite rather than silently reopening the class. No production code change — this is a coverage
decision.

### D5 — Boot guard: `allow_private` and wildcard `*` are production-off
When `allow_private` (debug) or a wildcard `*` allowlist is active, the IP classifier / host
allowlist is *solely* load-bearing. Add a boot guard — mirroring the existing trusted-mode
non-loopback guard — that refuses to start with either enabled on a non-loopback bind unless
network isolation is explicitly asserted in config. Converts "prod accidentally shipped with the
guard relaxed" from a silent exposure into a startup failure.
*Alternative*: warn-and-continue — rejected; a relaxed SSRF guard in prod is the exact class this
change exists to make un-shippable.

### Decision: IP address classification — Adopt `ip_network`

- **Status**: approved
- **Why**: `ip_network` is stable-toolchain and BSD-2-Clause; its `is_global`/`is_multicast`/
  `is_documentation`/reserved checks close the audit's multicast + reserved/future gaps (D3) with
  vetted code instead of growing hand-rolled segment math. Less maintained security-critical surface.
- **Considered**: keep hand-rolled `std::net` ranges (more surface we own); wait for std
  `IpAddr::is_global` (still unstable — the reason `ssrf.rs` hand-rolls today).
- **Isolation**: `ssrf.rs` stays the single classification module; a thin embed-unwrap for
  IPv4-mapped / IPv4-compatible / 6to4 / NAT64 still runs on top of `ip_network` (the library treats a
  6to4/NAT64 wrapper of a private v4 as globally routable, so that unwrap is ours to keep).
- **Follow-up**: new dependency ⇒ re-run `task supply-chain` (cargo-vet exemption/audit + cargo-deny
  license check for BSD-2-Clause) after adding it.

### Decision: SSRF-safe outbound HTTP pinning — Extend reqwest (custom DNS resolver)

- **Status**: approved
- **Why**: reqwest's `dns_resolver` hook is the same connect-time-pinning mechanism Stripe Smokescreen
  and Doyensec safeurl use, and the current `SsrfResolver` is already audited-good; the only
  purpose-built adopt candidate (`agent-fetch`) fails the maturity rubric — 5★, solo maintainer, 27
  commits, no security audit, undisclosed crypto stack — a hard reject for a perimeter defense.
- **Considered**: Adopt `agent-fetch` (immature); Build from raw hyper/`TcpStream` (reinvents reqwest
  and risks a second crypto stack).
- **Isolation**: `http.rs` `SsrfResolver` + the shared pinning helper extracted into `ssrf.rs`; `s3`
  reuses that same helper for its outbound ops (D2).

## Risks / Trade-offs

- [Explicit scheme check could reject a legitimately-needed scheme] → only `http`/`https` were
  ever reachable (reqwest supports no others); the check codifies current behavior, so no real
  target is lost.
- [`s3` pinning adds a resolver hop to operator-supplied endpoints] → negligible; same mechanism
  `http` already runs, and `s3` endpoints are low-cardinality/long-lived.
- [Boot guard could break an existing prod deploy that (mis)uses `allow_private`] → intended; the
  config that trips it was already an SSRF exposure. Provide the explicit network-isolation opt-out
  for operators who firewall egress at the infra layer.
- [In-engine guard is still necessary-not-sufficient] → true and unchanged; the network-layer
  second line stays the deployment recommendation (Non-Goals), not a code deliverable here.

## Migration Plan

1. Land the `ssrf.rs` classifier additions + shared pinning helper + alt-encoding tests (pure
   internal, no surface change).
2. Add the per-hop scheme allowlist to `http`; install the pinned resolver on `s3`.
3. Add the config boot guard; document the network-isolation opt-out.
4. Rollback: revert; the guard reverts to its prior (already-strong) behavior. No data or wire
   change.

## Open Questions

- Whether the boot guard's network-isolation opt-out should reuse the trusted-mode
  `network_isolation` assertion flag or take its own — lean: reuse, it is the same operator claim.
