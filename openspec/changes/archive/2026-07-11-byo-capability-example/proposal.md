## Why

The `byo-capabilities` pivot made composing your own capability the entire extension story — yet the model ships with **zero runnable example**. The only `CapabilityDef::new` call in the whole repo is its own definition site in `capability.rs`. A developer who wants to add a driver-shaped capability has the API surface and prose docs, but nothing to fork. This change gives them a tiny, compiled, copy-me template.

## What Changes

- Add **one minimal, compiled example capability**: an in-memory `kv` key-value store with two actions, `get(key)` and `set(key, value)`, backed by a `Mutex<HashMap<String, String>>`. No network, no credentials, no async, no SSRF — `Trust::OperatorSupplied` (there is no outbound target).
- The example shows the **four pieces that must stay in sync**, in one file:
  - a JS wrapper exposing `$std.kv` by routing through `$std.io.channel('kv')` / `io.call('kv', action, payload)`;
  - a Rust backend implementing `Egress::call(name, action, payload_json)`, matching on the snake_case action tokens `get` / `set`;
  - a `.d.ts` fragment declaring the `kv` global for editor IntelliSense;
  - the `CapabilityDef::new("kv", WRAPPER, TYPES, Trust::OperatorSupplied).with_backend(Arc::new(KvBackend))` that binds them together.
- It runs against **`LogicHost` directly** (no axum): compose `LogicHost::builder(...).capability(def).build()`, run one `Invocation` whose handler calls `$std.kv.set` then `$std.kv.get`, print the `{data}` result, and `assert!` the round-trip so CI/Docker keeps the example honest and non-rotting.
- Form: a **standalone example crate**, `examples/kv-capability/` (a new workspace member that deliberately does not inherit runlet-core's internal lint gauntlet — the `runlet-bench` precedent), runnable via `cargo run -p kv-capability`. The JS wrapper and `.d.ts` live inline as string literals in its `src/main.rs`, so the example reads like the normal Rust a real capability author writes in their own crate rather than runlet-core-internal house style.
- Add a short **fork-me README** section (beginner-doc tone, consistent with `docs/`): the call-loop diagram, the "these four snake_case tokens must agree" coupling diagram, and a "make it yours — change these four spots" list.

## Capabilities

### New Capabilities
- `capability-example`: A runnable, forkable reference showing how to compose a custom `CapabilityDef` (JS wrapper + `Egress` backend + `.d.ts` + registration) against `LogicHost`, using an in-memory `kv` store as the worked example.

### Modified Capabilities
<!-- None. The shipped runlet-core surface, its container/types.d.ts golden test, and the runlet server are untouched — this change only adds an example and docs. -->

## Impact

- **New files only:** `examples/kv-capability/{Cargo.toml, src/main.rs}` (the compiled example crate, with inline JS wrapper + `.d.ts` string literals) — added to the workspace `members` — and a fork-me section in the docs (`docs/`, plus a pointer from `README.md`).
- **No production code changes:** `runlet-core`, `runlet`, and `runlet-wire` keep their current surface. This routes through the existing `io` mux — **no** second in-engine primitive is added.
- **No golden-test change:** the example's `.d.ts` is the developer's own, standalone; it does not enter `container/types.d.ts` or its D11 golden test.
- **Build/CI:** the example compiles under the workspace (Docker build), so the strict lint gauntlet applies to it — it must be written to the same `no unwrap/expect/panic`, `no as` standard as shipped code, doubling as a demonstration of the house style.
- **Out of scope:** no HTTP/axum wiring, no `ScriptControlled`/SSRF demo, no `fabricd`.
