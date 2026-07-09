# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

# How to develop here

This project drives all work through **OpenSpec**, augmented with an abstraction-layer
discipline and a build-vs-adopt gate. Follow the pipeline — the rules apply themselves.

```
/opsx:explore   Think it through. Decompose (invariants, boundaries, ≥3 strategies). No code.
      ↓
/opsx:propose   Generate proposal.md (WHY/scope) + specs (abstract WHAT) + design.md (HOW).
      ↓
/opsx:decide    Build-vs-adopt gate: per critical concern, Rent>Adopt>Extend>Fork>Build.
      ↓             Records each decision into design.md. Run before implementing.
/opsx:apply     Implement the tasks. Thin entry points; adapters isolate every dependency.
      ↓
/opsx:sync      Fold the change's delta specs into the main specs.
      ↓
/opsx:archive   Close out the completed change.
```

## The two ideas that make this work

1. **Abstraction layers stay separate.** WHAT (`specs/`) is language-agnostic behavior.
   HOW (`design.md`) is structure + tool choices. DO (`tasks.md` + code) is the implementation.
   No layer leaks into another. Core holds behavior; every surface (CLI/GUI/API) is a thin adapter.

2. **Adopt before you build.** For anything correctness-, security-, or reliability-critical,
   prefer a mature tool over hand-writing it. `/opsx:decide` makes that call explicit and records it.

## Where the rules live (don't restate them)

- **`openspec/config.yaml`** — the philosophy, injected once into every artifact by the CLI.
- **`openspec/guidelines.md`** — the full reference: build-vs-adopt hierarchy, maturity rubric, doc taxonomy.

Commands stay thin and point at these, so a change costs few tokens to plan.

## Keep the main thread cheap

For broad or exploratory searches — locating code across many files, surveying naming
conventions, answering a question that spans several files — delegate to the `Explore` or
`general-purpose` subagent and keep only its conclusion. Don't read the file dumps into the
main thread. For a single known file or symbol, just search directly (a subagent would cost
more than it saves).

## Where findings go (promote or discard — never accumulate)

There is **no `/research` folder**. A global, ever-growing notes dump rots and starts
misleading. Every durable finding gets exactly one canonical home; everything else dies in
the scratchpad. When a subagent surfaces something worth keeping, route it:

- **Throwaway exploration** → the session scratchpad. Auto-discarded; never committed.
- **Derivable from code** → leave it in the code. Don't snapshot it.
- **A decision + its why** (build-vs-adopt, tradeoffs) → `design.md` ADR block.
- **Durable behavior contract** → `specs/` (synced to main specs on `/opsx:sync`).
- **In-flight notes for a specific change** → `openspec/changes/<change>/`, archived on `/opsx:archive`.
- **Non-derivable fact about the user/project** → the memory dir; update or delete when wrong.

The discipline is promote-or-discard, not save-more: one home, one lifecycle, one owner.

## What this is

A sandboxed JavaScript execution service in Rust. Clients `POST /execute` a JS
`handler(ctx)` function plus a JSON context; the server runs it in an isolated QuickJS
context and returns `{data, error, meta}`. The single endpoint is the whole product.

**Two repos, one contract.** This repo (`runlet-js`) is the box and is fully self-contained:
it links no vendor driver and holds no credentials. The egress sidecar **`fabricd`** and the
driver bag **`fabric-backends`** live in their own repo (`github.com/hlop3z/fabricd`, expected
as a sibling checkout `../fabricd` for dev/tests) and are **replaceable at any time**: this
repo owns the entire contract (`crates/runlet-wire`), and anything that speaks it can stand in
for `fabricd`. Nothing here depends on the fabricd repo — the dependency points the other way.

**Cargo workspace (three crates + a bench crate):**

