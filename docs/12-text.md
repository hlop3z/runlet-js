# 12. `text` — tidy strings the easy way ✂️

[← Back to the guide](README.md)

Text is everywhere in business scripts: product names, reference codes, account numbers, labels
for a receipt. `text` is the robot's built-in tool for **cleaning and shaping** strings — and
it's **always on**, no setup, just like `$` (money), `Decimal`, and `datetime`.

The names come from Python (`lower`, `strip`, `zfill`, …) so they're short and friendly. The
*behavior* is plain JavaScript underneath — we just gave it nicer names and added a few business
helpers.

## Make a `text`

```js
var t = text("  Hello World  "); // wrap any string
text(42);                        // numbers become "42"
text(t);                         // already a text? you get it back
```

Get the plain string back out any time with `.value`:

```js
text("Ac-Me").lower().value; // "ac-me"  ← a normal string again
```

## A `text` never changes 🔒

Every method gives you a **new** value; the one you had stays put. Great for avoiding bugs.

```js
var t = text("  Hi  ");
t.strip().value; // "Hi"
t.value;         // "  Hi  "  (unchanged!)
```

You can **chain** as many as you like — each step hands the next one a fresh value:

```js
text("  Café Ör 01! ").strip().slugify().value; // "cafe-or-01"
```

## The everyday helpers (Python names)

```js
text("HELLO").lower().value;          // "hello"
text("hello").upper().value;          // "HELLO"
text("hello world").title().value;    // "Hello World"
text("hello").capitalize().value;     // "Hello"

text("  spaced  ").strip().value;     // "spaced"
text("xxcodexx").strip("x").value;    // "code"   (strip specific characters)

text("SKU-0042").starts_with("SKU-"); // true
text("SKU-0042").removeprefix("SKU-").value; // "0042"
text("invoice.pdf").removesuffix(".pdf").value; // "invoice"

text("a.b.c").replace(".", "-").value; // "a-b-c"  (replaces ALL, like Python)
text("a.b.c").count(".");              // 2

text("a,b,c").split(",");              // ["a", "b", "c"]  (plain strings)
text("line1\nline2").splitlines();     // ["line1", "line2"]
```

**Is it made of…?** (handy for checking codes — `false` for an empty string)

```js
text("0042").is_digit(); // true
text("Café").is_alpha(); // true
text("A1").is_alnum();   // true
text("   ").is_space();  // true
```

## Line things up (padding)

```js
text("42").zfill(6).value;         // "000042"   (zero-pad a reference number)
text("-42").zfill(6).value;        // "-00042"   (keeps the sign in front)
text("x").rjust(5).value;          // "    x"
text("x").ljust(5, ".").value;     // "x...."
text("hi").center(6, "-").value;   // "--hi--"
```

> 🛟 Padding widths are **capped** so a runaway number (like `rjust(9999999999)`) can't eat all
> the memory — it throws a clear error instead.

## The business helpers 🧾

```js
// Turn a name into a URL-safe / code-safe slug (accents are folded away):
text("Café Málaga #2").slugify().value; // "cafe-malaga-2"

// Hide all but the last few characters (for showing a card or account safely):
text("4111111111111234").mask().value;            // "************1234"
text("4111111111111234").mask({ keep: 4, char: "#" }).value; // "############1234"
text("secret@mail.com").redact({ keep: 3 }).value; // "************com"  (redact = mask)

// Squash messy spacing into single spaces:
text("too    many\t spaces").collapse().value; // "too many spaces"

// Shorten long text with an ellipsis (…):
text("a very long description").truncate(10).value; // "a very lo…"
```

> 🔐 **`mask`/`redact` is for *showing* things safely — it is not encryption.** The hidden
> characters are simply gone from the result; there's no way to get them back. To hash or sign a
> value (something reversible/verifiable), reach for [`$sys.crypto`](09-sys.md) instead.

## Good to know

- **Counting is JavaScript-style.** `len()` and widths count UTF-16 units, so an emoji or some
  rare characters may count as 2. For everyday business text (letters, digits, punctuation)
  it matches what you'd expect.
- **Slugify folds accents, and drops other alphabets.** `"Café"` → `cafe`, but a word in
  Chinese or Cyrillic is left out of the slug rather than guessed at. That keeps codes
  predictable. (Need those alphabets kept? Ask — that's a future add-on.)
- **Checking things like "is this a real email?" isn't here.** `text` *shapes* strings.
  Validating an email/phone/URL is a different job (a future `valid` helper). `is_digit` and
  friends only ask "is every character a digit/letter/space?".
- **Always on, works everywhere.** `text` needs no config and is available even in the strict
  "deterministic" mode — nothing it does depends on the clock or randomness.

## Cheat-sheet

- **Make / unwrap:** `text(x)` → value; `.value` / `String(v)` → plain string; `json()` → the string.
- **Case:** `lower/upper`, `capitalize`, `title`, `swap_case`.
- **Trim:** `strip/lstrip/rstrip` (optional characters, else whitespace).
- **Ends:** `starts_with/ends_with`, `removeprefix/removesuffix`.
- **Edit:** `replace` (all), `count`, `split/rsplit/splitlines`.
- **Pad:** `zfill`, `ljust/rjust/center` (width is capped).
- **Is it…?** `is_digit/is_alpha/is_alnum/is_space`.
- **Business:** `slugify`, `mask/redact`, `collapse`, `truncate`, `len`.

**Next:** [When Things Go Wrong (Errors) →](99-errors.md)

[← Back to the guide](README.md)
