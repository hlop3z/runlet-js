# `check` — is this number typed right? ✅

Some numbers have a secret last digit that proves the rest were typed correctly — a **check
digit**. Credit cards, shop barcodes, and bank codes all use one. `$std.check` does that math
for you, so you can catch a typo *before* you send it anywhere.

It's **always on** — no setup.

```js
function handler(ctx) {
  const ok = $std.check(ctx.card).luhn();
  return json({ data: { looks_valid: ok } });
}
```

## What "valid" means here (read this!) 🧐

`check` only tells you the **digits are consistent with each other** — that the number was
probably typed correctly. It does **not** tell you the card, barcode, or account is **real**,
active, or belongs to anyone. A number can pass the check and still be made-up.

Think of it like a spell-check: "this word is spelled correctly" is not "this word is true." Use
`check` to catch typos early; use the real bank/shop/service to find out if something actually
exists.

## The three checks 🔎

### `luhn()` — cards & device IDs

Validates the **Luhn** check digit (the one credit/debit cards and IMEI phone IDs use). Spaces
and hyphens are fine — they're ignored.

```js
$std.check("4111111111111111").luhn();       // true
$std.check("4111 1111 1111 1111").luhn();     // true (spaces are OK)
$std.check("4111111111111112").luhn();        // false (typo in the last digit)
```

### `gtin()` — shop barcodes

Validates the **GS1** check digit on a product barcode: UPC-A (12 digits), EAN-13 (13), GTIN-8
(8), or GTIN-14 (14). Digits only.

```js
$std.check("4006381333931").gtin();   // true  (EAN-13)
$std.check("036000291452").gtin();    // true  (UPC-A)
$std.check("12345").gtin();           // false (not a real barcode length)
```

### `iso7064("mod_97_10")` — bank-code style checks

Validates an **ISO/IEC 7064 MOD 97-10** check — the math behind an **IBAN** or **LEI** code.
Letters and digits are both fine.

For an **IBAN**, do one small step first: move the first 4 characters (the country + check
digits) to the **end**, then check the rest:

```js
const iban = "GB82WEST12345698765432";
const rearranged = iban.slice(4) + iban.slice(0, 4);   // "WEST12345698765432GB82"
$std.check(rearranged).iso7064("mod_97_10");           // true
```

`check` does **not** know about countries or IBAN rules on its own — that's on purpose. You do
the little rearrange step, and `check` does the pure math.

## Bad input never crashes 🛟

If you hand `check` something that isn't the right shape — empty, letters where digits go, wrong
length — it just returns `false`. It never throws, so you can call it straight inside an `if`:

```js
if ($std.check(ctx.card).luhn()) {
  // looks good, carry on
} else {
  // ask them to re-type it
}
```

## What `check` will *not* do 🚫

To stay simple and correct forever, `check` sticks to worldwide standards that never change. It
**does not** validate things that depend on ever-changing country lists or rulebooks:

- No `iban` / `bic` / `vat` "is this a valid code for this country" check — country rules change.
  (For an IBAN's *math*, use the `iso7064` step above.)
- No `isbn` / `issn` book/magazine codes.

If you need one of those, do the check with the real service that owns the up-to-date list.

---

**Remember:** `check` catches typos, not lies. Green means "typed right," not "real." ✅
