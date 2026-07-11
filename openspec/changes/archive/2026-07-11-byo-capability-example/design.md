## Context

The `byo-capabilities` change removed the shipped driver preset and made composing your own
`CapabilityDef` the whole extension model, documented in `docs/design/composable-core.md` and
`docs/03-capabilities.md`. But there is no runnable example: the only `CapabilityDef::new` call in
the repo is its definition site in `crates/runlet-core/src/capability.rs`. This change adds the
missing worked example as a compiled, forkable template.

The extension seam is already fully in place — this design only assembles existing public API:

- `CapabilityDef::new(name, js_wrapper, types, trust)` + `.with_backend(Arc<dyn Egress>)`
  (`capability.rs`).
- `Trust::OperatorSupplied` (no SSRF policy — the simplest trust).
- The `Egress` port: `fn call(&self, name, action, payload_json) -> Result<String, EgressError>`
  (`runlet-wire`, re-exported as `runlet_core::Egress`).
- Host composition: `LogicHost::builder(pool, registry, settings).capability(def).build()`.
- `Invocation::inline(source, ctx).caps(CapabilitySet { io: &["kv"], ..CapabilitySet::NONE })`;
  a def with a bound backend needs **no** per-request `.egress(...)` — the mux precedence is
  local backend → per-request fallback → builder fallback, and a registered non-empty def set
  makes `registry.is_active()` true so the `io` global is injected. The wrapper for `kv` is
  injected because `caps.io` names it and `Profile::Full` (the default) allows I/O.

The JS wrapper routes through the existing `io` mux (`$std.io.channel('kv')`), so **no** new
in-engine primitive is added — the enumerated bypass surface (`http`/`s3`) is untouched.

## Goals / Non-Goals

**Goals:**
- One single-file, compiled example — `crates/runlet-core/examples/kv.rs` — runnable via
  `cargo run -p runlet-core --example kv`, no new crate manifest.
- Show the four coupled pieces in one file: JS wrapper, `Egress` backend, `.d.ts` fragment,
  `CapabilityDef` registration — with an `assert!` round-trip so CI/Docker keeps it honest.
- A beginner-tone fork-me doc section: the call-loop diagram, the "four tokens must agree"
  diagram, and a "change these spots" list.
- Written to the house lint standard (no `unwrap`/`expect`/`panic`, no `as`) so the example
  itself demonstrates the style a real capability author must follow.

**Non-Goals:**
- No HTTP/axum wiring (the example drives `LogicHost` directly — the actual port).
- No `ScriptControlled`/SSRF demo, no `fabricd`, no network, no credentials, no async.
- No change to shipped `runlet-core`/`runlet`/`runlet-wire` surface; no change to
  `container/types.d.ts` or its D11 golden test.
- No second in-engine primitive; no new dependency.

## Decisions

### D1 — Form: a standalone `examples/kv-capability/` crate that does NOT inherit the workspace lints (Build)
Originally scoped as a single `crates/runlet-core/examples/kv.rs`. **Reversed during apply**: any
target inside `runlet-core` inherits the crate's internal lint gauntlet (`print_stdout`,
`unwrap_used`, `missing_docs_in_private_items`, `unused_crate_dependencies` all **denied**, with
`allow_attributes` forbidden so suppression needs fragile `#![expect(...)]` that itself errors if
the lint doesn't fire). Two reasons this is the wrong bar for the example:
1. **Accuracy.** A real BYO-capability author writes their `CapabilityDef` in *their own crate with
   their own (normal) lints* — they never inherit this repo's house gauntlet. Forcing the example
   through it would teach a standard the actual extension model does not impose.
2. **Simplicity (the stated goal).** The gauntlet fights "so simple a kid can understand it": a demo
   buried in per-item docs and `#![expect]` gymnastics is not kid-simple.

So the example is a tiny **standalone crate**, `examples/kv-capability/` (a new workspace member),
that deliberately does **not** set `[lints] workspace = true` — exactly the `runlet-bench` precedent
(dev/example scaffolding opts out, with a documented reason). Run via `cargo run -p kv-capability`.
The JS wrapper and `.d.ts` live **inline as `const &str` literals** in `src/main.rs` so the whole
thing reads top-to-bottom. No new third-party dependency and no cargo-vet churn: it depends only on
the `runlet-core` path crate plus `serde_json`, both already in the tree. *Build over Adopt:* there
is nothing to adopt — this is clean glue over our own public API.

