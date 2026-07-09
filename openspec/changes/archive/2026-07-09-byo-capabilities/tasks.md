# Tasks — byo-capabilities

> Design settled in design.md: **D1** three in-engine built-ins (`http`/`s3`/`io`), no shipped
> driver-cap defs; **D2** Model 1 broker for `io` (box holds nothing remote); **D3** flatten
> `config.io`/`WireInit` — "kind" is operator-side; **D4** drop `mongo`; **D5** demote broker to an
> optional reference image; **D6** `http` targeted local allowlist; **D7** uds/quic = broker link,
> box-direct-local = an http resolution mode; **D8** box-direct-local egress allowed via **global
> config**, loopback-only; **D9** one `{action, payload}` envelope for broker and box-direct, many
> local services. Build-vs-adopt (approved): **Extend** the in-tree `http` SSRF guard for local-target
> validation; **reuse** the in-tree `reqwest` client for the box-direct POST. This change is mostly
> subtraction; the reference broker (sibling repo) is coordinated, not implemented here.

## 1. Wire contract — flatten to logical names (`crates/runlet-wire`)

- [x] 1.1 Replace the six per-kind `Option<String>` fields (`db`/`mongo`/`mail`/`redis`/`amq`/`auth`) on `WireInit` (`wire.rs`) with a single `resources: Vec<String>` (keep `timeout_ms`, `tenant`, `token`)
- [x] 1.2 Update the manual `Debug` impl to print `resources` (drop the six fields; keep `token` redaction)
- [x] 1.3 Update in-crate consumers + framing round-trip tests for the new shape
- [x] 1.4 Document that this is a **BREAKING cross-repo wire change** (coordinated with the reference broker — §9); `cargo build -p runlet-wire` clean

## 2. Box config + request path — flatten `config.io` (`crates/runlet`)

- [x] 2.1 Replace `RequestIo`'s six per-kind `Vec<String>` fields (`handler.rs`) with a flat `Vec<String>` allowlist (`config.io: ["orders","cache"]`); rewrite `any()` and `enabled_names()` accordingly (the allowlist is the enabled set — no kind→wrapper mapping)
- [x] 2.2 Rewrite `wire_init` (`handler.rs`) to build `WireInit { resources, timeout_ms, tenant, token }` (drop the per-kind `first().cloned()` logic)
- [x] 2.3 Pass the flat allowlist to the engine as `CapabilitySet.io`; `io` is injected globally under `Profile::Full`, and `io.call(name, …)` is gated by the per-request allowlist (`RESOURCE_NOT_FOUND` in the core mux)
- [x] 2.4 `cargo build -p runlet` + `task clippy` clean

## 3. Remove shipped driver capabilities (delete `crates/runlet-caps`)

- [x] 3.1 Delete `crates/runlet-caps` entirely (`js/{db,mongo,mail,redis,amq,auth}.js` + `.d.ts`, the `CapabilityDef`s, `actions::*`, the D10 fixture, the D11 golden test)
- [x] 3.2 Remove `runlet-caps` from the workspace `Cargo.toml` members
- [x] 3.3 `runlet` bin (`main.rs`): stop composing `runlet_caps::preset()`, drop the `runlet-caps` dependency; the builder composes **zero** defs (`http`/`s3`/`io` are in-engine, not defs)
- [x] 3.4 Relocate the golden `types.d.ts` drift test to `runlet-core` (or `runlet`) covering only `http` + `s3` + `io` + `$`/`Decimal`; regenerate `container/types.d.ts` (no `Db`/`Mongo`/… interfaces)
- [x] 3.5 Confirm `runlet-core` is untouched by the deletion (dependency points caps→core, never the reverse); `cargo build` clean

## 4. Drop `mongo` — this repo's surface (`docs`, tests, golden)

- [x] 4.1 Confirm the `mongo` def/js/`.d.ts`/action tokens are gone with §3.1; no `Mongo*` interface remains in `container/types.d.ts`
- [x] 4.2 Delete `docs/12-mongo.md`
- [x] 4.3 Remove the `mongo` section from `tests/test_simple.py`
- [x] 4.4 Note: the mongo **driver** + `mongocrypt` removal is sibling-repo (`fabric-backends`) — tracked in §9, not here

