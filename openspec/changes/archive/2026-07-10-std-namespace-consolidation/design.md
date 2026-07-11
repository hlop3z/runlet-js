## Context

The sandbox injects its built-ins as ~13 bare globals plus a second namespace object
`$sys`. Each `crates/runlet-core/src/js/*.js` file is an IIFE that writes
`globalThis.<name> = …`; util files cross-reference each other through those same globals
(`list.js` reads `globalThis.Decimal`/`money`/`dict`; `money.js` reads `globalThis.Decimal`;
`dict.js` reads `globalThis.list`). `determinism.js` enforces `Profile::Deterministic` by
`delete`-ing ambient authorities (`Math.random`, `datetime.now`, `$sys.crypto.uuid`) and
replacing the no-arg `Date()` clock. Typing lives in `base.d.ts` as per-util interfaces
plus `declare const <name>: <Factory>`; it is assembled into `container/types.d.ts` and
guarded by the `types_dts_is_up_to_date` golden test (D11).

There are **no current users**, so we can consolidate the author-facing surface without a
compatibility layer. This change is a pure surface refactor: no new dependency, no new
behavior. The minijinja-backed `template` util and the `unit`/`csv`/`tax`/`pricing`/`check`
utils are deliberately deferred to follow-on changes so this one stays dependency-free and
its blast radius legible.

## Goals / Non-Goals

**Goals:**
- One canonical, discoverable namespace `$std` that owns every built-in, defined once.
- Bare globals become a thin, declarative *projection* of `$std` — same object references.
- Delete `$sys`; relocate its members under `$std` (crypto grouped; env/secrets hoisted).
- Preserve every existing runtime behavior and security invariant (determinism prune,
  opaque secrets, capability gating) unchanged — only access paths move.
- Single-source typing: `$std.*`, destructuring, and the exposed globals all type-check off
  one interface; golden test keeps generated types in sync.

**Non-Goals:**
- No new util (`template`, `unit`, `csv`, `tax`, `pricing`, `check`) — separate changes.
- No new dependency; no change to capability composition (`CapabilityDef`, egress mux).
- No backwards-compatibility aliases or deprecation path (hard cutover).
- No change to the `{data, error, meta}` wire envelope or the HTTP surface.

## Decisions

### D1 — Globals are a projection of `$std`, driven by one `EXPOSE` list

`$std` is the sole container. A single declarative map projects a curated subset onto
`globalThis`, each mirror being the identical object reference:

```
EXPOSE = { $: "money", json: "json", log: "log", emit: "emit" }
for (g, member) of EXPOSE:  globalThis[g] = $std[member]
```

Result: exactly two `$*` globals (`$std`, `$`) plus the bare verbs `json`/`log`/`emit`.
There is no "verb vs. namespace" ontology — a member is a global purely because it is on
the list. Adding/removing a global is a one-line edit; nothing moves.

*Alternatives considered:* (a) keep bare globals as independent definitions and *also* add
`$std` — rejected: two sources of truth, drift risk, `$sys`-style arbitrariness persists.
(b) put *everything* under `$std` with no bare globals at all — rejected: `$`, `json`,
`log`, `emit` are written on nearly every script; forcing `$std.log(...)` is a real
ergonomic tax for the highest-frequency calls.

### D2 — Only pure members are eligible for exposure (the D9/WASI invariant)

An exposed global is a *second reference* to a `$std` member. If a prunable authority were
mirrored, `determinism.js`'s `delete $std.crypto.uuid` would leave the global copy alive —
re-opening ambient authority, exactly the leak D9 warns of. Therefore the exposure list
contains only pure, both-profile members (`money`, `json`, `log`, `emit`). Prunable
authorities (`datetime.now`, `crypto.uuid`, `Math.random`, no-arg `Date()`) live only at
their canonical path and are never mirrored.

*Corollary — internal cross-reference rule.* A util file MAY capture a pure sibling at load
(`var D = $std.decimal;`) for speed and decoupling, but any reference to a prunable
authority MUST remain a live `$std.*` lookup at call time, so a captured closure cannot
survive the determinism `delete`. Today's `list.js` looking up `globalThis.Decimal` at call
time is over-cautious for a pure util (safe to hoist to a load-time capture); that same
pattern would be *mandatory* if it ever touched `uuid`/`now`.

