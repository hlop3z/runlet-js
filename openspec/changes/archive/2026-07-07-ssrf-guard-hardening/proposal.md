# Proposal: ssrf-guard-hardening

## Why

The `api` (http) SSRF guard is already strong where it counts — it pins DNS at connect via a
custom `reqwest` resolver (`http.rs::SsrfResolver`), closing the rebinding TOCTOU window that
most rolled-my-own filters get wrong, fails closed on an all-private resolve, and re-validates
every redirect hop. Once `composable-capability-core` lands, this guard stops being one
capability's detail and becomes the **framework guarantee** applied to *every* `ScriptControlled`
capability a Rust dev registers. That promotion raises the bar: the residual gaps an audit found
must be closed so the framework can honestly claim "declare `ScriptControlled(SsrfPolicy)` and the
hole cannot be built."

The audited residual gaps (all incremental hardening, none a redesign):

1. **No explicit scheme allowlist.** `validate_url`/`validate_redirect` never check the scheme —
   http/https-only is inherited from `reqwest` supporting nothing else. A cross-protocol redirect
   is blocked only implicitly. `s3` already does the explicit check (`s3.rs::resolve_host`); `http`
   should match it, per hop.
2. **`s3` outbound calls are not IP-pinned.** `s3` validates the endpoint host once
   (`resolve_host` → `block_private_ip`) then hands off to its signer/client, leaving a
   rebinding window on the list/delete paths. Lower risk (endpoint is operator-supplied, not
   script-supplied) but it is defense-in-depth the `http` path already has.
3. **Deprecated IPv4-compatible IPv6 `::x.x.x.x`** (e.g. `::7f00:1` = `::127.0.0.1`, no `ffff`)
   is unclassified — `to_ipv4_mapped` returns `None` and no segment rule catches it.
4. **Multicast (`224.0.0.0/4`, `ff00::/8`) and reserved/future (`240.0.0.0/4`)** ranges are not
   blocked — low SSRF value but part of an `is_global`-equivalent claim.
5. **Alt-encoding defense (decimal/octal/hex/short-form IPs) is inherited from the `url` crate**,
   not asserted in-code, so a parser behavior change could silently reopen it.
6. **The `allow_private` debug flag and wildcard `*` host make the IP classifier solely
   load-bearing.** There is no boot guard forbidding them on a non-debug production bind.

## What Changes

- **`http` gains an explicit scheme allowlist** (`http`/`https` only), enforced on the initial URL
  **and every redirect hop** — a cross-protocol redirect (`file://`/`gopher://`/…) is rejected
  deterministically, not by relying on the client's supported-scheme set.
- **`s3` pins the validated IP at connect** for its outbound operations (list/delete/send), the
  same connect-time backstop `http` has; presign stays pure crypto (host validated at sign time).
- **The IP classifier (`ssrf.rs`) blocks the deprecated IPv4-compatible IPv6 form, multicast, and
  reserved/future ranges**, and gains explicit regression tests for alt-encoded literals
  (decimal/octal/hex/short-form) so the `url`-crate-inherited normalization is pinned by our own
  tests, not assumed.
- **A boot guard rejects `allow_private` (debug) and wildcard `*` hosts on a non-loopback
  production bind** unless network isolation is asserted — mirroring the existing trusted-mode boot
  guard — so the escape hatches cannot silently become the only line of defense in production.
- No JS-facing surface changes: the happy path and error model (`HTTP_SSRF_BLOCKED` in-band) are
  unchanged; strictly more targets are refused.

## Capabilities

### Modified Capabilities
- `api`: adds the explicit per-hop scheme allowlist; strengthens private/internal-IP blocking
  (IPv4-compatible IPv6, multicast, reserved) with pinned alt-encoding regression coverage; adds
  the production boot guard for the `allow_private`/wildcard escape hatches.
- `s3`: connect-time IP pinning for outbound operations, closing the rebinding gap on the
  operator-supplied endpoint (presign unchanged).

## Impact

- **Code**: `crates/runlet-core/src/ssrf.rs` (classifier ranges + shared connect-time pinning
  helper), `src/http.rs` (scheme allowlist per hop), `src/s3.rs` (pinned resolver on outbound
  ops), `runlet/src/config.rs` (boot guard for `allow_private`/wildcard on a production bind).
- **Unchanged**: the JS `api`/`s3` surface, the in-band error model, `runlet-wire`, fabricd.
- **Sequencing**: independent of `batch-execute-endpoint`; strongest landed **after**
  `composable-capability-core` so the hardened guard is what the framework enforces for every
  registered `ScriptControlled` capability, but it can land before if convenient (it only makes
  the existing `http`/`s3` guard stricter).
- **Tests/docs**: `ssrf.rs` unit tests (new ranges + alt-encodings), integration coverage for a
  cross-protocol redirect refusal, README/`docs/02-api.md` note that only http/https targets are
  reachable.
