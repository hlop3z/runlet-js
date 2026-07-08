# Tasks — emit-effects-channel

> Open questions are settled in design.md: D6 top-level `effects`; D7 `kind` ≤
> `max_emit_kind_len` (default 64), allowlist deferred; D8 no per-kind metering here; D9 config
> knob `max_emit_kind_len`.

## 1. Delete the dormant `read` seam (`crates/runlet-core`)

- [x] 1.1 Delete `inject_read` and the `__read`/`read` globals (`engine.rs:568`) and their injection call (`engine.rs:321-323`)
- [x] 1.2 Delete the `ReadHook` type (`engine.rs:117`), `Invocation.read_hook` field + builder (`host.rs:128,204`), and the `ExecParams.read_hook` threading (`host.rs:402`, `engine.rs:149`)
- [x] 1.3 Update the two test constructions that set `read_hook: None` (`engine.rs:1148`, `engine.rs:1336`)
- [x] 1.4 Remove `read`/`read_hook` references from `crates/runlet-core/CONSUMER_NOTES.md`
- [x] 1.5 Grep-confirm no remaining references (`read_hook`, `inject_read`, `__read`); `cargo build -p runlet-core` clean

## 2. Core: tagged effects capture (`crates/runlet-core`)

- [x] 2.1 Change `inject_emit` (`engine.rs:533`) to the `emit(kind, value)` signature: native `__emit(kind, value_json)`; JS wrapper `emit(kind, value)` stringifies `value` and forwards `kind`
- [x] 2.2 Validate `kind` (non-empty string, ≤ `max_emit_kind_len`); on failure throw a deterministic error and record nothing
- [x] 2.3 Change the effects buffer element + drained entry from a bare `RawValue` to `{ kind: String, value: Box<RawValue> }`; keep the `max_ops` cap (`engine.rs:541`)
- [x] 2.4 Update `drain_effects` (`engine.rs:671`) and the `ExecResult.effects` / `Outcome.effects` types (`host.rs:230`) to the tagged entry; confirm the post-run drain still runs on the error path (capture-on-failure)
- [x] 2.5 Add `max_emit_kind_len` to `EngineConfig` (`crates/runlet-core/src/config.rs`) — a plain count mirroring `max_ops`, default 64

## 3. Edge: surface effects on the response (`crates/runlet`)

- [x] 3.1 Stop dropping `Outcome.effects` in `build_response` (`handler.rs:1672`); serialize the `[{kind, value}]` list
- [x] 3.2 Attach a **top-level** `effects` field (D6) on the **success** path → `{data, error, meta, effects}`
- [x] 3.3 Attach `effects` on the **error** path too, so a handler that emits-then-throws returns its partial trail (spec: effects survive handler failure)
- [x] 3.4 Ensure a run that never emits yields an empty/absent `effects` and is otherwise byte-compatible with the prior `{data, error, meta}` response

## 4. Tests (first behavioral coverage for emit)

- [x] 4.1 Core: `emit("a",1); emit("b",2); emit("a",3)` → effects preserve order + duplicates as tagged entries
- [x] 4.2 Core: `emit("", v)` and single-arg `emit(v)` fail deterministically and record nothing
- [x] 4.3 Core: a handler that emits then throws → error outcome still carries the emitted effects (capture-on-failure)
- [x] 4.4 Core: exceeding the `max_ops` emit cap fails the over-limit call
- [x] 4.5 Core: an over-length `kind` (> `max_emit_kind_len`) is rejected
- [x] 4.6 Edge: `/execute` returns top-level `effects` on a 2xx (with values) and on a non-2xx (partial trail); a no-emit request is unchanged from the prior envelope
- [x] 4.7 Regression: no test references `read`/`read_hook`; suite green after removal

## 5. Docs, DX & wrap-up

- [x] 5.1 Update the `emit` `.d.ts` fragment + regenerate `container/types.d.ts` for the `(kind, value)` signature (D11 golden test)
- [x] 5.2 Update `determinism.js`'s note that effects are preserved to match the tagged shape
- [x] 5.3 Add a `docs/design/` rationale doc for the effects channel + propose/dispose model and the ①②③ use cases; keep WHY out of specs
- [x] 5.4 Update beginner docs / `README.md` reference for `emit(kind, value)` (currently undocumented as usable)
- [x] 5.5 `task fmt` + `task clippy` clean (strict lint gauntlet); `cargo test` green; run an `/execute` integration check via Docker
- [x] 5.6 `/opsx:sync` the delta specs into main specs, then `/opsx:archive`
