# The diagnostic log channel — `log.*`

A sandboxed `handler(ctx)` has **no `console`**. A script author is blind to intermediate values,
and a failing run leaves no trace of *why* it behaved as it did. `log.*` is the first-class
diagnostic primitive that fills that gap — structured, leveled, lossy, and routed by platform
policy the script never sees.

```js
function handler(ctx) {
  log.info("charged {user} {amount}", { user: ctx.id, amount: "10.00" }); // → "charged 42 10.00"
  const l = log.with({ requestId: ctx.reqId });   // bound context on every entry from `l`
  if (retrying) l.warn("retrying attempt {n}", { n });
  return json({ ok: true });
}
```

## Why `log.*`, not `console.log`

`console` carries the mental model "prints to a stream you own." A stateless box behind a gateway
cannot honor that: output is *captured*, *policy-routed*, and *sometimes dropped*. A dedicated
`log` global with explicit levels (`trace`/`debug`/`info`/`warn`/`error`) names the thing honestly
and avoids dragging in `console.error`/`console.table`/… expectations that lie about where output
goes. (D1)

## `log` vs `emit` — two different jobs

They look similar (both capture a per-invocation buffer, both survive a throw) but their *policies*
diverge on every axis, which is why they are two primitives, not one:

| | `emit(kind, value)` | `log.<level>(template, props)` |
|---|---|---|
| Audience | the **platform** (record/act) | the **developer** (diagnose) |
| Delivery | always-on, never dropped | level-filtered, **lossy** |
| Determinism | inside the reproducible `effects` | **outside** `data`/`effects` |
| Disposition | the host authorizes/performs | routed to sinks by policy |

Rule of thumb: **`emit` is what the run *did*; `log` is how it *went*.** Billing/audit/intent →
`emit`. "Why did this return 3?" → `log`.

## Message templates (Serilog), not a flat string

`log.info("charged {user} {amount}", { user, amount })` captures the **template**, the **named
properties**, and a **rendered message**. You get a human-readable line *and* queryable structured
fields from one call: a playground panel can color by level and render the key/values, and a future
indexed sink can filter on `user` without reparsing strings. The `{name}` substitution happens in
JS (where the properties object already lives — no Rust-side reparse); an unknown placeholder is
left verbatim. (D2)

`log.with({ ... })` derives a logger whose bound context is merged into every subsequent entry
(Pino-style child). On a key collision the **per-call property wins** over the bound value — the
most-specific value at the call site. (OQ3)

## The multi-sink model — one call, sinks chosen by policy

The script calls `log.info(...)`; **platform policy** (config + the gateway's trusted flags)
decides which sinks receive it. The author never branches on environment, and sinks can be added or
removed without touching the `log.*` API. (D3, Serilog's core idea.) Two sinks exist today:

- **Stream sink (always-on).** Captured entries flow to the per-tenant `events.rs` pipeline as a
  new `log`-type event, tenant-keyed and `trace_id`-correlated, non-blocking (`try_send`,
  drop-on-full). Logs get their **own bounded channel**, separate from the precious `usage`/`audit`
  events, so a chatty script can never starve billing/compliance — the load-bearing reliability
  call (D4). Its backpressure shows up as `runlet_log_events_dropped_total`.
- **Response mirror (gateway-gated).** When the **trusted gateway** sets a capture flag on the
  request, the same entries are attached inline as a top-level `logs: [...]` on the `/execute`
  response (omitted otherwise), with **capture-on-failure** so a log-then-throw run still returns
  its partial trail. This is the playground path ("Run → see output + logs inline"). The flag is
  **gateway-asserted, never caller-asserted** (D5): an untrusted end-user can neither force capture
  nor read another tenant's logs. Capture is resolved in the identity layer alongside the other
  trusted signals — there is no separate, weaker debug-auth path.

### Test vs live (Stripe's isolation model, OQ1)

Every execution carries a gateway-asserted **mode**, orthogonal to capture. A **live** run streams
to the tenant log stream (and *also* mirrors on the response if capture is requested — debugging
real traffic is allowed). A **test/playground** run is response-mirror-only and **never enters the
live stream, billing, or audit** — test data never reaches live. The mode is a platform-wide
dimension; this change only *consumes* it for log routing.

## The level floor is checked first (Pino's cost trick, D6)

A `config.log_level` floor (default **`info`**, the industry production default) is checked in the
JS wrapper **before** merging context or serializing properties, so a stripped `log.debug` is
nearly free on the always-on hot path. The trusted gateway MAY **lower the floor per-request** for
a capture run (OQ2, Lambda's model), so production stays `info`+ while the playground gets
`debug`/`trace`.

## Bounds — Vercel's per-request triad (D7/OQ4)

Per execution: **256 entries**, **256 KB/entry**, **1 MB total** (the total binds first). A call
past the count or total cap records nothing; an oversize entry is truncated — its properties become
a `{"truncated":true}` marker (like Cloudflare Workers' `$cloudflare.truncated`) and its message is
trimmed to a char boundary. All three are configurable (`max_log_entries` /
`max_log_entry_bytes` / `max_log_total_bytes`).

## Determinism — outside the reproducible contract (D8)

Logs never influence `data`/`effects`. Ordering is a deterministic **`seq`** counter (safe under
`Profile::Deterministic`); a relative **microsecond `offset_us`** is attached **only** under
`Profile::Full`, since the deterministic profile strips the clock. A deterministic script is often
exactly the one you want to debug, so logging under it is allowed — just without timing, and with
byte-identical outputs across runs.

## Build, not adopt (the dependency decision)

No new dependency. The Message Templates model is a language-neutral *spec* with no Rust
implementation, and `tracing`/`slog` target *Rust-side, compile-time* logging — whereas our events
originate dynamically from **JS calls in QuickJS** and must be captured into a per-tenant buffer.
Bridging them through `tracing` + `tracing-capture` is more machinery than capturing structured
entries directly and fights tracing's compile-time-fields model. We adopt the *design* (Message
Templates semantics + tracing's level model + Serilog sinks + Pino's cost/child ideas), not a
crate. The engine FFI reuses the vetted string-in/JSON-string bridge `emit` and every capability
already use; the stream sink extends the existing `events.rs` pipeline; the capture gate extends
the trusted-identity layer.

## Deliberate non-goals

- **Durable / guaranteed delivery** — logs stay lossy (observability-grade).
- **Per-tenant routing to a customer-owned external sink** (Fastly BYO-endpoint style).
- **Log-based metrics; nestable tracing spans** (the request is already one server-side OTel span).
- **A durable test-namespace stream** — v1 test/playground runs are response-mirror-only.
- **The playground UI itself** (nexus owns it) and **the `/batch` path surfacing logs.**