### D2 — Capability shape: in-memory `kv` with `get`/`set`
The store is a `Mutex<HashMap<String, String>>` owned by the backend struct and shared via
`Arc`. Two actions keep it minimal while still modeling the canonical "named resource + verbs"
mental model (a single stateless action would under-teach). String→string keeps payload parsing
trivial. Concurrency via `Mutex` is honest (the pooled runtime is `Send + Sync`) and still
kid-simple; a poisoned-lock path is handled without `unwrap` (map the `PoisonError` to an
`EgressError`, or recover the guard) to satisfy the lint gauntlet.

### D3 — Backend contract: parse payload, match action, return JSON string
`Egress::call` receives `payload_json` (the wrapper JSON-stringifies its args). The backend
`serde_json`-parses `{key}` / `{key,value}`, matches on the `action` token, and returns a JSON
**string**:
- `get` → `{"value": "<v>"}` or `{"value": null}` for a miss.
- `set` → `{"ok": true}`.
- any other action → `Err(EgressError::new("kv", "KV_BAD_ACTION", …))` (surfaces as a capability
  error, never a panic). A malformed payload → `Err(EgressError::new("kv", "KV_BAD_PAYLOAD", …))`.
This mirrors how a real driver backend behaves and demonstrates the error taxonomy without
depending on it.

### D4 — JS wrapper: thin, unwraps `get` to the bare value
```
var c = $std.io.channel('kv');
$std.kv = {
  get: function(key){ return c('get', { key: key }).value; },   // → "Ada" | null
  set: function(key, value){ return c('set', { key: key, value: value }); } // → { ok: true }
};
```
`get` returns the bare value so `$std.kv.get("name")` reads naturally in a handler; `set` returns
the backend envelope. The method names and the `io.call` action tokens are the **same**
`snake_case` spelling — the coupling the fork-me doc highlights.

### D5 — Wiring: bound backend, `caps.io = &["kv"]`, `Profile::Full`
`CapabilityDef::new("kv", KV_WRAPPER_JS, KV_TYPES_DTS, Trust::OperatorSupplied)
.with_backend(Arc::new(KvBackend::default()))`, registered on `LogicHost::builder(...)`.
The `Invocation` uses `CapabilitySet { io: &["kv"], ..CapabilitySet::NONE }` (spread over `NONE`
to stay correct under the feature-gated `http`/`s3` fields) and no per-request egress. The pool +
registry + `HostSettings` are built from `EngineConfig`'s defaults so the example is
self-contained.

### D6 — Docs live with the capability guide, not a new top-level doc
The fork-me section is appended to the existing capabilities beginner doc (`docs/03-capabilities.md`
or a sibling under `docs/`) with a one-line pointer from `README.md`, keeping the doc taxonomy
intact (no `/research`-style orphan). It carries the two ASCII diagrams and the edit-site list.

## Risks / Trade-offs

- **`LogicHost`-only under-sells the server story.** A reader might want the full HTTP wiring. The
  fork-me doc mitigates with one line: "the shipped `runlet` server is this same `.capability(def)`
  call plus an axum adapter." Accepted — showing axum would swamp the lesson.
- **Example rot.** Mitigated by compiling under the workspace + the `assert!` round-trip; if the
  builder API changes, the example fails to build in CI/Docker. It is intentionally not added to
  the Python integration harness (that suite is box-only, no `LogicHost` embedding) — the compile +
  assert is the guard.
- **Lint gauntlet on example code.** Resolved by D1: the example crate opts out of the workspace
  gauntlet (like `runlet-bench`), so it reads like the normal Rust a real author writes rather than
  runlet-core-internal house style. It still gets default clippy correctness lints; it is simply not
  held to the repo's restriction lints, which the extension model does not impose on users anyway.
- **Feature-gating.** `CapabilitySet` has `#[cfg(feature = "http"/"s3")]` fields; constructing via
  `{ io: …, ..CapabilitySet::NONE }` keeps the example correct under any feature combination,
  including `--no-default-features`. The example needs none of those features.
