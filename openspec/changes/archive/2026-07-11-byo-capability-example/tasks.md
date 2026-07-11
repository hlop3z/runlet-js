## 1. The compiled example crate (`examples/kv-capability/`)

- [x] 1.1 Add `examples/kv-capability/Cargo.toml` (package `kv-capability`, a bin) depending on the `runlet-core` path crate + `serde_json`; deliberately **omit** `[lints] workspace = true` so it does not inherit runlet-core's internal restriction gauntlet (the `runlet-bench` precedent — documented with a comment). Add `examples/kv-capability` to the workspace `members` in the root `Cargo.toml`.
- [x] 1.2 In `src/main.rs`, define `KV_WRAPPER_JS: &str` inline — the JS wrapper exposing `$std.kv.get(key)` (returns the bare `.value`) and `$std.kv.set(key, value)`, routing through `$std.io.channel('kv')` / `io.call('kv', action, payload)`.
- [x] 1.3 Define `KV_TYPES_DTS: &str` inline — a small `.d.ts` fragment declaring the `kv` global (`get(key: string): string | null`, `set(key: string, value: string): { ok: boolean }`), prefixed interface name to keep the shared TS namespace flat.
- [x] 1.4 Implement `struct KvBackend` holding `Arc<Mutex<HashMap<String, String>>>` (via `Default`), and `impl Egress for KvBackend`: `serde_json`-parse the payload, match `action` on `get`/`set`, return the JSON string envelopes from design D3; unknown action → `KV_BAD_ACTION`, bad payload → `KV_BAD_PAYLOAD` `EgressError`s. Recover the `Mutex` poison guard rather than panicking.
- [x] 1.5 In `main`, compose the host: build a `JsPool` + `ScriptRegistry` + `HostSettings` from `EngineConfig` defaults, then `LogicHost::builder(...).capability(CapabilityDef::new("kv", KV_WRAPPER_JS, KV_TYPES_DTS, Trust::OperatorSupplied).with_backend(Arc::new(KvBackend::default()))).build()`.
- [x] 1.6 Run one `Invocation::inline(HANDLER_JS, "{}")` with `caps(CapabilitySet { io: &["kv"], ..CapabilitySet::NONE })` where `HANDLER_JS` does `$std.kv.set('name','Ada')` then returns `{ data: $std.kv.get('name') }`; print the `Outcome` `{data}`.
- [x] 1.7 On a wrong round-trip, `return Err(...)` from `main` (non-zero exit, no panic/`exit()`), so a regression fails the run; on success print a confirmation.
- [x] 1.8 Add a top-of-file doc comment: what the example teaches, the run command, and a pointer to the fork-me doc section.

## 2. The fork-me documentation

- [x] 2.1 Add a beginner-tone "Bring your own capability — a worked example" section to the capabilities doc (`docs/03-capabilities.md` or a sibling under `docs/`) with the call-loop diagram and the "four `snake_case` tokens must agree" diagram.
- [x] 2.2 Add the "make it yours — change these spots" list naming each edit site (capability name, action tokens, backend body, wrapper methods, `.d.ts` signatures) and link to `examples/kv-capability/src/main.rs` + the run command.
- [x] 2.3 Add a one-line pointer from `README.md` to the example + fork-me section.

## 3. Verify (Docker build — native build blocked by WDAC/aws-lc-sys)

- [x] 3.1 `cargo run -p kv-capability` builds and prints `Ada`, exit 0 (run in the project's Docker toolchain).
- [x] 3.2 `cargo clippy -p kv-capability` and workspace `cargo clippy` are clean (the example crate carries default clippy correctness lints; runlet-core's restriction gauntlet is unchanged and still enforced on the shipped crates).
- [x] 3.3 `cargo fmt --all --check` passes.
- [x] 3.4 Confirm the shipped `container/types.d.ts` and its `types_dts_is_up_to_date` golden test are unchanged (the example's `.d.ts` is standalone, never concatenated).
- [x] 3.5 Confirm no new third-party dependency and no change to `runlet-core`/`runlet`/`runlet-wire` public surface (new example crate + docs only).