- **`runlet-wire`** (`crates/runlet-wire/`) — the shared, driver-free, QuickJS-free egress
  contract, depended on by every other crate (including, cross-repo, the fabricd repo's
  crates): the `Egress` trait + `EgressError`, the error
  taxonomy (`ErrorOwner`/`Fault`/`DynamicFault` + the `__runlet` wire envelope), the per-target
  `CircuitBreaker`, the metric `Collector`, **and** the box↔`fabricd` wire protocol (`wire.rs`:
  `WireInit`/`WireCall`/`WireRequest`/`WireResponse` + length-prefixed framing, the `*Metric`
  types, `BackendMetrics`, `MeteredEgress` — `WireInit` also carries the **trusted tenant id** so
  `fabricd` can scope resolution) plus the one shared constant-time compare (`ct.rs`: `ct_eq`, on
  `subtle`, used by both the box's edge-credential check and `fabricd`'s static-token check) plus
  the **QUIC remote transport** (`quic.rs`:
  pinned-self-signed-cert server/client `quinn` endpoint builders + `ServerTls`, on the shared
  `aws-lc-rs` provider) that the same framing rides for a network `fabricd`. **Changing this
  crate changes the cross-repo contract** — keep it additive/compatible or coordinate a
  matching fabricd change.
- **`runlet-core`** (`crates/runlet-core/`) — the reusable logic host: the QuickJS engine,
  runtime pool, sandbox, resilience, error taxonomy, capabilities, and the callable
  [`LogicHost`] port (`Invocation` → `Outcome`). Knows nothing about HTTP. The public entry
  is `runlet_core::host::LogicHost`. Capabilities are **composed as `CapabilityDef` values** via
  `LogicHost::builder(...)` (the capability registry + egress mux — `capability.rs`), not baked in;
  a deterministic-only consumer builds with `default-features = false` and links nothing. **Links
  no network driver even with `full`** — **no driver-backed capability ships here at all** (the six
  old wrappers `db`/`mongo`/`mail`/`redis`/`amq`/`auth` and the whole `runlet-caps` preset crate were
  removed on the byo-capabilities change; users compose their own via `CapabilityDef`). Core keeps
  exactly **three built-in primitives**: `http` (SSRF-guarded, script-controlled URL) and `s3` (pure
  SigV4 signing) as cargo features, plus **`io`** — the operator-named logical-egress primitive
  (`js/io.js`: `io.call(name, action, payload)`) that routes through the egress mux. The mux enforces
  the sandbox invariants centrally and **fails closed**; `Profile::Deterministic` *removes* ambient
  authority. See `docs/design/composable-core.md`.
- **`runlet`** (`crates/runlet/`) — the binary: the axum HTTP `/execute` front + server config,
  a thin adapter over `LogicHost::run`. **Links no network driver and holds no credentials**
  (it does not depend on `fabric-backends`): when a request names an `io` resource in `config.io`
  (now a **flat allowlist of logical names**), the box resolves it either **box-direct** to a
  co-located loopback endpoint declared in the operator's global `local_resources`
  (`local_io::BoxEgress` — box-direct HTTP POST of the identical `{action, payload}` envelope, no
  broker, no creds; guarded to loopback/private by the `Config::check_local_resources` boot guard),
  else over
  a `fabricd` session on a **local UDS or a remote QUIC link** (`sidecar::SidecarEgress`, transport
  chosen by config — `fabricd_socket` vs `fabricd_quic`), forwarding the logical name for `fabricd` to
  resolve to a kind+endpoint+credentials. On the QUIC path the box pins the
  daemon cert by fingerprint and presents an auth token (`BoxAuth`: static secret or a re-read k8s
  SA-token file). No sidecar configured + a driver request ⇒ `503 EGRESS_UNAVAILABLE`.
  Deterministic/`http`/`s3` requests need no sidecar. Prometheus metric names use the `runlet_*`
  prefix and the internal capability-error wire tag is `__runlet`. **Observability is three signals,
  hybrid transport** (`telemetry.rs`, opt-in `config.telemetry`): metrics stay Prometheus **PULL**
  (`/metrics`), logs are structured **JSON to stdout**, and each `/execute` is an OpenTelemetry span
  **pushed** OTLP/gRPC to a collector (plaintext to a local collector ⇒ no second crypto stack;
  `cargo tree -i ring` still empty). Identity (tenant/user/plan) rides spans/logs as **attributes,
  never metric labels** (cardinality). Tracing continues a W3C `traceparent` from the edge (N6) or
  starts its own root; export is non-blocking + fail-open. Optional **per-tenant events**
  (`events.rs`, opt-in `config.events`): one unified versioned envelope per request to a dedicated
  stdout JSON stream — a `usage` event per executed request (billing dims) + an `audit` event per
  request (`allowed`, or `denied`+reason at each gate), keyed by tenant. Non-blocking + unsampled
  (bounded channel, drop-on-full → `runlet_events_dropped_total`); the `event_id` + schema are the
  seam for a later durable billing outbox. Optional **trusted-identity mode**
  (`config.trusted`, opt-in): behind the nexus edge the box derives tenant/user identity from
  configured trusted headers (`identity.rs`), rejects anonymous/suspended callers, keys Tier 5
  fairness + the bytecode-cache namespace off the trusted tenant id (dropping the caller-asserted
  `X-Partition-Key`), forwards that tenant in `WireInit`, gates member capabilities off
  roles/entitlements (`authz.rs`), and enforces per-tenant plan-gated quota (`quota.rs`). A boot
  guard refuses trusted mode on a non-loopback bind unless network isolation is asserted. See
  `docs/design/multitenant-trust.md`.

