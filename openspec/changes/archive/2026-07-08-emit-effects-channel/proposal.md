## Why

A handler's `return` carries exactly one thing — *the answer* (`{data}`). But real logic
needs to say more: the *why* of a decision (an audit trail), a stream of items it found (a
reconciliation run's mismatches), or side effects it wants performed (an email, a flag) — and
it needs those captured **even when the handler later fails.** The engine already ships a
mechanism for this — `emit(value)` accumulates values into a per-invocation buffer — but the
HTTP front intentionally throws that buffer away (`handler.rs:1672`), and `emit` is opaque and
untagged, so the platform can't route or govern what a script proposes. The channel is built
and dark. This change turns `emit` into a first-class, **tagged effects channel** —
`emit(kind, value)` surfaced on the response — so a script can *propose* structured effects
that the host *disposes* ("logic proposes, host disposes"), without ever handing the sandbox a
credential.

## What Changes

- **`emit(value)` → `emit(kind, value)`.** `kind` is a required non-empty string tag; `value`
  is opaque JSON. **BREAKING** — but free: `emit` is dormant (zero callers, zero tests), so the
  signature is costless to fix now and expensive to fix later.
- **Surface captured effects on the `/execute` response** as a top-level `effects: [{kind,
  value}]`, in call order — the response becomes `{data, error, meta, effects}`, where the
  buffer was previously discarded.
- **Capture survives failure.** Effects emitted before a handler throws SHALL still appear on
  the (error) response, so an itemized run keeps everything it found up to the crash.
- **`kind` is a platform-owned routing/governance slot.** The core surfaces `kind` structurally
  and never interprets its meaning; this is the seam for later **per-kind effect
  authorization** (gate `charge` intents like a capability) and **durable-stream disposition**
  — neither of which an opaque, script-controlled JSON field could support.
- Keep the existing per-execution emit cap (`max_ops`); bound `kind` at `max_emit_kind_len`
  (default 64 chars).
- **`read` / `read_hook` are deleted.** The dormant read seam is removed outright — the
  `read`/`__read` globals, the `ReadHook` type, `Invocation.read_hook`, its `ExecParams`
  threading, and the `CONSUMER_NOTES.md` references — so it cannot resurface as a phantom
  option. Under a pre-supplied model `read` is redundant with `ctx`; as a live fetch it
  duplicates a capability. It has no product niche.

Out of scope (deliberate follow-ups, each reusing this channel): box-side **durable
disposition** into the per-tenant `events.rs` stream; **per-kind effect authorization** via
`authz.rs`; **incrementally streamed** effects (SSE) for progress/agent output.

## Capabilities

### New Capabilities
- `effects-channel`: the tagged `emit(kind, value)` effects output channel — required `kind`
  tag + opaque `value`, ordered capture, capture-on-failure, the per-execution emit bound, and
  the domain-agnostic rule that the core routes/meters by `kind` but never interprets it (the
  authorization/disposition seam).

### Modified Capabilities
- `execution`: the `/execute` response SHALL carry the `effects` list (previously the buffer
  was intentionally discarded), on both the success and error paths; the `emit` signature
  changes from one arg to `(kind, value)`.

## Impact

- **Code — `crates/runlet-core`** (`engine.rs`, `host.rs`): change `inject_emit` to the
  `(kind, value)` signature and validate `kind`; change the effects entry type to
  `{kind, value}` (a tag + `RawValue`); thread it through `ExecResult` → `Outcome.effects`.
  The buffer already drains regardless of handler outcome, so capture-on-failure is preserved
  at the engine layer.
- **Code — `crates/runlet`** (`handler.rs`): stop dropping `Outcome.effects`; attach a
  top-level `effects` list to the response on **both** the success and error paths.
- **Code — read removal** (`crates/runlet-core`): delete `inject_read`/`__read`/`read`, the
  `ReadHook` type, `Invocation.read_hook` + builder, the `ExecParams.read_hook` threading, the
  two test constructions that set `read_hook: None`, and the `CONSUMER_NOTES.md` references.
- **JS/DX:** update the `emit` wrapper and its `.d.ts` / `container/types.d.ts` signature (D11
  golden test); update `determinism.js`'s note that effects are preserved.
- **Wire/API:** additive `/execute` response field; a breaking `emit` *script* signature. **No
  change to `runlet-wire`** — edge-local, not the cross-repo `fabricd` contract.
- **Docs:** a `docs/design/` rationale doc for the effects channel and the propose/dispose
  model; first behavioral tests for `emit` (today: zero).
