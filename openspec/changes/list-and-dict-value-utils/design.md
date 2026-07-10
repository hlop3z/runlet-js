## Context

The sandbox ships value-utils for money (`$`/`money`), exact numbers (`Decimal`), dates
(`datetime`), and strings (`text`) — each a chainable, immutable, snake_case global. Collections
are the last gap: an array of records and a single record object. The target audience is ERP /
e-commerce **self-serve authors who are not programmers**, so the design constraint that dominates
everything below is: **no callbacks, no arrow functions** anywhere in the surface. Every verb is
driven by field-name strings and match-by-example objects — the vocabulary these authors already
half-know from spreadsheets, SQL, and Shopify.

The structural template is `text` (`crates/runlet-core/src/js/text.js` +
`crates/runlet-core/src/text.rs`): a thin immutable JS wrapper plus a ~30-line Rust injector that
runs after the engine sets up globals, needing no `__sys` bridge and no Rust domain math. `list`/
`dict` add one wrinkle over `text`: their aggregates reuse the already-injected `Decimal` global
(itself over the `__decimal` FFI), so they must be injected **after** `decimal`.

Constraints that shape the design:
- The repo's lint gauntlet and the "chainable instance-method-only, snake_case, no static
  shortcuts" convention (CLAUDE.md; overrides camelCase for this business-scripting surface).
- **The engine removes `Proxy`** before any handler runs, so a wrapper cannot transparently support
  `wrapped[i]` — indexing is `.get(i)`/`.at(i)`; iteration is `Symbol.iterator` (not a Proxy trap).
- Injected under **both** profiles, so the surface must contain **no** clock/randomness — this bans
  `shuffle`/`sample` outright (they cannot be "removed" the way `datetime.now` is; they must simply
  not exist).
- The D11 golden test (`types_dts_is_up_to_date`) requires `base.d.ts` → `container/types.d.ts` to
  stay in sync for any surface change.
- Build/test are Docker-only (aws-lc-sys needs a C toolchain; native cargo is WDAC-blocked here).

## Goals / Non-Goals

**Goals:**
- Two pure, always-on collection value-utils (`list`, `dict`) injected identically under both
  profiles.
- A **field-name-first, callback-free** surface named after SQL / Shopify Liquid, in the
  snake_case house style.
- Exact `Decimal` aggregates (`sum`/`avg`/`min`/`max`) so currency columns never float-sum.
- `list` ⇄ `dict` interop (`group_by` → `dict`, `dict.entries()` → `list`) as one matched design.
- Zero new dependency, no Rust math beyond the existing `__decimal`, no config, no metering.
- Full IntelliSense coverage via `base.d.ts`, kept honest by the D11 golden test.

**Non-Goals:**
- A general-purpose functional/collection library. No callbacks, no `zip`/`unzip`/`windows`/
  `partition`/`flat_map`, no deep `set`/`map_entries`. If a verb needs a lambda, it is out of scope.
- Random-order operations (`shuffle`/`sample`) — excluded by the pure/both-profiles constraint.
- `Map`/`Set`-backed dicts with non-string keys — `dict` wraps a plain JSON object (string keys);
  business data is JSON-shaped and must round-trip through `toJSON` losslessly.
- Reimplementing numeric semantics — aggregates delegate to the existing `Decimal`.
- SQL-style multi-column projection under the name `select` (would collide with the single-column
  `column` mental model) — deliberately not shipped.

## Decisions

### D1 — Two utils, one change (matched design)

**Decision:** Ship `list` and `dict` together in this change. `list.group_by("f")` returns a
`dict`; `dict.entries()` returns a `list`. Each is a thin wrapper over a plain backing value
(`Array` for `list`, `Object` for `dict`).

**Why:** The two constantly convert into each other; designing them apart makes the seam awkward
and risks inconsistent wrap/unwrap rules. They share the injector, the `.d.ts` conventions, and the
allocation guard.

### D2 — Field-name-first, callback-free surface (adopt, don't invent)

**Decision:** No method takes a function. Filtering is match-by-example (`where({status:"paid"})`);
selection/sort/group/aggregate take a field-name string (`sort_by("price")`, `group_by("region")`,
`column("email")`, `sum("total")`). Nested reads take a dotted path (`get("a.b.c")`).

