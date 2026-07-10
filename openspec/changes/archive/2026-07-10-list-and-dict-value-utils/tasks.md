## 1. `list` JS wrapper (`crates/runlet-core/src/js/list.js`)

- [x] 1.1 Create the IIFE wrapper following `js/text.js` structure: an immutable `Lst` value holding a plain array, `list(input)` factory that coerces to an array (non-array → wrap-as-single or empty per the factory contract), and `.to_array()`/`toJSON`/`valueOf`/`toString` unwrap. Implement `Symbol.iterator`, `.get(i)`/`.at(i)`, and `.len()` — no `Proxy` (D3).
- [x] 1.2 Implement the field-name-first, callback-free shaping verbs (D2): `where(match)` (keep records where every `field: value` in `match` equals the record's field), `sort_by(field, direction?)` (stable, ascending; `"desc"` descending), `column(field)` (new `list` of one field per record), `unique()` (distinct scalars by value), `unique_by(field)` (distinct records by field, first wins). No callbacks anywhere.
- [x] 1.3 Implement `group_by(field)` returning a `dict` of `list`s (stringified keys, input order preserved within each group) — the list→dict bridge (D1).
- [x] 1.4 Implement exact-`Decimal` aggregates over the `Decimal` global (D4): `sum(field)` → `Decimal` (empty → `Decimal(0)`), `avg`/`min`/`max(field)` → `Decimal` or `null` when empty, skipping non-numeric/absent values; `count()` → plain number.
- [x] 1.5 Implement `first()`/`last()` (element or `null` when empty).
- [x] 1.6 Add the caller-controlled allocation guard (D7) in the spirit of `text.js`'s `MAX_OUTPUT`, throwing before any oversize expansion allocates.

## 2. `dict` JS wrapper (`crates/runlet-core/src/js/dict.js`)

- [x] 2.1 Create the IIFE wrapper: an immutable `Dct` value over a plain object (non-object → empty record), `dict(input)` factory, and `.to_object()`/`toJSON`/`valueOf`/`toString` unwrap. Keys are strings (D5).
- [x] 2.2 Implement `get(path, default?)` — dotted safe read, returning the value at the full path or the default (or `undefined`) on any missing/non-object hop (D5).
- [x] 2.3 Implement the callback-free reshaping/membership verbs: `pick(...fields)`, `omit(...fields)`, `has(field)`, `merge(other)` (shallow last-wins). Each transform returns a new `dict`.
- [x] 2.4 Implement `keys()`/`values()`/`entries()` returning `list`s (insertion order; `entries()` → list of `[k,v]` pairs) — the dict→list bridge (D1).

## 3. Rust injector + wiring

- [x] 3.1 Create the injector(s) mirroring `text.rs` — one `collections.rs` (or `list.rs`/`dict.rs`): `include_str!` each wrapper and `inject_list`/`inject_dict` that eval them; module + fn docs noting they are pure and need no `__sys` bridge (only the existing `Decimal` global).
- [x] 3.2 Register the module in `lib.rs` and call the injectors from `engine.rs` alongside the other always-on value-utils, **after `decimal::inject_decimal`** (aggregates compose over `Decimal`) and under **both** profiles (no entry in `js/determinism.js`). Inject `dict` before `list` (or ensure `list.group_by` can reach the `dict` factory at call time).

## 4. Type declarations

- [x] 4.1 Add `list`/`dict` `.d.ts` fragments to `crates/runlet-core/src/js/base.d.ts` declaring every public method (snake_case, instance-only, no static shortcuts), prefixing interface names to keep the shared TS namespace flat; type the aggregates as returning `Decimal`.
- [x] 4.2 Regenerate `container/types.d.ts` and confirm the D11 golden test (`types_dts_is_up_to_date`) passes.

## 5. Tests

- [x] 5.1 `list` unit tests (Rust, through the engine) covering each spec scenario: unwrap/`toJSON`, immutability, iteration + `.get`/`.len`, `where`/`sort_by` (asc+desc)/`column`/`unique`/`unique_by`, `group_by` → dict of lists, `first`/`last`, and the allocation-refusal guard.
- [x] 5.2 Aggregate tests pinning exactness: `sum(["0.1","0.2"])` → `"0.3"` (not `0.30000000000000004`), `avg`/`min`/`max` return `Decimal`, `count()` a number, empty-column semantics (`sum`=0, `avg`/`min`/`max`=`null`), non-numeric values skipped.
- [x] 5.3 `dict` unit tests: unwrap/`toJSON`, immutability, `get` dotted read (present + missing→default), `pick`/`omit`/`has`/`merge`, `keys`/`values`/`entries` → lists.
- [x] 5.4 Test that `list`, `dict`, and all methods are present and identical under `Profile::Deterministic` (nothing removed/stubbed), and confirm no random-order verb exists.

## 6. Docs & gate

- [x] 6.1 Add beginner doc page(s) under `docs/` for `list`/`dict` (matching the other value-util pages), leaning on the spreadsheet/SQL/Liquid framing, noting aggregates return exact `Decimal` and indexing is `.get(i)` (not `[i]`); update `README.md` reference.
- [x] 6.2 Run the full gate in Docker: `cargo test -p runlet-core`, `cargo clippy` (project gate, re-run until clean), and `cargo fmt --all --check`.
