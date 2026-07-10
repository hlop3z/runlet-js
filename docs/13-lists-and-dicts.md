# 13. `list` & `dict` — tidy up tables and records 📋

[← Back to the guide](README.md)

Business scripts are full of **lists of things** (a basket of orders, a bunch of products) and
**single records** (one customer, one order). Plain JavaScript makes those surprisingly fiddly —
you end up writing little `for` loops and arrow-function puzzles just to add up a column or grab a
field. `list` and `dict` do it for you, and they're **always on**, no setup, just like `$` (money),
`Decimal`, `datetime`, and `text`.

The big idea: **you never write a function.** You just name the **field** you care about — the same
way you would in a spreadsheet, in SQL, or in a Shopify theme. `where`, `sort_by`, `group_by`,
`sum` — if you've used those words before, you already know how these work.

## `list` — a table of records

Wrap an array (usually a list of little objects) and shape it with a chain:

```js
var orders = [
  { id: 1, status: "paid", region: "US", total: "19.99" },
  { id: 2, status: "open", region: "US", total: "5.00" },
  { id: 3, status: "paid", region: "EU", total: "12.50" },
];

// Only paid orders, newest field order, just the totals:
list(orders).where({ status: "paid" }).column("total").to_array();
// ["19.99", "12.50"]
```

### Pick rows: `where`

Give it an example and it keeps the rows that match **all** of it — no `if`, no arrow function:

```js
list(orders).where({ status: "paid", region: "US" }).count(); // 1
```

### Sort rows: `sort_by`

Name the field. Add `"desc"` to flip it:

```js
list(orders).sort_by("total").column("id").to_array();          // cheapest first
list(orders).sort_by("total", "desc").first();                  // the biggest order
```

Money, decimals, and dates sort **by their real value**, not alphabetically — so `$100.00` sorts
after `$19.99` (a plain `"100.00"` string would sort *before* it), and a `datetime` column sorts in
time order.

### Grab one column: `column`

```js
list(orders).column("region").unique().to_array(); // ["US", "EU"]
```

`unique()` removes duplicate scalars; `unique_by("field")` keeps the first row per field value. Both
compare by real value, so two equal `money` or `datetime` values count as the same (and `$1 USD` vs
`$1 EUR` stay distinct — the currency is part of the identity).

### Group rows: `group_by`

This hands you back a **`dict`** (see below) — one key per group, each holding a little `list`:

```js
var byRegion = list(orders).group_by("region");
byRegion.get("US").count(); // 2
byRegion.get("EU").sum("total").toString(); // "12.5"
```

### Add up a money column: `sum`, `avg`, `min`, `max` 💰

These are the important ones for money — and they're **exact**. Adding `0.1 + 0.2` in plain
JavaScript famously gives `0.30000000000000004`. Here you get an exact answer, so cents never drift:

```js
list(orders).where({ status: "paid" }).sum("total").toString(); // "32.49"  (exact!)
```

The result type follows the column:

- A column of **plain numbers or number strings** (like `total: "19.99"`) → a real {@link Decimal},
  the same exact-money type `$` uses.
- A column of real **`money` values** (built with `$(...)`) → a **`money`** back, with the currency
  kept. Mixing currencies in one column **throws** (there's no silent conversion) — split by
  currency first with `group_by` if you need per-currency totals.

```js
var cart = [{ price: $("0.10", "USD") }, { price: $("0.20", "USD") }];
list(cart).sum("price").format(); // "$0.30"  — a money value, not a bare number
```

- `sum` → a `Decimal` or `money` (empty column → `Decimal(0)`)
- `avg` / `min` / `max` → a `Decimal` or `money`, or `null` if there's nothing to measure
- `count()` → a plain number (it's a tally, not money)

Blank or non-number values are simply **skipped**, so one missing price won't break the total. Want
a plain number instead of a `Decimal`? Add `.to_number()`.

### Peek at items

Because the robot keeps lists safe, you read an item with `.get(i)` (or `.at(-1)` for the last
one) — **not** square brackets `[i]`:

```js
list(["a", "b", "c"]).get(0);  // "a"
list(["a", "b", "c"]).at(-1);  // "c"
list(orders).first();          // the first order (or null if empty)
list(orders).last();           // the last order  (or null if empty)
list(orders).len();            // 3
```

You can also loop with `for..of` or spread with `[...]` — those work like normal.

## `dict` — one record

Wrap a single object to read and reshape it safely:

```js
var customer = {
  name: "Ada",
  address: { city: "London", zip: "EC1" },
  vip: true,
};
```

### Safe nested read: `get` 🕳️

The everyday hero. Reach deep with a dotted path, and give a fallback so a missing piece never
crashes your script:

```js
dict(customer).get("address.city");          // "London"
dict(customer).get("address.country", "—");  // "—"  (missing → your fallback)
dict(customer).get("billing.card.last4");    // undefined (no crash)
```

### Keep or drop fields: `pick` / `omit`

```js
dict(customer).pick("name", "vip").to_object();  // { name: "Ada", vip: true }
dict(customer).omit("address").to_object();      // { name: "Ada", vip: true }
```

### Check and combine: `has` / `merge`

```js
dict(customer).has("vip");                       // true
dict(customer).merge({ vip: false, tier: "gold" }).to_object();
// { name:"Ada", address:{…}, vip:false, tier:"gold" }   (last value wins)
```

### Turn it into a list: `keys` / `values` / `entries`

These bridge back to `list`, so you can keep chaining:

```js
dict({ a: 1, b: 2 }).keys().to_array();    // ["a", "b"]
dict({ a: 1, b: 2 }).values().to_array();  // [1, 2]
dict({ a: 1, b: 2 }).entries().to_array(); // [["a",1], ["b",2]]
```

## They never change 🔒

Like every other value-util, `list` and `dict` are **immutable**: every method hands you a **new**
value and leaves the original alone. Chain freely without worrying about clobbering your data.

```js
var l = list([3, 1, 2]);
l.sort_by().to_array(); // [1, 2, 3]
l.to_array();           // [3, 1, 2]  (unchanged!)
```

## Get the plain data back

- `list(...).to_array()` → a normal array
- `dict(...).to_object()` → a normal object
- Returning or `emit`-ing one just works — it serializes as plain JSON automatically.

## Good to know

- **No functions, ever.** If a task seems to need a callback (a custom rule, a computed sort), that's
  on purpose out of scope — keep the surface simple. Reach for the field-name verbs above.
- **Keys are text.** A `dict` is a plain JSON record, so its keys are strings (just like JSON).
- **Money stays exact.** `sum`/`avg`/`min`/`max` give you `Decimal`, not a floating-point number —
  the whole reason this lives in a money-safe box.