**In the sibling repo** (`github.com/hlop3z/fabricd`): **`fabricd`** — now an **optional reference
broker** (demoted from shipped-core on byo-capabilities): the egress sidecar (bin) holding the
operator credential table and **all** the vendor drivers (via its `fabric-backends` crate), hosting a
`BackendSet` per session behind the `runlet-wire` protocol over UDS or QUIC. It resolves the flat
`WireInit.resources` names → kind → endpoint → credentials (the `mongo` driver + `mongocrypt` are
dropped). Needed only for `io` resources that resolve to a broker (a box-direct `local_resources`
binding, `http`, `s3`, and deterministic requests never touch it). Its design docs stay canonical
here: `docs/design/resource-egress.md`, `docs/design/network-fabric.md`.

## Commands

The project uses [Task](https://taskfile.dev) (`Taskfile.yml`). Raw `cargo` equivalents in parens.

- **Build:** `task build` (`cargo build`, whole workspace) · release: `task build-release` (`cargo build --release`)
- **Run:** `task run` (`cargo run -p runlet`) — serves on `http://127.0.0.1:3000`
- **Deterministic-only core build (no built-in primitives):** `cargo build -p runlet-core --no-default-features` (optionally `--features http,s3` to opt the in-engine primitives back in)
- **Format:** `task fmt` / `task fmt-check`
- **Lint:** `task clippy` (`cargo clippy`) — see the lint warning below
- **Unit tests:** `cargo test`
- **Integration tests:** `task test-db-up` (starts Postgres + PgBouncer + CockroachDB + a local httpbin via `docker compose`), then `python tests/test_simple.py`. The db section also runs through PgBouncer (transaction pooling, host `:6432`) to keep the external-pooler path covered (`docs/design/pooled-capabilities.md`). The HTTP `api` tests hit the local `httpbin` service (host `:8095`, env-overridable `HTTPBIN_URL`) — hermetic, no httpbin.org dependency; note go-httpbin echoes headers as **arrays**, and reaching it requires `debug: true` (SSRF private-IP relax). The script-registry section needs the server started by the harness itself (it generates `.test-run/config.json` with `debug: true` + `scripts_dir=tests/scripts`) and self-skips otherwise. The Python harness **starts its own server** with `cargo run`, so don't run one separately. Driver-backed sections (db/mongo/nats/auth) additionally need the **`fabricd` sidecar from its own repo**: set `FABRICD_BIN` to a prebuilt binary, or keep a sibling checkout at `../fabricd` (the harness builds it there); without one, those sections self-skip. It is a custom runner (not pytest) — there is no per-test name filter; edit `main()` in `tests/test_simple.py` to narrow what runs. Each capability section **self-skips** if its backend isn't reachable (it live-probes first), so a partial `docker compose up` only runs what's available.
  - **Auth (`auth`) tests** need an identity provider. `docker compose up -d keycloak zitadel` brings up both. Keycloak (host `:8081`) is fully automatic — the harness mints a token via the `admin-cli` password grant and creates a confidential client for introspection. ZITADEL (host `:8082`) needs its bootstrap service-account PAT: `docker compose exec`-free, it's written to `./.zitadel/zitadel-admin-sa.pat` (a gitignored bind mount) on first start, so run the suite with `ZITADEL_PAT_FILE=./.zitadel/zitadel-admin-sa.pat` (or `ZITADEL_PAT=<token>`). Provider URLs/creds are env-overridable (`KEYCLOAK_ISSUER`, `ZITADEL_ISSUER`, …) for in-network/CI runs. ZITADEL introspection needs an API app, so introspection-with-creds is exercised on Keycloak; ZITADEL covers discovery + userinfo + the throw path.
- **Everything:** `task` (fmt-check + clippy + tests + supply-chain) · `task check` (no supply-chain)
- **Supply chain:** `task supply-chain` (cargo-audit + cargo-deny + cargo-vet; install via `task setup`). cargo-vet is initialized (`supply-chain/`): the dep tree is covered by imported third-party audit sets (Mozilla/Google/Bytecode Alliance/Embark/Zcash/ISRG) with the remainder as `exemptions` — a new/bumped dep that isn't audited or exempted fails `cargo vet`, so re-run it after dependency changes and `cargo vet prune` / add an exemption as needed. **cargo-vet is version-pinned in lockstep between `task setup` (Taskfile) and CI (`ci.yml`), currently 0.10.2** — the `imports.lock` format changes across versions (0.10.2 writes `trusted-publisher` entries that 0.10.0, the last prebuilt GitHub release, cannot parse), so bump both places together or CI fails on a lock file the local tool wrote.
- **Docker:** `task docker-build` / `task docker-run`

### CRITICAL: `cargo build` does not run the clippy lints

The strict lint contract lives in `[lints.clippy]` in `Cargo.toml`, and **`cargo build` / `cargo test` do NOT enforce it** — only `cargo clippy` does. Always run `task clippy` before considering a change done; a clean `cargo build` is not enough. A hard clippy error can also short-circuit later lint passes, so fixing one error often surfaces more on the next run — re-run until truly clean.

## Build environment gotchas

- **`aws-lc-sys` (the rustls crypto backend) needs a C toolchain.** Plain Windows hosts without MSVC build tools + NASM cannot build this project natively. **Build and test via Docker** (the `Dockerfile` cross-compiles to musl/Alpine, which handles `aws-lc-sys` with just `musl-dev`). The release `Dockerfile` is multi-stage (cargo-chef + fat-LTO + `strip` → distroless/static, ~18 MB). For a fast functional test, a debug `cargo build` on `rust:1.92-alpine` is much quicker than the release LTO build and still enforces the rustc lints.
- **rustls provider is `aws-lc-rs`, not `ring`.** When adding a TLS-using dependency, configure it with `rustls-no-provider` + the dep's `aws-lc-rs` feature so it reuses the existing provider. Pulling `ring` (or default `native-tls`/OpenSSL) links a second crypto stack and bloats the binary.

## Architecture (request lifecycle)

Module paths below are under `crates/runlet-core/src/` unless prefixed with `runlet/`.

`runlet/src/main.rs` wires an axum router (`/execute`, `/health`, `/metrics`) with an
`AppState` holding a `LogicHost` (+ the bulkhead/partition/breaker/metrics it doesn't own).

1. **`runlet/src/handler.rs`** — `execute()` resolves the script source — exactly one of inline `script` or registered `key` (looked up in `registry.rs`, a startup-loaded map of `scripts_dir/**/*.js`; see `docs/design/script-registry.md`) — validates input sizes, admits via the bulkhead, then `tokio::task::spawn_blocking` (QuickJS is synchronous/single-threaded and must run off the async runtime) builds an `Invocation` and calls `host.run`.
2. **`host.rs`** — `LogicHost::run(Invocation) -> Outcome` is the callable port (no HTTP assumption): resolves the `CodeRef`, acquires a pooled runtime, builds `ExecParams`, calls `engine::run`, releases the runtime, and maps `ExecResult` → `Outcome { result, effects, metrics }`. The HTTP front and any non-HTTP scheduler both go through this.
3. **`pool.rs`** — `JsPool` is a fixed pool of pre-warmed `Runtime`s (sized to CPU cores). `acquire()`/`release()`; a fresh `Context` is created per request (cheap) so global scope never leaks between requests. `release()` runs GC.
4. **`engine.rs`** — `run()` is the orchestrator: sets a timeout interrupt handler, makes a `Context`, injects the `json()` bridge → `$`/Decimal → `$sys` → `emit` → `log` → capabilities (`inject_apis`, gated by `Profile`) → evals the user script → removes `eval`/`Proxy` (+ determinism sanitizer under `Profile::Deterministic`) → calls `handler(ctx)` → extracts the JSON result. Returns `ExecResult { outcome, effects, logs, *_metrics }`.
5. **`runlet/src/handler.rs`** — parses the JS `{data, error}` envelope (zero-copy via `RawValue`), attaches the Rust-computed `meta`, and surfaces the drained `Outcome.effects` as a top-level `effects: [{kind, value}]` list (omitted when empty), returning `{data, error, meta, effects?}` on both the success and error paths.

**Batch (`POST /batch`, `handler.rs`):** a thin fan-out over the *same* per-request path — `batch()` resolves auth + identity **once** (batch-level), then runs each item through `run_batch_item` (the shared `execute_blocking` core plus the identical per-item gates `/execute` uses: script resolve → size validate → per-item authz → per-item quota → session → admit → execute), collecting order-preserving `{data, error, meta, id?}` envelopes via a `tokio::JoinSet`. A batch bounds its own concurrency to its partition's fair share (items queue, unlike `/execute`'s fast-fail) and cannot monopolize the pool. Security gates are evaluated **per item, never once for the batch** (the D5 GraphQL-batch-attack guard); an admitted batch is HTTP 200 with per-item errors, batch-level rejections (auth/caps) are non-200. Caps: `config.batch.{max_items,max_input_bytes,max_response_bytes}` (the last truncates an oversize item to a `BATCH_RESPONSE_TRUNCATED` envelope). `LogicHost`/`runlet-core` are untouched.

**`Profile` + `emit` (the effects channel — "logic proposes, the host disposes"):** `Profile::Full` is the jsbox capability set + `emit`; `Profile::Deterministic` injects **no** I/O capabilities and neutralizes nondeterminism (`Math.random`/`Date`/`$sys` time+random — see `js/determinism.js`). `emit(kind, value)` appends a tagged `{kind, value}` entry (required non-empty `kind` ≤ `max_emit_kind_len`; opaque verbatim `value`) to a per-invocation buffer surfaced in call order as `Outcome.effects`, captured on **both** the success and error paths. The core routes/meters by `kind` but never interprets it (the authorization/durable-disposition seam). The HTTP front surfaces it as the response `effects` list. See `docs/design/effects-channel.md`.

**`log.*` (the diagnostic channel — "diagnostics, not billing; lossy"):** a structured, leveled logger (`log.trace/debug/info/warn/error`, Serilog-style `"msg {name}"` templates + named properties + `log.with(ctx)` bound context) injected under **both** profiles beside `emit`. Each accepted call captures a `LogEntry {level, template, properties, message, seq, offset_us?}` into a per-invocation buffer drained into `Outcome.logs` on **both** paths (capture-on-failure). The level floor (`EngineConfig.log_level`, default `info`) is checked in `js/log.js` **before** serializing (D6); bounds are the D7 triad (`max_log_entries`=256, `max_log_entry_bytes`=256 KB, `max_log_total_bytes`=1 MB — total binds first, oversize → `{"truncated":true}`). Logs are **outside** the reproducible `data`/`effects` contract: `seq` always, `offset_us` only under `Profile::Full`. Routing is edge-side policy (`runlet`), never the script's: an always-on per-tenant **stream** sink (`events.rs` `EventBody::Log` on its **own** bounded channel + `runlet_log_events_dropped_total`, isolated from `usage`/`audit` — D4) plus a gateway-gated **response mirror** (`logs: [...]`, trusted `capture` header only, capture-on-failure). Trusted `mode` (live vs test/playground) + `capture` + a per-request floor override arrive as trusted headers (`identity.rs`); a **test** run is response-mirror-only and never streams (OQ1). See `docs/design/diagnostic-logging.md`.

`config.rs` (core) owns `EngineConfig` (engine sandbox limits, human-readable byte sizes like `"32mb"`: memory, stack, wall-clock timeout, max script/context bytes, `max_ops`). The server `Config` (bind address, `/execute` auth token, `scripts_dir`/`modules_dir`) lives in `runlet/src/config.rs` and embeds `EngineConfig`.

### The capability model — three built-ins, bring your own for the rest (post byo-capabilities)

The box ships **exactly three built-in primitives** and lets you add everything else; there is no
shipped driver preset anymore (see `docs/design/composable-core.md` and the user-facing
`docs/03-capabilities.md`).

**The three built-ins (in `runlet-core`):**
- **`http`** (`http.rs`, `http` feature) — SSRF-guarded because the URL is **script-controlled**.
- **`s3`** (`s3.rs`, `s3` feature) — pure SigV4 request signing (no network of its own).
- **`io`** (`js/io.js`) — the operator-named logical-egress primitive: `io.call(name, action, payload)`,
  the one **string-in / string-out JSON FFI** everything driver-shaped rides. `config.io` is a **flat
  allowlist of logical names** (nicknames); a name not in the list fails `RESOURCE_NOT_FOUND` before
  connect. The box is **kind-blind** — it never knows whether a name is Postgres or Redis.

`http`/`s3` are the enumerated **mux-bypass surface** (`engine.rs::inject_apis`, gated by their cargo
features, metered via `sandbox.rs` `Collector<T>` → `ExecMetrics` → `meta.io.http`/`meta.io.s3`);
adding another in-engine primitive is a core change and must be justified against composing over `io`.

**Three ways to reach everything else (the extension spectrum, less→more isolation):**
- **(a) `http` to a local service** — allowlisted `localhost:8000`; no wire protocol, you run the service.
- **(b) your own Rust `CapabilityDef`** — compile your driver + creds into *your own* binary via the
  library: [`CapabilityDef::new(name, js, d.ts, Trust)`](crates/runlet-core/src/capability.rs) composed
  via `LogicHost::builder(...).capability(def)`. The JS wrapper routes every op through
  `io.call('<name>', '<action>', payload)`; its `.d.ts` fragment travels with it (prefix interface
  names to keep the shared TS namespace flat). Mandatory `Trust`: `OperatorSupplied` (targets from
  operator config, no SSRF) or `ScriptControlled(SsrfPolicy)` (targets from script input, framework
  applies the SSRF guard pre-connect). **`runlet-core` is not touched.**
- **(c) `io` → a broker** (`fabricd`) that holds **all** the credentials — the multitenant path, box
  holds nothing. A broker-free variant resolves a name **box-direct** to a co-located loopback service
  declared in the global `local_resources` map (`local_io::BoxEgress`, D8/D9) — the on-the-wire
  `{action, payload}` envelope is identical, so a service moves between box-direct and broker with
  zero script change.

Whichever path, an `io` call is **injected only** when the request names it in `config.io` **and** the
`Profile` allows I/O, and is **metered + limited + deadline-propagated + fail-closed centrally** by the
mux — an author cannot opt out. Metrics surface under `meta.io.<name>`. The D11 golden test
(`types_dts_is_up_to_date`, now in `runlet-core`) keeps `container/types.d.ts` in sync.

### Trust: script-controlled vs operator-supplied

- **`http` (`http.rs`) is SSRF-guarded** because the URL is **script-controlled**: host allowlist
  (`allowed_hosts`), private/internal IP blocking, redirect re-validation.
- **`io` names are operator-supplied** — the script only ever sees a nickname; the real host/creds live
  in the broker (or the box-direct `local_resources` binding, which the boot guard pins to
  loopback/private). No SSRF surface. When you build a `CapabilityDef` (path b), pick `Trust` the same
  way: `OperatorSupplied` for config-named targets, `ScriptControlled(SsrfPolicy)` for script-named ones.

### Resilience / deadlines

The execution wall-clock deadline is **propagated to egress by the mux**, so a hung `io` call is bounded
even when a server-side timeout is lost through a transaction-mode pooler. The driver-side detail (async
`block_on(timeout(...))` on the blocking thread, pooler behavior) now lives in `fabricd`'s
`fabric-backends`, not this repo; `docs/design/resilience.md` + `docs/design/pooled-capabilities.md`
stay the canonical write-ups.

### Numbers / decimals

A driver behind an `io` name returns values that don't fit a JS number exactly as **strings** (e.g.
Postgres `BIGINT`/`NUMERIC`) — that mapping is now the broker's (`fabric-backends`) concern, over the
wire. In-script, the `$`/`Decimal` global (`decimal.rs`, backed by `rust_decimal`) gives exact math. JS
has no operator overloading, so it's method-based (`.add().mul().round()`), not `+ - * /`;
`__decimal(op, a, b)` does the work and stays panic-free via `Decimal::checked_*`.

