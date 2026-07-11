# 5. `$` — Money & Exact Numbers 💵

[← Back to the guide](README.md)

Remember the decimal problem (`0.1 + 0.2 = 0.30000000000000004`)? Here's the fix — and a lot
more, built for **money**.

jsbox gives you two always-on helpers:

- **`$`** (you can also write `$std.money`) — a **money** value that knows its currency. It rounds
  to the right number of cents by itself, refuses to mix currencies, and splits without losing
  a penny. Perfect for prices, tax, and refunds. 🎉
- **`Decimal`** — an **exact number** for everything that _isn't_ money: quantities, weights,
  rates, percentages, ratios.

> Under the hood both use the **same exact-decimal engine** that reads `NUMERIC` columns from
> the database, so the numbers match perfectly.

## Make some money 💰

Wrap an amount with `$(...)` and tell it the currency. Use a **string** for perfect accuracy:

```js
var price = $("19.99", "USD"); // ✅ exact, in US dollars
var yen = $("1000", "JPY"); // yen has no cents
```

Don't want to repeat the currency everywhere? Set it once for the whole request in your
`config` (`"config": { "currency": "USD" }`), or let the operator set a box-wide default. Then
just write the amount:

```js
var price = $("19.99"); // currency comes from config / the box default
```

If no currency is set anywhere, `$("19.99")` throws a friendly error asking for one. Better to
be told than to guess. 🙂

## Do the math (with methods, not `+`)

⚠️ **Important:** you can't use `+ - * /` symbols — JavaScript won't let us make those exact.
Use **methods** instead, and they **chain**:

```js
var net = $("100.00", "USD");
var gross = net.add_pct(8.25); // add 8.25% tax → 108.25 USD
```

| Method              | Means                                | Example                                       |
| ------------------- | ------------------------------------ | --------------------------------------------- |
| `.add(m)`           | plus (same currency)                 | `$("1.10","USD").add($("0.20","USD"))` → 1.30 |
| `.sub(m)`           | minus (same currency)                | `$("5","USD").sub($("1.50","USD"))` → 3.50    |
| `.mul(n)`           | times a **number**                   | `$("19.99","USD").mul(3)` → 59.97             |
| `.div(n)`           | divided by a **number** → money      | `$("99.00","USD").div(3)` → 33.00             |
| `.div(m)`           | divided by **money** → a ratio       | `$("115","USD").div($("100","USD"))` → 1.15   |
| `.neg()`            | flip the sign                        | `$("5","USD").neg()` → -5.00                   |
| `.abs()`            | drop the sign                        | `$("-5","USD").abs()` → 5.00                   |
| `.pct(p)`           | `p` percent of it                    | `$("200","USD").pct(8.25)` → 16.50            |
| `.add_pct(p)`       | add `p` percent (tax, markup)        | `$("100","USD").add_pct(8.25)` → 108.25       |
| `.sub_pct(p)`       | take off `p` percent (discount)      | `$("50","USD").sub_pct(10)` → 45.00           |
| `.round(mode?)`     | round to the currency's cents        | `$("1.005","USD").round()` → 1.01             |

Two safety rules the money value enforces for you:

- **No mixing currencies.** `$("1","USD").add($("1","EUR"))` throws — there's no exchange rate
  inside jsbox, so it never guesses one.
- **Money times money isn't money.** `.mul` only takes a plain number. But **money ÷ money**
  _is_ allowed — it gives a plain `Decimal` **ratio** (great for margin or growth: `115/100 = 1.15`).

## Split without losing a penny 🪙

Refunds and shared costs need the parts to add back up to the whole — exactly. `.allocate_to(n)`
splits evenly, `.allocate(weights)` splits by weight, and `.split(n)` is a nickname for
`.allocate_to`:

```js
$("100.00", "USD").allocate_to(3); // [33.34, 33.33, 33.33]  → sums to 100.00
$("0.05", "USD").allocate([70, 30]); // [0.04, 0.01]          → sums to 0.05
```

The leftover cents go to the biggest fractions first (and, on a tie, to the earlier share), so
the same input always gives the same answer.

## Rounding, your way

`.round()` uses **half-up** by default (the way you learned in school: `1.005` → `1.01`). Need
banker's rounding for a ledger, or always-up/always-down? Pass a mode:

```js
$("1.005", "USD").round("half_even"); // 1.00  (ties go to the even neighbour)
```

Modes: `"half_up"` (default), `"half_even"`, `"up"`, `"down"`, `"ceil"`, `"floor"`.

## Getting money out 📤

