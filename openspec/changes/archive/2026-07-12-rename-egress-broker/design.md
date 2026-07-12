## Context

The box reaches driver-backed egress through a replaceable peer: this repo owns the wire contract
(`runlet-wire`) and treats `fabricd` as *one* reference implementation — "anything that speaks the
protocol can stand in for it." Despite that, the box's own operator config keys (`fabricd_socket`,
`fabricd_quic`), a config struct (`FabricdQuic`), and the transport types (`SidecarEgress`,
`SidecarTransport`, module `sidecar.rs`) name the seam after the reference product or after a
deployment topology. That is a branding/topology leak into an abstraction meant to be
implementation-agnostic. Two additional facts sharpen the case:

- **"Sidecar" is inaccurate.** The QUIC transport is explicitly a broker on another host (a shared
  cluster service), not a co-located sidecar. The topological word already misdescribes half the
  transports; a role word ("broker") is true for UDS, sidecar, and remote QUIC alike.
- **Greenfield.** The config keys carry no `#[serde(alias)]`; there are no external operators to
  keep compatible. We can rename outright with no deprecation shim.

Scope is confined to the `runlet` binary crate's own surface plus prose. `runlet-core` and
`runlet-wire` code, the wire protocol bytes/structs, and the sibling `fabricd` product are not
touched by the rename.

## Goals / Non-Goals

**Goals:**
- Name the box-owned egress seam by its **role** ("broker") in config keys, Rust types, the module
  file, and user-facing prose.
- Keep the WHAT layer (specs) brand-neutral for the seam the box owns.
- Zero behavioral change: same transports, same fail-closed semantics, same wire bytes.

**Non-Goals:**
- Renaming the sibling `fabricd` repo, binary, its config file (`fabricd.json` / `FABRICD_CONFIG` /
  `FABRICD_SOCKET`), or its own daemon-scoped metric names (`fabricd_db_breaker_trips_total`, …).
  Those name the external product legitimately and stay.
- Changing the wire protocol: `runlet-wire`'s `WireInit`/`WireCall`/… bytes and struct fields do not
  change, so no cross-repo coordination with `fabricd` is required.
- Renaming the correctly-named seam vocabulary: the `Egress` trait, egress mux, `EgressError`,
  `MeteredEgress`, and the role-neutral `SessionConn` / `SessionError` / `connect_session`.
- Adding backward-compatibility aliases for the old config keys (greenfield — see Decision 2).

## Decisions

### Decision 1 — Role word: `broker` (not `egress`, not `sidecar`)

The seam is renamed to **broker**. Alternatives considered:
- **`egress`** (`egress_socket`, `EgressBackend`): rejected because `egress` is already this repo's
  *seam/direction* vocabulary — the `Egress` trait, egress mux, `EgressError`, `MeteredEgress`,
  `EGRESS_UNAVAILABLE`. Reusing it for the *external party behind* the seam collapses two distinct
  layers into one word (`EgressBackend` next to `MeteredEgress` next to the `Egress` trait loses the
  ability to say which layer is meant).
- **`sidecar`** (status quo): rejected — names a deployment topology, and is factually wrong for the
  remote QUIC transport.
- **`broker`** (chosen): a role noun for the party that resolves logical names → kind/endpoint/creds
  and performs the I/O. True across UDS, sidecar, and remote QUIC. Distinct from the `Egress` seam
  layer, giving a clean two-layer story: **Egress** = the in-process port/direction; **Broker** = the
  concrete remote peer behind it (reference impl: `fabricd`).

Concrete renames: `fabricd_socket→broker_socket`, `fabricd_quic→broker_quic`, `FabricdQuic→BrokerQuic`,
`SidecarEgress→BrokerEgress`, `SidecarTransport→BrokerTransport`, `sidecar.rs→broker.rs`.

### Decision 2 — No compatibility aliases; hard rename

The old JSON keys are removed outright (no `#[serde(alias = "fabricd_socket")]`). Rationale: greenfield,
no external operators. Trade-off surfaced under Risks: the top-level `Config` uses `#[serde(default)]`
without `deny_unknown_fields`, so a stale key is *silently ignored* rather than rejected.

### Decision 3 — Scope line: rename the box-owned seam, keep the external product's brand