### D3 — Setup ordering: build → project → eval → prune → freeze

```
1. bootstrap  globalThis.$std = {}
2. build      pure utils into $std (decimal before money), then crypto/env/secrets,
              then json/log/emit, then profile+config-gated io/http/s3
3. project    apply EXPOSE → globalThis        (BEFORE user code, so scripts see $/json/log/emit)
4. eval       user script
5. sanitize   remove eval/Proxy
6. prune      (Profile::Deterministic only) delete $std.datetime.now, $std.crypto.uuid,
              Math.random; replace Date()
7. freeze     deep-freeze $std; lock EXPOSE'd global bindings non-writable/non-configurable
8. invoke     handler(ctx)
```

**Prune-before-freeze is a hard ordering constraint** — freezing first would make the
determinism deletions fail. Deep-freeze after prune locks the pruned state in and makes the
whole surface tamper-proof for the handler. Under `Profile::Deterministic`, io/http/s3 are
simply not built into `$std` in step 2 (absent, not stubbed), preserving today's behavior.

*Alternative considered:* keep building bare globals in each IIFE and "reparent" into `$std`
at the end (a collect step), leaving the internal cross-refs alone. Rejected: it leaves the
util call-time lookups pointing at `globalThis.<name>` which we then delete, breaking them
at runtime; and it keeps a transient window where the flat globals exist. Rewriting the
references directly (D2 rule) is cleaner and correct.

### D4 — Single-source typing via an indexed-access projection

`base.d.ts` declares the shape once and derives the mirror-global declarations from it:

```ts
interface Std { money: MoneyFactory; decimal: DecimalFactory; text: TextFactory;
  datetime: DateTimeFactory; list: ListFactory; dict: DictFactory;
  io: Io; http: Http; s3: S3; crypto: SysCrypto; env: SysEnv; secrets: SysSecrets;
  json: JsonFn; log: Logger; emit: EmitFn }
declare const $std: Std;
declare const $:    Std["money"];   // mirror declares are DERIVED from Std…
declare const json: Std["json"];    // …so they cannot drift from the namespace
declare const log:  Std["log"];
declare const emit: Std["emit"];
```

Both `$std.io` and `const { io, http } = $std` type-check off `Std`; the exposed globals use
indexed-access types into the same interface. The type-level story thus mirrors the runtime
one exactly — globals are a projection of `$std` in both. The `base.d.ts` generator should
emit the mirror `declare const`s from the *same* `EXPOSE` list the runtime uses, so a single
source decides "what is global," guarded by the D11 golden. `$std.json` (our `{data,error}`
envelope helper) is distinct from the JS builtin `JSON` — different case, no collision.

### D5 — Keep the `sys` capability id; relocate its surface under `$std`

`$sys` is deleted, but the crypto/env/secrets *behavior* is unchanged, so the `sys` spec
folder persists as the capability id with its requirements re-pathed to `$std.crypto` /
`$std.env` / `$std.secrets`. This avoids inventing a new capability for behavior that only
moved namespaces.

## Risks / Trade-offs

- **[Broad mechanical sweep touches many files]** → The change spans every `js/*.js`,
  `base.d.ts`, the generated golden, docs, and test scripts. Mitigation: it is behavior-
  preserving; the existing unit tests + the regenerated D11 golden + the Python harness are
  the safety net. Land it alone (no new dep) so a failure is unambiguous.
- **[Internal capture defeats the determinism prune]** → If a refactor captures
  `datetime.now`/`crypto.uuid` into a closure, the prune becomes a no-op. Mitigation: the
  D2 corollary is a written rule; add/keep a determinism test asserting unreachability via
  every path after the prune.
- **[Deep-freeze breaks a member that expects mutation]** → A capability object that
  mutated per-call would break under freeze. Mitigation: current built-ins are stateless
  function tables closing over per-request state via FFI; verify none rely on mutating their
  own namespace object before freezing.
- **[Golden test churn masks a real diff]** → Regenerating `container/types.d.ts` produces a
  large diff. Mitigation: review the generated diff against the intended `Std` shape rather
  than rubber-stamping it.