## Conventions

- **Lint gauntlet:** `[workspace.lints]` in the root `Cargo.toml` (inherited by both crates via `[lints] workspace = true`) forbids `unsafe`, denies `clippy::{all,pedantic,nursery,cargo}` plus many restriction lints (no `unwrap`/`expect`/`panic`, no bare arithmetic — use `checked_*`/`saturating_*`, no `as` casts, `missing_docs_in_private_items`, no `#[allow]` — use `#[expect(..., reason="...")]`). Mirror an existing module (`http.rs` / `s3.rs` are the canonical in-engine templates; `local_io.rs` for an egress adapter) and keep functions small (cognitive-complexity and line thresholds in `clippy.toml`).
- **Beginner docs** live in `docs/` (a kid-friendly guide to each capability). Keep them in sync with API changes; `README.md` is the reference version.
- **Capability method names are `snake_case` — always.** Every method on a built-in or user-composed capability global (`http`/`s3`, and any `CapabilityDef` you add) uses `snake_case`, e.g. `s3.upload_url`. **Do not** copy the underlying library's casing — a MongoDB-style `findOne`/`insertMany` becomes `find_one`/`insert_many`. The `action` token passed to `io.call(name, action, payload)` (and any internal `__<cap>` FFI) must use the same `snake_case` name as the JS method, kept in sync between the JS wrapper and the resolver's dispatch. (Exception: the value-util globals `$`/`Decimal` use JS-idiomatic camelCase fluent methods like `toCents`/`toString` — see the next bullet. The snake_case rule is for I/O capabilities only.)
- **Util API surface — one canonical, IntelliSense-discoverable form.** New helpers on a value-util global like `$`/`Decimal` (and any future util we add) are exposed as **chainable instance methods only** (e.g. `$(x).toCents()`, camelCase to match JS natives like `toString`), never duplicated as static shortcuts on the factory (no `$.toCents(x)`). Every public method must be declared in `container/types.d.ts` so editor autocomplete (the bundled `tsconfig.json` runs `checkJs`) is the single source of truth for what's callable. One way to do a thing, and it shows up in IntelliSense.
- **Releases** are CI-only (`.github/workflows/release.yml`, manual `workflow_dispatch` with a version bump) — it bumps `Cargo.toml`, tags, and pushes the image to GHCR. Don't hand-edit versions for a release.