The word `fabricd` is removed only where it names *this box's own* seam (config keys, types, module,
and seam prose). It is kept where it names the external product as a distinct thing: the sibling repo
checkout (`../fabricd`), the binary/service/image, its config file and `FABRICD_*` env vars, and its
own daemon metrics. In reworded prose, the pattern is "the egress **broker** (reference
implementation: `fabricd`)" — the role first, the product named as one implementation of it.

### Decision 4 — Spec terminology: reword the box-owned seam, leave the daemon's own semantics

In the WHAT layer, only the box-owned seam is reworded to "broker": the session-handshake peer
(`tenant-egress` "Tenant identity carried on the egress session") and the configured transport
described topologically (`tenant-egress` "Fail-closed…", which named a "`fabricd` sidecar"), plus the
generic fallback-egress example in `capability-registry`. Requirements that describe the *reference
broker daemon's* internal resolution/privilege semantics (e.g. `tenant-egress` "Tenant-scoped resource
resolution in fabricd", "Multitenant path forbids the privilege opt-out") keep their wording: those
are proven by `fabricd`'s own conformance suite in its repo, and rewording their titles would be
RENAMED churn for no behavioral gain. `observability` / `tenant-metering` prose naming the daemon's
own drained-egress metrics is likewise kept (Decision 3). These are terminology deltas — no scenario
changes behavior.

### Decision 5 — `runlet-core` / `runlet-wire` are prose-only

Those crates carry no branded *identifiers* fronting the broker — only illustrative doc-comments
("(a `fabricd` sidecar)" as an example wired `Egress`). Sweeping that prose to "broker" is optional
polish that keeps the abstraction clean; it changes no code, types, or the proposal's "core/wire code
untouched" guarantee. It is grouped as a low-priority prose task.

## Build-vs-Adopt Gate

This change introduces no new capability, dependency, or hand-rolled
correctness/security/reliability logic — it is a rename plus terminology reword. The
`Rent > Adopt > Extend > Fork > Build` hierarchy is therefore inapplicable to the change body: there
is nothing being built that a mature tool would otherwise provide. The mechanical rename moves
already-lint-clean code and is executed via explicit edits + a grep verification pass (task 8.1); no
automated-refactor tool is adopted because `cargo fix` is known to mangle re-exports in this repo.

One tooling-flavored fork was surfaced and resolved:

### Decision: Stale-config-key guard — Build nothing (leave out of scope)

- **Status**: approved
- **Why**: the hard key rename makes a stale `fabricd_socket`/`fabricd_quic` silently ignored
  (top-level `Config` is `#[serde(default)]` without `deny_unknown_fields`); the silent-drop risk is
  greenfield-only, so this change stays a pure, behavior-preserving rename.
- **Considered**: Adopt serde's built-in `#[serde(deny_unknown_fields)]` on `Config` to turn a
  stale/typo'd key into a loud boot failure — declined here because it rejects *all* unknown keys
  (a behavioral change beyond the rename); it can be a separate hardening change if ever wanted.
- **Isolation**: config deserialization in `crates/runlet/src/config/mod.rs`.

## Risks / Trade-offs

- **Stale config key silently ignored** → A `config.json` still using `fabricd_socket`/`fabricd_quic`
  after the rename is *silently dropped* (top-level `Config` is `#[serde(default)]` without
  `deny_unknown_fields`), so the box boots with no broker wired and the failure only surfaces later as
  a runtime `503 EGRESS_UNAVAILABLE`. Mitigation: greenfield (no real stale configs); update the two
  stress scripts and all example/docs configs in the same change; optionally consider
  `deny_unknown_fields` on `Config` as a separate hardening change (out of scope here).
- **Doc/prose churn is large** → Many files reference the brand; a partial sweep leaves the
  abstraction half-branded. Mitigation: the tasks group the sweep by file area with an explicit
  keep/reword classification (from the scoping inventory) so nothing is missed and no legitimate
  external-product reference is over-corrected.
- **Lint gauntlet on the renamed module** → `broker.rs` inherits the strict clippy contract. Mitigation:
  a pure rename moves existing (already-clean) code; run `task clippy` after, per the repo rule that
  `cargo build` does not enforce lints.
