# 10. `datetime` — dates & times done right 📅

[← Back to the guide](README.md)

Dates are sneaky. "The end of the month" is a different moment in Tokyo than in New York.
"One month after January 31" isn't February 31 (there's no such day!). `datetime` is the
robot's built-in tool for getting all of this **right** — and it's **always on**, no setup,
just like `$` (money) and `Decimal`.

> 🧭 It used to live at `$sys.date`. It grew up and moved out into its own name: **`datetime`**.

## Make a `datetime`

```js
var d = datetime.parse("2026-07-10T13:30:00Z"); // an ISO / RFC 3339 moment
var day = datetime.parse("2026-07-10");          // just a day works too (becomes midnight UTC)
var ms = datetime.parse(1783690200000);          // or epoch milliseconds
var now = datetime.now();                         // right now (UTC)
var built = datetime.from({ year: 2026, month: 7, day: 10 }); // from parts
datetime("2026-07-10T13:30:00Z");                // calling datetime(x) is the same as .parse(x)
```

`parse` understands ISO 8601 / RFC 3339 (with or without a timezone), a plain `YYYY-MM-DD`,
or epoch millis — and always stores a **UTC instant** inside. It will **not** guess a
`07/10/2026`-style string (is that July 10th or October 7th? nobody agrees) — that throws, on
purpose, so you never get a silent wrong answer.

> ⏱️ `datetime.now()` is the one part that's turned **off** in the "deterministic" mode (where
> the same input must always give the same answer). Everything else keeps working — you just
> pass in the moment you care about.

## A `datetime` never changes 🔒

Every method gives you a **new** value; the one you had stays put. Great for avoiding bugs.

```js
var d = datetime.parse("2026-07-10T00:00:00Z");
var next = d.add({ days: 1 });
d.day();    // 10  (unchanged!)
next.day(); // 11
```

## Read the pieces

```js
var d = datetime.parse("2026-07-10T13:30:00Z");
d.year();       // 2026
d.month();      // 7   (1–12)
d.day();        // 10
d.hour();       // 13
d.weekday();    // 5   (ISO: 1 = Monday … 7 = Sunday, so 5 = Friday)
d.quarter();    // 3   (1–4)
d.day_of_year(); // 191
d.iso_week();   // { week: 28, week_year: 2026 }
d.days_in_month(); // 31
```

## Do calendar math

Add or subtract any mix of `years`, `months`, `weeks`, `days`, `hours`, `minutes`,
`seconds`, `ms`:

```js
var due = datetime.parse(ctx.invoiced).add({ months: 1 }); // one month later
datetime.parse(ctx.when).sub({ weeks: 2 });                 // two weeks earlier
```

Months and years are **smart** about short months — Jan 31 + 1 month lands on the **last day
of February**, not an imaginary Feb 31:

```js
datetime.from({ year: 2026, month: 1, day: 31 }).add({ months: 1 }).day(); // 28
```

### Snap to the start or end of a period

`day`, `week` (starts **Monday**), `month`, `quarter`, `year`:

```js
d.start_of("month").iso(); // "2026-07-01T00:00:00Z"
d.end_of("month").iso();   // "2026-07-31T23:59:59.999Z"
d.start_of("quarter");     // first instant of the calendar quarter
```

### Business days (skip the weekend)

Saturdays and Sundays are skipped. (Holidays are **not** — those are country/company specific.)

```js
var fri = datetime.parse("2026-07-10T00:00:00Z"); // a Friday
fri.add_business_days(1).weekday(); // 1 (the next Monday)
fri.is_weekend();                    // false
fri.is_business_day();               // true
```

## How far apart? Compare?

```js
var a = datetime.parse("2026-07-10T00:00:00Z");
var b = datetime.parse("2026-07-08T00:00:00Z");
a.diff(b);            // { total_ms, total_seconds, days: 2, hours, minutes, seconds }  (signed)
a.diff_in(b, "days"); // 2   (whole units: ms|seconds|minutes|hours|days|weeks)

a.gt(b);              // true   (also: cmp / eq / lt / lte / gte)
a.is_between(b, a);   // true   (inclusive)
```

> 🚫 There's no "is it in the past?" helper on purpose — that would secretly read the clock.
> Compare against `datetime.now()` yourself when you mean "now".

## The big one: timezones 🌍

The value is always a UTC instant. Ask for a **view** in someone's timezone with
`in_zone("Area/City")` — the components, boundaries, and formatting resolve **there**, while the
underlying moment stays the same. This is the thing UTC-only tools simply can't do.

```js
var d = datetime.parse("2026-07-15T12:00:00Z");
var tokyo = d.in_zone("Asia/Tokyo");
tokyo.hour();               // 21   (12:00 UTC is 21:00 in Tokyo)
tokyo.epoch_ms() === d.epoch_ms(); // true — it's the SAME moment, just read in Tokyo

// "End of this month, in the customer's timezone" — done right:
d.in_zone("America/New_York").end_of("month").iso();
```

A timezone name it doesn't recognize throws, so typos surface immediately.

## Get your answer out

```js
d.iso();      // "2026-07-10T13:30:00Z"   (RFC 3339; pass a zone for that offset)
d.unix();     // 1783690200               (epoch seconds)
d.epoch_ms(); // 1783690200000            (epoch milliseconds — the canonical value)
d.format("YYYY-MM-DD HH:mm:ss");        // "2026-07-10 13:30:00"  (numeric tokens only)
```

`format` tokens are plain **numbers** (no language-specific month names): `YYYY` `YY` `MM`
`DD` `HH` `mm` `ss` `SSS`; anything else is a literal.

And in `json(...)`, a `datetime` becomes its ISO string (UTC `Z`) **automatically**:

```js
return json({ due: d }, null); // -> { "due": "2026-07-10T13:30:00Z" }
```

## Cheat sheet 📝

- **Make:** `datetime(x)` / `datetime.parse(x)` / `datetime.now()` / `datetime.from({year,month,day,…}, zone?)`.
- **Read:** `year/month/day/hour/minute/second/millisecond`, `weekday` (1=Mon), `quarter`,
  `day_of_year`, `iso_week`, `days_in_month`.
- **Math:** `add({…})` / `sub({…})` (months/years clamp end-of-month), `start_of/end_of("day|week|month|quarter|year")`,
  `add_business_days(n)`, `is_weekend/is_business_day`.
- **Compare:** `diff(o)` / `diff_in(o, unit)`, `cmp/eq/lt/lte/gt/gte`, `is_between(lo, hi)`.
- **Zones:** `in_zone("Area/City")` → a view; `epoch_ms()` never changes.
- **Out:** `iso(zone?)` / `unix()` / `epoch_ms()` / `format(pattern, zone?)`; `json()` → ISO string.

**Next:** [Hasura — GraphQL the easy way →](11-hasura.md)

[← Back to the guide](README.md)
