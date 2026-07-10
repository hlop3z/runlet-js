## 1. Interop hooks on the wrapper value-utils

- [x] 1.1 Add `__order_key()` and `__id_key()` to `decimal.js` (`Dec` proto): order → the Decimal itself (exact), id → its canonical string.
- [x] 1.2 Add `__order_key()` and `__id_key()` to `money.js` (`Money` proto): order → the amount as exact Decimal (currency stripped), id → `"<amount> <CURRENCY>"`.
- [x] 1.3 Add `__order_key()` and `__id_key()` to `datetime.js` (`DateTime` proto): order → epoch ms, id → ISO string.
- [x] 1.4 Add `__order_key()` and `__id_key()` to `text.js` (`Text` proto): order → the string, id → the string.
- [x] 1.5 Confirm all four hooks are non-enumerable / underscore-prefixed and do NOT appear in any `.d.ts` (internal only).

## 2. Rewire the `list` verbs through the hooks

- [x] 2.1 Add internal `order_of(v)` (prefer `__order_key`; fall back to existing `num_of` numeric coercion; else raw value) and `id_of(v)` (prefer `__id_key`; else `String(v)`) helpers in `list.js`.
- [x] 2.2 `sort_by` (list.js:~97): compare via `order_of`, so decimal/money sort numerically and datetime chronologically (not lexically). Keep stable sort + `desc`.
- [x] 2.3 `where` (list.js:~88): match wrapper-valued fields via `id_of` equality instead of strict `!==`.
- [x] 2.4 `unique`/`unique_by` (list.js:~118-137): dedupe by `id_of` key (Set of id strings) so equal wrapper values collapse; money distinguished by amount+currency.
- [x] 2.5 `group_by` (list.js:~141): key by `id_of` so money keeps currency (USD 19.99 ≠ EUR 19.99); decimal/datetime/text group by exact value.

## 3. Currency-preserving aggregates

- [x] 3.1 Teach `num_of`/the aggregate path to recognize `money` values (no longer skip them).
- [x] 3.2 `sum`/`avg`/`min`/`max`: when the column is `money`, return a `money` preserving currency; reuse `money.js` `sameCurrency` so mixed currencies throw a catchable error. Decimal/number columns keep returning `Decimal`.
- [x] 3.3 Preserve existing empty-column behavior: `sum` → `Decimal(0)`, `avg`/`min`/`max` → `null`.

## 4. Surface lock

- [x] 4.1 Remove the deprecated camelCase aliases `isZero`/`isNegative`/`toNumber` from `decimal.js` (lines ~57-59).
- [x] 4.2 Remove their `@deprecated` declarations from `base.d.ts`.
- [x] 4.3 Declare `toString(): string` and `valueOf()` on the `List` and `Dict` interfaces in `base.d.ts` (match the `Text` precedent).
- [x] 4.4 Rename the private helper `allocateWeights` → `allocate_weights` in `money.js` (and its call sites).
- [x] 4.5 Regenerate `container/types.d.ts` and confirm the golden test `types_dts_is_up_to_date` (crates/runlet-core/src/types.rs) passes.

## 5. Docs sync

- [x] 5.1 `docs/13-lists-and-dicts.md` (~44-79) + `README.md` (~733-742): correct the money-safe column-sum and numeric `sort_by` claims to match actual behavior; add examples over real `$(...)` money columns.
- [x] 5.2 Document the currently-undocumented `list.unique_by` and `list.last`.
- [x] 5.3 `docs/05-decimal.md`: document `money.neg`/`abs`/`to_number` and `decimal.neg`/`abs`; remove any mention of the now-removed camelCase aliases.

## 6. Tests & verification (Docker)

- [x] 6.1 Add/extend Rust unit tests in `runlet-core` for: money-column sum returns money, mixed-currency sum throws, sort_by numeric order over money, group_by currency-distinct, unique dedupes equal money.
- [x] 6.2 Add value-util interop cases to the Python harness (`tests/test_simple.py`) exercising the same via `/execute`.
- [x] 6.3 Run in Docker: `cargo test` (unit + golden), `task clippy` (clean), `cargo fmt --all --check`, then the Python harness.