| Method         | Gives you                                                          |
| -------------- | ----------------------------------------------------------------- |
| `.to_minor()`  | integer cents for a payment API — `$("19.99","USD")` → `1999`      |
| `.format()`    | a display string — `"$19.99"`                                     |
| `.amount()`    | the number **without** the currency, as a `Decimal`               |
| `.currency()`  | the currency code — `"USD"`                                        |
| `.to_string()` | the exact amount text — `"19.99"`                                  |

In `json(...)`, money turns into a **self-describing** object automatically — amount, currency,
and currency-correct minor units:

```js
function handler() {
  return json({ total: $("19.99", "USD") }, null);
}
// -> { "total": { "amount": "19.99", "currency": "USD", "minor_units": 1999 } }
```

(For yen, `minor_units` is `1000` for ¥1000 — no phantom ×100.)

## Compare two amounts

Same currency on both sides (or it throws):

```js
$("19.99", "USD").gt($("9.99", "USD")); // true
$("-1.00", "USD").is_negative(); // true
```

Methods: `.eq .lt .lte .gt .gte`, `.cmp` (gives `-1`/`0`/`1`), and `.is_zero .is_negative
.is_positive`.

## `Decimal` — exact numbers that aren't money 🔢

For quantities, weights, rates, and percentages, use `Decimal`. Same method style, no currency:

```js
$std.decimal("0.1").add("0.2").to_string(); // "0.3"  (no float mistakes)
$std.decimal("120").clamp(0, 100).to_string(); // "100"
$std.decimal("2.03").round_to("0.05").to_string(); // "2.05"  (round to the nearest 5¢)
$std.decimal("200").pct(15).to_string(); // "30"   (15% of 200)
```

`Decimal` does the same arithmetic as money — `.add .sub .mul .div`, plus `.neg()` (flip the sign)
and `.abs()` (drop it). Handy extras: `.clamp(lo, hi)`, `.min(x)`, `.max(x)`, `.pct(p)`,
`.round(places, mode)`, and `.round_to(step, mode)`. It has the same compares as money
(`.eq .lt … .is_zero`), plus `.to_number()` and `.to_string()`.

## A full order example 🛒

```js
function handler(ctx) {
  // ctx = { items: [ { price: "19.99", qty: 2 }, { price: "4.50", qty: 3 } ], currency: "USD" }
  var subtotal = $("0", ctx.currency);
  for (var i = 0; i < ctx.items.length; i++) {
    var item = ctx.items[i];
    subtotal = subtotal.add($(item.price, ctx.currency).mul(item.qty));
  }
  var total = subtotal.add_pct(8).round(); // +8% tax, rounded to cents

  return json(
    {
      subtotal: subtotal.round(), // { amount, currency, minor_units }
      total: total,
      pay_cents: total.to_minor(), // integer for a payment API
    },
    null,
  );
}
```

## Good to know

- **Always on** — no `config` needed to use them. `$` and `$std.money` are the same thing; `Decimal`
  is separate (numbers, not money).
- Money math stays **exact** until you `.round()` — so round when you're ready to show or store.
- Holds about **28–29 digits** — plenty for money and counting. (Not for giant science numbers.)
- Dividing by zero, mixing currencies, an unknown currency code, or a number too big to hold all
  **throw an error** you can catch with `try/catch`.

## Cheat sheet 📝

- `$("19.99", "USD")` makes money; `$std.decimal("2.5")` makes a plain exact number.
- Use **methods** (`.add .sub .mul .div .add_pct .allocate_to`), **not** `+ - * /`.
- `.round("half_even")` for a ledger; `.to_minor()` for a payment API; `.format()` to show it.
- In `json(...)`, money becomes `{ amount, currency, minor_units }` for free.

> **Moving from the old `$`?** `$` used to be a plain decimal. That's now **`Decimal`**. And
> `.toCents(places)` is now **`.to_minor()`** on money (the currency supplies the digits). See the
> [migration note](#migrating-from-the-old-) below.

## Migrating from the old `$`

`$` changed from "a bare decimal" to "money". Quick mapping:

| Old (decimal `$`)          | New                                             |
| -------------------------- | ----------------------------------------------- |
| `$("19.99")` (not money)   | `$std.decimal("19.99")`                         |
| `$("19.99").toCents()`     | `$("19.99", "USD").to_minor()`                  |
| `$("1000").toCents(0)`     | `$("1000", "JPY").to_minor()` (currency sets 0) |
| `$(1999).fromCents()`      | build money from minor units at construction    |
| `.isZero()` / `.toNumber()`| `.is_zero()` / `.to_number()`                   |

The old camelCase names (`isZero`, `isNegative`, `toNumber`) have been **removed** — use the
snake_case forms (`is_zero`, `is_negative`, `to_number`). They were deprecated aliases for one
release and are now gone, so the surface has exactly one name per operation.

**Next:** [`s3` — Signed Upload & Download Links →](06-s3.md)

[← Back to the guide](README.md)