**Why (prior art):** This is the settled pattern for letting non-programmers reshape tabular data,
arrived at independently by five systems — and the closest analog is in our exact domain:

| Verb | SQL | **Shopify Liquid** | Excel dyn-arrays | lodash shorthand | jq |
|---|---|---|---|---|---|
| `where({...})` | `WHERE x=` | `where: "f","v"` | `FILTER` | `_.filter(x,{a:1})` | `select(.a=="v")` |
| `sort_by("f")` | `ORDER BY` | `sort: "f"` | `SORT` | `_.sortBy(x,'f')` | `sort_by(.f)` |
| `group_by("f")` | `GROUP BY` | `group_by` | `GROUPBY` | `_.groupBy(x,'f')` | `group_by(.f)` |
| `column("f")` | `SELECT f` | `map: "f"` | `CHOOSECOLS` | `_.map(x,'f')` | `map(.f)` |
| `unique()` | `DISTINCT` | `uniq` | `UNIQUE` | `_.uniqBy` | `unique` |

Even lodash — the JS standard — built in "iteratee shorthands" (`_.filter(x,{a:1})`,
`_.map(x,'name')`) because callbacks are the barrier. We make the shorthand the *only* form. Shopify
Liquid is the direct precedent (non-programmer theme authors, e-commerce, embedded language,
pipe-chained `where|sort|map|uniq|sum`).

**Alternatives considered:**
- **Callback-first (lodash/native style)** — `group_by(o => o.region)`: rejected, the arrow
  function is precisely the barrier for this audience.
- **A static free-function namespace** (`list.group_by(arr, "f")`) — rejected: violates the
  chainable-instance-method-only + "no static shortcuts" convention, and loses the Liquid-pipe read.

### D3 — Chainable immutable wrapper; `Proxy`-free indexing

**Decision:** `list([...])`/`dict({...})` return immutable values; every transform returns a new
`list`/`dict` (or a bridged one); `.to_array()`/`.to_object()`/`toString`/`toJSON`/`valueOf` unwrap
to the plain backing value. Element access is `.get(i)`/`.at(i)`; the wrapper implements
`Symbol.iterator` so `for..of` and spread work.

**Why:** Consistency with every sibling value-util, and the fluent chain
`list(orders).where({status:"paid"}).sort_by("date").column("email")` is the Liquid pipe rewritten
in dots — the whole ergonomic point. `toJSON` returning the plain array/object means an emitted or
returned wrapper serializes transparently.

**Why not transparent indexing:** the engine `globals.remove("Proxy")`s before the handler runs, so
`wrapped[i]` is impossible to intercept. `.get(i)`/`.at(i)` is the honest cost and is Pythonic-
adjacent (native `.at()` exists).

### D4 — Aggregates return exact `Decimal`

**Decision:** `sum`/`avg`/`min`/`max` on a column return a `Decimal` (composing over the
already-injected `Decimal` global). `count()` returns a plain `number` (it is a tally, never money).
An author who wants a float for a non-money column calls `.to_number()`.

**Why:** This is an ERP box. Silently doing `0.1 + 0.2` across a column of prices is a bug factory,
and we already ship exact `$`/`money`/`Decimal`. `list(orders).sum("total")` must drop straight into
`$(...)`. This is the one place `list` reaches past `Array`/`Object` — it uses `Decimal`, which is
why the injector runs **after** `decimal::inject_decimal`.

**Edge cases:** empty column → `sum`=`Decimal(0)`, `avg`/`min`/`max`=`null` (no value to report);
non-numeric/absent field values are skipped (documented), matching Liquid/`SUM`'s ignore-blanks
behavior rather than throwing.

### D5 — `dict` wraps a plain JSON object (string keys)

**Decision:** `dict` backs onto a plain `Object`, not a `Map`. Keys are strings; `group_by`/
`index`-shaped keys are stringified. `get("a.b.c", default)` walks dotted segments and returns the
default on any missing/non-object hop. `merge` is a shallow last-wins object merge.

**Why:** Business data is JSON; a `Map` serializes to `{}` and breaks the `toJSON` round-trip that
the whole value-util series relies on. `get`'s dotted safe-read is the single biggest "JSON is ugly
in JS" win and stays callback-free.

