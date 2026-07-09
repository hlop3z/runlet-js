## Context

`tests/test_simple.py` (2230 lines) mixes three concerns:

```
(A) box-only behavior ......... runs with no peer, self-contained (keep as-is)
(B) box↔wire contract ......... box forms WireInit, maps InitError/Reply, fails closed
(C) real-driver conformance ... does Postgres/NATS/auth actually work end-to-end
```

Layer C is fabricd's behavior tested *through* the box. It drags in `_locate_fabricd` (builds a
sibling `../fabricd` checkout or reads `FABRICD_BIN`), `_full_resources` (a fabricd credential/
resource table the harness constructs), a `_fabricd_up` flag, and per-section reachability probes
that self-skip when fabricd is absent (~46 lines of ceremony total). Two C sections
(`test_circuit_breaker`, `test_statement_timeout_clamp`) *always* skip — they assert behavior that
emigrated to fabricd and "isn't implemented there yet," so they are dark placeholders in this repo.

The repo's own contract (CLAUDE.md): this repo owns `runlet-wire`; fabricd is the replaceable
conformer; the dependency points fabricd → runlet-wire. So conformance is fabricd's to prove.

Constraints: build/test are Docker-only (aws-lc-sys). Layer B's *encoding* is already unit-tested
in `runlet-wire` (`wire.rs` `mod tests`). The box's fail-closed behavior (no backend ⇒
`503 EGRESS_UNAVAILABLE`) is **not** positively asserted anywhere — it appears only as the reason C
sections skip. The user's directive is explicit: reduce code, minimize new boilerplate.

## Goals / Non-Goals

**Goals:**
- Remove layer-C sections and their fabricd scaffolding from `tests/test_simple.py`.
- Sever the suite's dependency on a `../fabricd` checkout / `FABRICD_BIN`.
- Close the one real gap the C sections were masking: a positive fail-closed assertion.
- Keep the change net-negative in lines of code here.

**Non-Goals:**
- Building a fake fabricd peer in this repo.
- Publishing a separate wire "contract kit" / fixture set artifact.
- Changing `runlet-wire` (the contract is unchanged; encoding tests stay).
- Changing any production egress behavior (fail-closed already exists).
- Authoring fabricd's replacement conformance suite (that lives in the fabricd repo).

## Decisions

### D1 — Delete layer C, don't keep-and-skip. **Build-vs-adopt: Build (delete) over Extend.**
The C sections either duplicate fabricd's responsibility or are permanently dark. Keeping them
"just in case" preserves the cross-repo build dependency and the scaffolding that motivated this
change. Deleting them is the largest available code reduction and the only option that actually
removes the coupling. *Alternative rejected:* gate C behind an env flag — still keeps every line.

### D2 — No fake peer. **Adopt existing coverage over Build.**
A fake fabricd would be new code whose only unique value is the happy-path round-trip. But (a) the
wire *encoding* is already unit-tested in `runlet-wire`, and (b) the round-trip is exercised for
real by fabricd's own integration suite against a real box. A fake peer would re-assert what two
existing test surfaces already cover. *Alternative rejected:* Rust fake-peer bin linking
`runlet-wire` — protocol-accurate and self-contained, but it is *addition* for marginal coverage;
revisit only if fabricd's suite proves an inadequate guard for the box direction.

### D3 — No contract kit. **Rent the contract we already own.**
`runlet-wire`'s serde types *are* the contract; fabricd already depends on the crate and tests
against those types in its repo. A published fixture set or `testkit` feature is a second source of
truth to maintain. *Alternative rejected:* shared JSON fixtures (pins both directions in one
artifact) — elegant, but cross-repo scope and new maintenance for a contract that is already
code-level shared. Not worth it under the reduce-code directive.

### D4 — Fail-closed test home: Rust unit test on `sidecar.rs`. **Extend the nearest guarded code.**
The refusal is decided in the box's session-open path before execution admission — a pure box
decision needing no socket or process. A `#[cfg(test)]` unit test there (session-open with no
sidecar + no box-direct binding ⇒ the `Unavailable`/`503` outcome) is the lowest-boilerplate home
and lives beside the code it guards. *Alternative considered:* a `/execute` assertion in
`test_simple.py` (configure `io`, no sidecar, expect `503 EGRESS_UNAVAILABLE`). Cheaper to read as
an end-to-end proof but adds Python and needs a running server; prefer the unit test, add the
Python assertion only if the unit boundary can't reach the decision cleanly.

### D5 — Cross-repo hand-off is documented, not enforced.
This change records (in design + a note toward the fabricd repo) that real-driver conformance now
lives in fabricd against the `runlet-wire` types. We do not block this deletion on fabricd's CI —
the two repos move independently and the C sections here are already non-asserting when fabricd is
absent, so deleting them removes no *green* signal from this repo's CI.

## Risks / Trade-offs

- **Coverage handoff gap** → If fabricd never grows the real-driver suite, that behavior goes
  untested globally. Mitigation: the hand-off note makes ownership explicit; and the box side (the
  only side this repo is responsible for) keeps its encoding tests + the new fail-closed test.
- **Losing the box→fabricd happy-path round-trip as a box-side signal** → Mitigation: fabricd's
  integration suite drives a real box, covering that direction; D2 leaves the door open to a fake
  peer if that proves insufficient in practice.
- **Doc drift** → CLAUDE.md and docs/design reference `FABRICD_BIN`, the sibling checkout, and the
  self-skipping sections. Mitigation: update them in the same change (tasks include the doc sweep).
- **Deletion misses a shared helper** → Some helpers (identity-provider token mint) are used only by
  C. Mitigation: grep for each removed helper's remaining callers before deleting; run the suite to
  confirm remaining sections are green.

## Open Questions

- Does the session-open decision boundary in `sidecar.rs` expose a cleanly unit-testable function,
  or is the `503` mapping only observable at the handler layer? Resolve during apply — if the
  former, D4 is a pure unit test; if the latter, fall back to the D4 Python assertion.
- Should the fabricd hand-off note live in this repo's `docs/design/tenant-egress`-adjacent doc, or
  only in the change record (archived on `/opsx:archive`)? Lean: a short pointer in the design doc,
  since the contract ownership is a durable fact.