## 5. `io` box-direct-local resolution — D8/D9 (`crates/runlet`)

- [x] 5.1 Add a **global** local-resource map to the box config (`config.rs`): logical name → `{ url }` loopback endpoint (operator-only; never per-request, never script-influenced)
- [x] 5.2 Boot guard: validate each box-direct binding resolves **loopback/private only** (via the extended `http` guard, §6); refuse boot on a non-loopback/remote target (fail-closed; a remote name must go through a broker)
- [x] 5.3 Implement box-direct resolution behind the `io` mux: name in the global local map ⇒ POST the **identical `{action, payload}` envelope** to the endpoint via the **shared `reqwest` client**, bounded by the execution deadline, mapping failure to a `__runlet`-tagged error; else fall through to `SidecarEgress` (broker)
- [x] 5.4 Resolution order: request `config.io` allowlist → global local map (box-direct) → broker; meter `meta.io.<name>` on the box-direct path too
- [x] 5.5 `cargo build -p runlet` + `task clippy` clean

## 6. `http` targeted local allowlist — D6 (`crates/runlet-core`)

- [x] 6.1 Extend the `http` SSRF guard (`http.rs`): an **explicitly allowlisted** `host:port` (in `http.allowed_hosts`) bypasses the private-IP block for that exact host, while all other guards (allowlist match, redirect re-validation) still apply
- [x] 6.2 Keep `debug` as the separate **blanket** private-IP relax (development-only); the allowlist is the precise, production-safe path
- [x] 6.3 Expose a `loopback/private-only` validation helper reused by the §5.2 box-direct boot guard
- [x] 6.4 Unit tests: named local target reachable with `debug` off; un-named local target still blocked; `debug` still relaxes globally; `task clippy` clean

## 7. Docs

- [x] 7.1 Collapse the six beginner guides (`03`,`04`,`07`,`08`,`10`,`11`,`12`) into one **"Build your own capability over `io.call`"** guide: the three extension paths (raw `http`, in-process Rust cap, `io`→broker) + the box-direct-local option (global config)
- [x] 7.2 Update `docs/design/resource-egress.md`: three built-ins, the extension spectrum, Model 1 + box-direct-local (D8/D9), the one `io.call` envelope; **keep** the least-privilege section carried from `resource-privilege-guard`
- [x] 7.3 Document config: flat `config.io` allowlist, the global local-resource map, `http.allowed_hosts` local bypass; sync `README.md` (reference version)
- [x] 7.4 Confirm `container/types.d.ts` is `http`/`s3`/`io` + `$`/`Decimal` only

## 8. Tests

- [x] 8.1 Rework `tests/test_simple.py`: driver sections that called shipped wrappers (`db.query`, `redis.*`, …) move to `io.call("<name>", "<action>", payload)` (serviced by the reference broker) or are removed
- [x] 8.2 Add a **box-direct-local** test: stand a tiny loopback echo service, bind it in the global config, call `io.call`, assert the same-envelope round-trip + `meta.io.<name>` + the loopback-only boot guard rejects a remote binding
- [x] 8.3 Golden `types.d.ts` drift test (relocated in §3.4) passes with the three-primitive surface

## 9. Downstream coordination — sibling reference broker (OUT OF SCOPE here, do not implement)

- [ ] 9.1 Track: the reference broker (`fabric`/`fabricd`) consumes `WireInit.resources` (flat names), resolves name→kind→endpoint→creds, **drops the mongo driver + `mongocrypt`**, and ships as an optional `docker run` image — coordinated PR in the sibling repo, no code here
- [ ] 9.2 Track: the box↔broker wire break (§1) lands in lockstep with the broker change

## 10. Wrap-up

- [x] 10.1 Supersede `resource-privilege-guard`: its least-privilege docs fold into `resource-egress.md` (§7.2); archive that change on sync
- [x] 10.2 `task fmt` + `task clippy` clean (strict lint gauntlet); `cargo build` green (build/test via Docker per the env gotcha — WDAC blocks native cargo)
- [x] 10.3 `/opsx:sync` the delta specs — `capability-registry` + `tenant-egress` + `http` modified; `db`/`mongo`/`mail`/`redis`/`amq`/`auth` removed — into `openspec/specs/`, then `/opsx:archive`
