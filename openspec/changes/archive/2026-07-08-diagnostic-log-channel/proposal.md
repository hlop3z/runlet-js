## Why

A sandboxed `handler(ctx)` has **no `console`** — a script author is blind to intermediate
values, with no way to trace *why* a run behaved as it did. Runlet always sits behind the nexus
gateway (which will host a **browser playground/editor** where customers develop their apps), and
production apps need structured operational diagnostics while the playground needs logs shown
**inline while editing**. The just-shipped `emit(kind, value)` effects channel is the wrong tool:
effects are platform-facing, always-on, and disposed by the caller; diagnostics are
developer-facing, level-filtered, lossy, and must never pollute the billing/audit stream. This
change adds a first-class **diagnostic logging primitive** — `log.*` — routed by policy, invisible
to the script.

## What Changes

- **New `log.*` global** inside the sandbox: `log.trace/debug/info/warn/error`, with **Serilog-style
  message templates + named properties** (`log.info("charged {user} {amount}", { user, amount })`
  captures template + properties + rendered message) and **Pino-style bound context** via
  `log.with({ ... })`. Deliberately **not** `console.log` — `console` implies "prints to a stream
  you own," a promise a stateless box cannot keep.
- **Multi-sink routing (the Serilog sink model):** one structured log event, sinks chosen by
  policy, invisible to the script.
  - **Stream sink (primary, always-on):** logs flow to the per-tenant events pipeline
    (`events.rs`) as a **new `log`-type event**, tenant-keyed and `trace_id`-correlated,
    non-blocking (`try_send`, drop-on-full → a dropped counter). Logs get their **own bounded
    channel**, separate from the precious `usage`/`audit` events, so a chatty script can never
    starve billing/audit. Logs are **lossy / observability-grade** by design.
  - **Response mirror (gateway-gated):** when nexus sets a **trusted** debug flag on the request,
    the same entries are attached inline to the `/execute` response as a top-level `logs: [...]`
    array (omitted when absent), with **capture-on-failure** so a handler that logs-then-throws
    still returns its partial trail. This is the playground path ("Run → see output + logs
    inline"). The flag is **gateway-asserted, never caller-asserted**.
- **Level floor** (`config.log_level`): checked in the JS wrapper **before** building the entry
  (Pino's cost trick), so a below-floor `log.debug` is nearly free on the always-on hot path.
- **Bounds:** a per-execution log-count cap (mirroring `max_ops`) and a per-entry size bound, so
  one execution cannot emit unbounded volume — reusing the `emit-effects-channel` bounding pattern.
- **Determinism:** logs are **outside** the bit-reproducible `data`/`effects` contract — ordered by
  a deterministic `seq`; a relative-microsecond offset is available only on the non-deterministic
  (`Profile::Full`) path, since `Profile::Deterministic` strips the clock.

## Capabilities

### New Capabilities
- `diagnostic-logging`: the sandbox `log.*` channel — leveled structured entries (template +
  properties + rendered message + `seq`), bound context, the level floor and per-execution bounds,
  the determinism-exclusion rule, and the two policy-selected sinks (the always-on tenant **stream**
  on its own lossy channel; the gateway-gated **response mirror** with capture-on-failure). The core
  surfaces structure but never interprets a log's meaning; sink selection is gateway policy, never
  caller-asserted.

### Modified Capabilities
- `execution`: the `/execute` response MAY carry a top-level `logs` list when the trusted gateway
  requests it (absent otherwise), on both the success and error paths; additive to the existing
  `{data, error, meta, effects?}` contract.

## Impact

- **Code — `crates/runlet-core`** (`engine.rs`, `host.rs`, `config.rs`, `js/`): a `log.*` injector
  + native `__log` FFI (mirroring `inject_emit`); a bounded per-invocation log buffer drained after
  execution on **both** paths (capture-on-failure); a public `LogEntry` type threaded
  `ExecResult → Outcome`; `EngineConfig.log_level` + per-exec log cap; the `log.d.ts` fragment;
  determinism note. Core stays domain-agnostic and links nothing new.
- **Code — `crates/runlet`** (`events.rs`, `handler.rs`, `config.rs`): a new `EventBody::Log`
  variant on its **own** bounded channel + dropped counter; emission of one log event per captured
  entry keyed by tenant/`trace_id`; a gateway-gated top-level `logs` field on the response
  (success + error); the trusted debug-flag plumbing from the identity/trusted-header layer.
- **Wire/API:** additive `/execute` response field (`logs`); a new tenant-stream event type. **No
  change to `runlet-wire`** — edge-local, not the cross-repo `fabricd` contract.
- **Docs/DX:** a `docs/design/` rationale doc (the multi-sink model, the influences, why not
  `console.log`); `log.*` in `base.d.ts` + regenerated `container/types.d.ts` (D11 golden test);
  README/beginner reference.

Out of scope (deliberate follow-ups): durable/guaranteed log delivery (logs stay lossy); per-tenant
routing to a customer-owned external sink (Fastly BYO-endpoint style); log-based metrics; nestable
tracing spans (the request is already one OTel span server-side); the playground UI itself (nexus).
