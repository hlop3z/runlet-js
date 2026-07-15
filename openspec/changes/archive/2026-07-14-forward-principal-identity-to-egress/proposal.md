## Why

The signed-contract sub-mode now **verifies and captures** `principal_kind` (`user`/`apikey`/`service`)
and `on_behalf_of` (the human behind an api-key) onto `TrustedIdentity`, but the box **drops both at its
egress trust boundary** — it re-vouches only `tenant` + `actor` to a broker (`WireInit`) and box-direct
target (`x-runlet-*` headers). Two correctness consequences for a downstream consumer (e.g. event-logs):

- **Api-key attribution is lost.** For an `apikey` principal `actor = sub = the key id`, and the human
  (`on_behalf_of`) never reaches the wire — so a consumer attributes the action to the key and cannot name
  the person behind it, contrary to nexus's "attribute to both the key and the human."
- **A consumer can't branch on principal kind.** Without `principal_kind`, a downstream service can't
  distinguish a `service` writer (nexus's legitimate event-writer) from a human, which its ingest door
  wants to gate on.

The data is one function-call away from the wire; this change threads it through. Forwarding was the
explicit deferred open-question in the prior change's `design.md`. The premise the current specs cite for
*not* forwarding kind — "the box does not verify signed assertions" — is **no longer true**.

## What Changes

- **`runlet-wire` (BREAKING, greenfield — no consumers yet):** add `principal_kind: Option<String>` and
  `on_behalf_of: Option<String>` to `WireInit`, each `skip_serializing_if = "Option::is_none"` so the
  plain / single-tenant handshake stays byte-minimal. This is a **cross-repo contract change**; it is
  additive (an unchanged broker ignores the new fields), and `fabricd` consuming them is out of scope.
- **Box-direct:** two new out-of-band headers `x-runlet-principal-kind` and `x-runlet-on-behalf-of`,
  attached only when present, mirroring the existing `x-runlet-tenant`/`x-runlet-actor` pattern; the
  `{action, payload}` **body stays identical** (D9). `BoxEgress` gains the two fields.
- **Threading:** source both from `TrustedIdentity` at the two egress build sites (the `wire_init(...)`
  call and the `BoxEgress` construction); extend the `wire_init` helper signature. Emitted **only when a
  verified contract populated them** — absent on the plain-header path and single-tenant.
- **`actor` is unchanged** — it stays exactly the acting subject (`identity.user`, the key id for an
  api-key); the two new fields ride **alongside** it, so a consumer attributes to the key (`actor`) **and**
  the human (`on_behalf_of`) and branches on `principal_kind`.

## Capabilities

### New Capabilities

(none)

### Modified Capabilities

- `tenant-egress`: the two "Actor identity carried on the egress session" and "Actor identity carried on
  the box-direct local egress path" requirements each currently state principal kind SHALL NOT be
  forwarded *because the box verifies no signed assertions*. Modify both to carry `principal_kind` and
  `on_behalf_of` — as separate `WireInit` fields on the broker path and separate `x-runlet-*` headers on
  the box-direct path — emitted only when a verified contract populated them, with `actor` unchanged and
  the box-direct body still identical.

## Impact

- **Code:** `crates/runlet-wire/src/wire.rs` (`WireInit` two fields), `crates/runlet/src/handler/types.rs`
  (`wire_init` helper signature), `crates/runlet/src/handler/mod.rs` (the `wire_init(...)` call + the
  `BoxEgress` build), `crates/runlet/src/local_io.rs` (two header consts + `BoxEgress` fields + its
  header-set unit tests).
- **Contract:** changes `runlet-wire` — the cross-repo egress contract. Additive/greenfield; `fabricd`
  (sibling repo) can read the new fields later. **Out of scope:** any `fabricd` change.
- **Docs/specs:** `docs/design/multitenant-trust.md` "Identity on the egress paths" section (it currently
  says kind is *not* forwarded — flip it) and the `tenant-egress` spec delta above.
- **Out of scope (follow-ups):** *gating* on `principal_kind` (admit a `service` writer vs a human), and
  event-logs actually **consuming** these (its ingest route/schema).
- Build/test is Docker-only (WDAC); no new crate dependency.
