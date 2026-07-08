# Design — emit-effects-channel

## Context

`emit` already exists in the engine (`crates/runlet-core/src/engine.rs:533`, `inject_emit`):
a native `__emit` pushes `JSON.stringify`'d values into a per-invocation
`Arc<Mutex<Vec<String>>>` (`engine.rs:307`), capped at `max_ops`, drained after the run by
`drain_effects` (`engine.rs:671`) into `ExecResult.effects` → `Outcome.effects`
(`Vec<Box<RawValue>>`, `host.rs:230`). Crucially, the drain runs **after** `ctx.with(...)`
regardless of whether the handler succeeded or errored — so capture-on-failure is already true
at the engine layer. The gap is entirely at the edge and in the shape: the HTTP front discards
`Outcome.effects` (`handler.rs:1672`), and the values are opaque and untagged, so the platform
has nothing structured to route or govern on.

This change makes `emit` a first-class tagged effects channel and surfaces it. It builds on the
three use-case clusters that motivate it: **① audit / decision log** (emit the *why*),
**② itemized findings** (emit each mismatch a reconciliation run finds; keep them on a crash),
and **③ intent outbox** (emit `email`/`charge` actions a trusted host performs, so the sandbox
never holds credentials). The `read` half of the old dormant pair is dropped — under a
pre-supplied model it is redundant with `ctx`, and as a live fetch it is just a capability.

## Goals / Non-Goals

**Goals:**
- `emit(kind, value)` with a required `kind` tag and opaque `value`.
- Surface effects as `[{kind, value}]` on the `/execute` response, success and error paths.
- Preserve and make explicit the capture-on-failure guarantee.
- Make `kind` the platform's routing/governance slot without the core interpreting it.
- First behavioral tests for `emit`.

**Non-Goals:**
- Box-side durable disposition (routing effects into the `events.rs` per-tenant stream).
- Per-kind effect authorization (gating `charge` intents via `authz.rs`).
- Incrementally streamed effects (SSE) — effects still flush at run end.
- Expanding or re-purposing `read` — it is removed outright (see D5), not reworked.
- The batch path (`POST /batch`) surfacing effects — follow-up.

## Decisions