- **[Ergonomic regression for `$sys` users]** → familiar `$sys.crypto` calls now throw.
  Accepted: hard cutover, no users; docs sweep updates every example.

## Migration Plan

1. Rewrite each `js/*.js` IIFE to populate `$std.<name>`; repoint internal cross-refs per
   D2; remove `sys.js`'s `$sys` assembly (build `$std.crypto/env/secrets`).
2. Add the `$std` bootstrap + `EXPOSE` projection + freeze/lock epilogue in the engine's
   injection sequence (D3); update `determinism.js` to prune `$std.*` paths.
3. Rewrite `base.d.ts` (D4); regenerate `container/types.d.ts`.
4. Sweep `docs/*.md`, `README.md`, `openspec/specs/*`, and `tests/scripts/*.js` for bare-
   global / `$sys` references.
5. Run the full gate (Docker: `cargo test`, `cargo clippy`, `cargo fmt --check`, the Python
   harness). No rollback strategy needed beyond reverting the branch — no data, no wire
   change, no dependency.

## Open Questions

- **Exposure-list location** — encode `EXPOSE` as a JS constant in an injection epilogue, or
  own it in Rust as injection policy and drive both projection and `.d.ts` generation from
  there? (Leaning JS epilogue for simplicity, with the generator reading the same constant.)
- **Lock strength for globals** — non-writable is enough to stop reassignment; also make the
  bindings non-configurable? (Leaning yes, to match the frozen `$std`.)

## Build-vs-Adopt Gate

This change is a dependency-free surface refactor; the gate's job here is to confirm we are
not hand-rolling anything a mature tool should own. It stands entirely on already-adopted
foundations. No new external component is adopted or built.

### Decision: Namespace projection + `EXPOSE` mechanism — Build (glue)

- **Status**: approved
- **Why**: Engine-specific injection glue (~10 lines mirroring `$std` members onto
  `globalThis` as identity-equal references); no third-party component exists for "project a
  QuickJS namespace," and it reimplements nothing.
- **Considered**: adopting a module/loader library — rejected, none map to in-context QuickJS
  global injection.
- **Isolation**: the `EXPOSE` list + projection epilogue in the engine injection sequence.

### Decision: Surface immutability — Rent (runtime builtins)

- **Status**: approved
- **Why**: `Object.freeze` + `Object.defineProperty` are the language-runtime primitives;
  nothing is hand-rolled for immutability.
- **Considered**: a custom write-guard/Proxy — rejected (Proxy is removed pre-handler, and
  builtins are simpler and stronger).
- **Isolation**: the freeze/lock epilogue, sequenced after the determinism prune.

### Decision: Determinism-prune invariant under `$std` — Extend (existing sandbox discipline)

- **Status**: approved
- **Why**: Re-paths the existing prune (`delete $std.datetime.now` / `$std.crypto.uuid`) and
  reuses the adopted rquickjs sandbox + the established D9 ambient-authority discipline; adds
  only the test-guarded pure-only-exposure invariant (D2), not a new security primitive.
- **Considered**: a fresh capability-gating layer — rejected (would duplicate proven logic).
- **Isolation**: `determinism.js` + a determinism test asserting unreachability via every path.

### Decision: Single-source typing — Rent/Extend (TypeScript, already in use)

- **Status**: approved
- **Why**: TypeScript `checkJs` + the existing `base.d.ts` → `container/types.d.ts` generator
  + the D11 golden already own this; we extend the authored `.d.ts`, adopt no new tooling.
- **Considered**: a bespoke type-gen step — rejected (the golden-guarded generator suffices).
- **Isolation**: `base.d.ts` (the `Std` interface + derived mirror declares) + D11 golden.

### Deferred: Templating engine — decided in the follow-on `template` change

- **Status**: deferred (out of scope here)
- **Note**: The one genuine adopt call — **minijinja** vs. hand-rolled templating for
  `$std.template` — is gated in the follow-on template change (it introduces the only new
  dependency). Captured intent for that change: expose **explicit escaping modes**
  `$std.template.html(...)` (HTML auto-escape) and `$std.template.text(...)` (literal, no
  escape) rather than one mode with an ambiguous default — the author states which output
  they mean.
