# Tasks — diagnostic-log-channel

> Build-vs-adopt gate + design open questions are settled in design.md (all **Extend**/**Build**, no
> new dependency; OQ1–OQ5 locked to industry standards). Locked values: **level floor default
> `info`**, gateway may lower per-request; bounds **256 entries/exec, 256 KB/entry, 1 MB total/exec**
> (total binds first, oversize → `truncated`); `log.with` **per-call keys override bound context**;
> **mode** (live vs test) + **capture** are orthogonal **trusted headers** (identity-layer resolved),
> a test/playground run is **response-mirror-only** and never streams. Reuse the archived
> `emit-effects-channel` machinery throughout (bounded per-invocation buffer, drain on both paths,
> public entry type threaded `ExecResult → Outcome`).

## 1. Core: the `log.*` primitive (`crates/runlet-core`)

- [x] 1.1 Add a native `__log(level, template, properties_json, seq?)` FFI + a `log` JS wrapper with `trace/debug/info/warn/error` and `log.with(fields)` (bound context merged into per-call properties), mirroring `inject_emit`/`js/*.js`
- [x] 1.2 Level floor: check `config.log_level` in the JS wrapper **before** stringifying properties (D6); a below-floor call returns immediately, recording nothing
- [x] 1.3 Capture each accepted call into a bounded per-invocation log buffer as `(level, template, properties_json, seq)`; assign a monotonic `seq`; enforce the D7 triad (256 entries/exec, 256 KB/entry, 1 MB total/exec — total binds first), dropping past the count/total and truncating an oversize entry with a `truncated` marker
- [x] 1.4 Add a public `LogEntry` type (`{ level, template, properties, message, seq, offset_us? }`); drain the buffer after execution on **both** the success and error paths (capture-on-failure, D8/spec) into `ExecResult` → `Outcome`
- [x] 1.5 Determinism (D8): `seq` always; attach the relative `offset_us` only under `Profile::Full` (none under `Profile::Deterministic`, which strips the clock); logs never touch `data`/`effects`
- [x] 1.6 Add `log_level` (default `info`) + the cap/size knobs (`max_log_entries`=256, `max_log_entry_bytes`=256 KB, `max_log_total_bytes`=1 MB) to `EngineConfig` (`config.rs`); thread through `ExecParams`; support the trusted per-request floor override (D6/OQ2)
- [x] 1.7 Core stays domain-agnostic and links nothing new; `runlet-wire` untouched

## 2. Edge — stream sink (`crates/runlet`, `events.rs`)

- [x] 2.1 Add an `EventBody::Log` variant (level, template, properties, message, seq, offset?) to the versioned envelope, keyed by tenant + `trace_id`
- [x] 2.2 Give logs their **own** bounded channel + dropped counter, separate from the `usage`/`audit` channel (D4) — a metric like `runlet_log_events_dropped_total`; non-blocking `try_send`, drop-on-full
- [x] 2.3 After execution, emit one `log` event per drained `LogEntry` to the diagnostic channel; verify a saturated log channel never drops `usage`/`audit` events (spec scenario)

## 3. Edge — response mirror (`crates/runlet`, `handler.rs`, trusted layer)

- [x] 3.1 Resolve the **trusted** `mode` (live vs test) + `capture` signals from the gateway as trusted headers (via the identity layer, `identity.rs`/`authz.rs`; OQ5) — never caller-asserted body fields; default `(live, no-capture)`
- [x] 3.2 When capture is requested, attach a top-level `logs: [...]` to the `/execute` response on **both** the success and error paths; omit the field otherwise (byte-compatible with `{data, error, meta, effects?}`)
- [x] 3.3 Apply the OQ1 routing: a **live** run streams to the production tenant stream (§2); a **test/playground** run is **response-mirror-only** and MUST NOT enter the live stream/billing/audit — carry the mode tag so §2 emission is suppressed for test runs
- [x] 3.4 Confirm an untrusted caller-asserted debug flag surfaces no logs (spec scenario)

## 4. Tests

- [x] 4.1 Core: `log.info` records a structured entry (level, template, properties, rendered message, seq); order preserved across calls
- [x] 4.2 Core: a below-floor `log.debug` records nothing (and does not evaluate properties)
- [x] 4.3 Core: `log.with(ctx)` merges bound context into subsequent entries
- [x] 4.4 Core: exceeding the per-execution cap drops further entries; oversize entry truncated
- [x] 4.5 Core: a handler that logs then throws → error outcome still carries the entries (capture-on-failure)
- [x] 4.6 Core: deterministic run → identical `data`/`effects` with logging; `seq` present, no timing; full-profile run → `offset_us` present
- [x] 4.7 Edge: a saturated log channel drops logs but not `usage`/`audit` events
- [x] 4.8 Edge: with the trusted flag, `/execute` returns top-level `logs` on 2xx and non-2xx; without it (and with a caller-asserted flag) the envelope is unchanged

## 5. Docs, DX & wrap-up

- [x] 5.1 Add `log.*` to `base.d.ts` (levels, template signature, `with`) + regenerate `container/types.d.ts` (D11 golden test)
- [x] 5.2 Add a `docs/design/` rationale doc: the multi-sink model, the influences (Serilog templates + sinks, tracing levels, Pino cost/child), why not `console.log`, and the tenant-stream-vs-response-mirror split
- [x] 5.3 Update README / beginner docs with the `log.*` reference and the "diagnostics, not billing; lossy" framing; cross-link `emit` vs `log`
- [x] 5.4 `task fmt` + `task clippy` clean (strict gauntlet); `cargo test` green; verify via Docker (native cargo is WDAC-blocked)
- [x] 5.5 `/opsx:sync` the delta specs into main specs, then `/opsx:archive`