### D6 — Pure JS wrapper, no new dependency, no bridge beyond `__decimal`

**Decision:** Implement both surfaces as pure-JS wrappers over `Array.prototype`/`Object`, reusing
the existing `Decimal` global for D4. A minimal Rust injector (one `collections.rs`, or
`list.rs`/`dict.rs` mirroring `text.rs`) evals the wrappers. No `__sys`/FFI bridge, no new crate.

**Why:** Every operation is expressible in JS; the only "math" is the aggregate, which `Decimal`
already provides. Mirrors `text`'s zero-dependency structure.

### D7 — Injected under both profiles; allocation guard; excluded verbs

**Decision:** Inject `list`/`dict` under both `Profile::Full` and `Profile::Deterministic` (they
touch no clock/randomness/ambient state — the sanitizer removes nothing). Cap caller-controlled
expansions before allocation in the spirit of `text.js`'s `MAX_OUTPUT`. **Exclude** `shuffle`/
`sample` (need `Math.random`, impossible under the deterministic profile) and every callback/lambda
verb.

**Why:** Same purity guarantee as `text`; the both-profiles injection is what forbids randomness at
the surface level rather than relying on the sanitizer to strip it.

## Build-vs-Adopt Gate

### Decision: Exact aggregation math — Adopt in-repo `Decimal` (`rust_decimal`)

- **Status**: approved
- **Why**: `sum`/`avg`/`min`/`max` on a currency column must never float-sum (`0.1 + 0.2`); the repo
  already ships an exact `Decimal` global backed by `rust_decimal` — the same engine `money` and the
  NUMERIC wire-decode use — so the aggregates compose over it with zero new dependency.
- **Considered**: hand-rolled JS number summation (rejected — reintroduces the float-sum bug this
  change exists to prevent).
- **Isolation**: aggregates call the already-injected `Decimal` global (over the `__decimal` FFI);
  the `list` injector runs after `decimal::inject_decimal` and reaches nothing new.

### Decision: Collection-shaping verbs — Build hand-written pure JS

- **Status**: approved
- **Why**: the surface is deliberately *not* any library's — a callback-free, field-name-first, ~24
  verb curation for non-programmers. Each verb is ~5 lines of trivial `Array`/`Object` code, and the
  QuickJS isolate has no bundler/tree-shaking, so adopting a lib would ship its full source and still
  need a field-name→callback wrapper on top. Building is smaller, dependency-free, and gives the
  exact API we want.
- **Considered**: adopt lodash-es/radash/remeda (rejected — all callback-first/data-last and ESM
  tree-shaken; wrong API + supply-chain/size cost with no tree-shaking in-isolate); extend a vendored
  lib behind our API (rejected — all the cost of adopting, none of the benefit over building).
- **Isolation**: the entire surface lives in `js/list.js` + `js/dict.js` behind the `list`/`dict`
  factory globals; no new crate, no `__sys`/FFI bridge beyond the existing `Decimal`.

## Risks / Trade-offs

- **`.get(i)` tax over native `arr[i]`.** Accepted: the fluent chain and transparent `toJSON` are
  worth it, and `Proxy` removal makes transparent indexing impossible anyway.
- **Aggregate `Decimal` return may surprise an author summing a quantity column** (gets a `Decimal`,
  not a `number`). Mitigated by `.to_number()` and by `.d.ts` types making the return explicit.
- **Surface-creep pressure** ("just add `zip`/`partition`…"). Held by D2's callback-free rule: any
  verb that would need a lambda is out of scope by construction, which is a clean, defensible line.
- **String-keyed `dict`** loses non-string-key fidelity. Accepted: business data is JSON.

## Migration Notes

Net-new always-on globals; no existing spec, config, or script behavior changes. Authors opt in by
calling `list(...)`/`dict(...)`. `container/types.d.ts` must be regenerated so the D11 golden test
passes.

## Open Questions

- Final aggregate names/edge semantics on empty/non-numeric columns (D4) — pin exact behavior in the
  spec scenarios; current lean is skip-blanks + `null` for empty `avg`/`min`/`max`.
- Whether `list`/`dict` live in one `collections.rs` injector or two files mirroring `text.rs` — an
  implementation detail settled during `/opsx:apply`, not a spec concern.
