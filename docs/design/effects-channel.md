# The effects channel — `emit(kind, value)`

A handler's `return` carries exactly one thing: *the answer* (`{data}` / `{error}`). Real logic
often needs to say more — *why* it decided (an audit trail), a *stream of items* it found (a
reconciliation run's mismatches), or *side effects it wants performed* (send an email, flip a
flag) — and it needs those captured **even when the handler later fails.** The effects channel is
that second output.

```js
function handler(ctx) {
  emit("decided", { tier: "tier-3", reason: "spend > 10k" }); // an audit/decision trail
  for (const m of mismatches) emit("finding", m);             // itemized findings
  emit("email", { to: ctx.user, template: "welcome" });       // an intent for the host to perform
  return json({ ok: true });
}
```

The response becomes `{ data, error, meta, effects }`, where `effects` is an ordered list of
`{ kind, value }` entries in call order.

## Logic proposes, the host disposes

The sandbox holds no credentials and performs no privileged action. `emit` lets a script
*propose* a structured effect; a **trusted host** *disposes* of it — records the audit, executes
the intent, stores the findings. The box only captures and surfaces; it never interprets what an
effect means. This is the same "capability the author cannot opt out of" discipline as the egress
mux, applied to outputs instead of inputs.

## Why `kind` is required and structural

Each effect carries a required non-empty **`kind`** tag (≤ `max_emit_kind_len`, default 64 chars)
plus an opaque **`value`** (verbatim `JSON.stringify` output, preserved byte-for-byte via
`serde_json::RawValue`).

`kind` is the platform's **routing/governance slot**: the core may route, count, and — in a later
change — *authorize* effects by `kind` (gate a `charge` intent like a capability), but it never
interprets the meaning of a tag. An opaque, script-controlled `{type}` convention buried in the
JSON could not be trusted for authorization; a first-class tag can. Making `kind` required keeps
the whole effect stream uniformly typed, with no untyped escape hatch to special-case.

`value` stays fully opaque — the core neither validates nor inspects its shape, consistent with
the existing "the value is opaque to the core" philosophy.

## Capture-on-failure

Effects emitted **before** a handler throws are still surfaced, on the (error) response. The
per-invocation buffer drains after execution regardless of outcome, so an itemized run keeps
everything it found up to the crash — the one thing a bare `return` cannot do. Consumers should
treat a non-2xx response's `effects` as a *partial* trail.

## Bounds

- The number of effects per execution is capped by the existing per-execution `max_ops` bound; an
  `emit` past the cap fails deterministically rather than growing the buffer.
- `kind` is bounded by `max_emit_kind_len` (a plain character count, default 64); an empty,
  non-string, or over-long `kind` is rejected and records **nothing**.
- A run that never emits carries no `effects` key at all, so it is byte-compatible with the prior
  `{data, error, meta}` envelope.

## The three use-case clusters

1. **Audit / decision log** — emit the *why* of a decision alongside the answer.
2. **Itemized findings** — emit each mismatch a reconciliation run finds; keep them on a crash.
3. **Intent outbox** — emit `email` / `charge` actions a trusted host performs, so the sandbox
   never holds a credential.

## Deliberate non-goals (each reuses this same captured stream)

- **Durable disposition** — routing effects into the per-tenant `events.rs` stream (the durable
  billing/audit outbox seam). v1 disposition is "the trusted caller disposes via the response."
- **Per-kind authorization** — gating effects by `kind` via `authz.rs`. Designing `kind` in now is
  precisely what keeps that follow-on from re-breaking the API.
- **Incrementally streamed effects** (SSE) for progress / agent output. Effects still flush at run
  end.
- **The batch path** (`POST /batch`) surfacing effects.

## Historical note — the deleted `read` seam

`emit`'s dormant twin `read` (a consumer-supplied "read a declared dependency" hook) was **removed
outright** in this change, not reworked. Under a pre-supplied context model `read` is redundant
with `ctx`; as a live fetch it duplicates a capability. It had no product niche, and leaving dead
code invites it to resurface as a phantom option — so the effects channel is the single,
unambiguous "logic proposes, host disposes" story.