### D1 — `emit(kind, value)` with a required `kind`
Change the signature from one opaque arg to a required non-empty string `kind` plus an opaque
`value`. **Why required, not optional:** every one of ①②③ genuinely has a kind (a decision, a
finding-type, an action); requiring it makes the whole effect stream uniformly typed and
routable with no untyped escape hatch to special-case. **Why now:** `emit` is dormant (no
callers, no tests), so the breaking change is costless today and expensive after adoption.
*Alternatives rejected:* single-arg opaque + a `{type}` convention (the tag is then
script-controlled JSON the platform can't trust for routing/authz); an overload `emit(value)`
or `emit(kind, value)` (a uniform required `kind` is cleaner and the dormancy removes any
back-compat pressure).

### D2 — `kind` is a routing tag; `value` stays opaque
The core surfaces `kind` structurally and **never interprets its meaning** — it may route,
count, and (later) authorize by `kind`, but "what `charge` means" is the consumer's domain.
`value` remains fully opaque (`RawValue`, verbatim `JSON.stringify` output). **Why:** this
keeps the core domain-agnostic (consistent with the existing "value is opaque to the core"
philosophy) while giving the platform the one structured slot it needs. The effect entry
becomes `{ kind: String, value: Box<RawValue> }`.

### D3 — Capture-on-failure is a first-class guarantee
Effects emitted before a handler throws are surfaced on the error response. The engine already
drains regardless of outcome; the only work is at the edge — attach `effects` when building the
**error** response, not just the success one. **Why:** cluster ② (keep every finding up to the
crash) and audit-of-partial-runs depend on it; it is the main thing a bare `return` cannot do.

### D4 — v1 disposition: caller disposes via the response
Effects are returned on the response; the (trusted) caller disposes them (records the audit,
executes the intents, stores the findings). **Why:** it is the smallest honest change, keeps the
box stateless, and serves ①②③ for a trusted-webhook caller immediately. The heavier
dispositions are deliberate follow-ons that **reuse this same captured stream**: durable
per-tenant disposition (the `events.rs` "durable outbox" seam) and per-kind authorization
(`authz.rs`). Designing `kind` in now is precisely what keeps those follow-ons from re-breaking
the API.

### D5 — `read` is removed, not merely unused
This change **deletes** the dormant `read` seam entirely: the `read`/`__read` globals and
`inject_read` (`engine.rs:568`), the `ReadHook` type (`engine.rs:117`), the
`Invocation.read_hook` field + builder (`host.rs:128,204`), the `ExecParams.read_hook`
threading, and the `CONSUMER_NOTES.md` references. **Why:** under a pre-supplied model `read`
is redundant with `ctx`; as a live fetch it duplicates a capability. It has no product niche,
and leaving dead code invites it to resurface as a false option in future design work. Deleting
it makes the effects channel the single, unambiguous story. *Alternative rejected:* leaving it
dormant — a phantom capability that keeps re-appearing in design discussions.

## Risks / Trade-offs

- **Breaking the `emit` signature** → Mitigation: `emit` is dormant (zero callers/tests); the
  break is invisible and this is the cheapest moment to make it. `.d.ts` + `determinism.js`
  note updated in lockstep.
- **Response bloat from many/large effects** → Mitigation: keep the existing per-execution
  `max_ops` emit cap; add a `kind` length bound; per-kind caps and byte ceilings can follow.
- **Core drifts off "fully opaque"** → Mitigation: only `kind` is structured, and the core
  never interprets it (routing/metering only); `value` stays verbatim-opaque. The domain-
  agnostic line holds.
- **Effects on the error path could surface data a consumer didn't expect** → Mitigation: the
  data is the same the script chose to emit either way; document that error responses may carry
  a partial effects trail so consumers treat it as such.

## Migration Plan

- Purely additive on the wire response; the only breaking surface is the *script-facing* `emit`
  arity, and no in-tree script or test uses it. No `runlet-wire` / `fabricd` contract change,
  so no protocol version bump.
- Rollback: edge-local; reverting `engine.rs`/`handler.rs` restores the prior single-arg,
  dropped-effects behavior with no state to unwind.

## Resolved Decisions (previously open)

- **D6 (was OQ1) — Response placement: top-level `effects`.** The response becomes
  `{data, error, meta, effects}`; `effects` is a first-class field, a peer of `data` — the two
  outputs of a run are "the answer" and "the proposed effects." Not nested under `meta`.
- **D7 (was OQ2) — `kind` constraints.** `kind` is a non-empty string of at most
  `max_emit_kind_len` characters (default **64**), rejected otherwise. A per-tenant
  allowed-kinds allowlist is deferred to the authorization follow-up.
- **D8 (was OQ3) — No per-kind metering in this change.** Effect counts in `meta` are deferred
  to the durable-disposition follow-up; this change only captures and surfaces.
- **D9 (was OQ4) — Config knob `max_emit_kind_len`** on `EngineConfig` — a plain count
  mirroring `max_ops` (not a byte size).

## Build-vs-Adopt Gate

Every concern in this change is plumbing over already-adopted foundations (rquickjs, serde_json,
tokio/axum), so the gate adds **no new dependency**. Recorded for the record — the genuinely
adopt-worthy decisions (durable disposition, per-kind authorization) live in the deferred
follow-ups and get their own gate when proposed.

### Decision: emit(kind, value) QuickJS boundary — Extend rquickjs FFI bridge

- **Status**: approved
- **Why**: Reuse the vetted string-in / JSON-string bridge (`Function::new` + `JSON.stringify`
  wrapper) already hardened across every capability and today's `emit`; a new binding mechanism
  would re-solve a solved, security-sensitive boundary.
- **Considered**: Build a bespoke value-marshalling path (rejected — duplicates the capability
  FFI contract, more surface to audit).
- **Isolation**: `inject_emit` + its JS wrapper in `engine.rs` — the same seam as the current
  `emit`.

### Decision: Effect buffer + capture-on-failure — Extend Arc<Mutex<Vec>> + drain

- **Status**: approved
- **Why**: The existing per-invocation buffer already drains after the run on **both** the
  success and error paths, so capture-on-failure needs no new machinery — only the element type
  changes to `{kind, value}`. std primitives, no dep.
- **Considered**: Adopt a channel/broadcast crate (rejected here — only earns its keep for the
  streaming follow-up, which is out of scope); build a custom accumulator (rejected — the Vec
  already suffices).
- **Isolation**: the `effects` buffer in `engine::run` + `drain_effects`.

### Decision: Opaque value fidelity — Adopt serde_json RawValue (existing)

- **Status**: approved
- **Why**: `RawValue` preserves the emitted JSON bytes verbatim (no key reordering, no lossy
  round-trip) and is already the dep and the current effect storage type.
- **Considered**: Re-parse + re-serialize the value (rejected — reorders keys, drops fidelity,
  needless work).
- **Isolation**: `Box<RawValue>` as the `value` field of the effect entry.

### Decision: kind validation — Build a trivial inline guard

- **Status**: approved
- **Why**: "non-empty string ≤ `max_emit_kind_len`" is a three-line check; adopting a
  validation/identifier crate would be heavier than the guard it replaces and add a dependency
  for nothing.
- **Considered**: Adopt a string-validation crate (rejected — disproportionate to a length +
  non-empty check).
- **Isolation**: the validation branch in `inject_emit` / native `__emit`.
