# 14. `template` — fill-in-the-blank text 📝

[← Back to the guide](README.md)

Lots of business jobs end the same way: take some data and turn it into **words a person reads** —
an invoice, an email, a text message, a receipt. `template` is the robot's built-in **fill-in-the-blank**
tool. You write the wording once with little `{{ blanks }}`, then hand it the data to fill them in.

It's **always on**, no setup — just like `$` (money), `Decimal`, `datetime`, and `text`.

The blanks use **Jinja** style: `{{ name }}` drops in a value, and `{% ... %}` does small logic
like a loop. If you've used email "merge tags" before, this will feel familiar.

## Two kinds: `html` and `text` (you always pick one) 🔀

There is **no default** — you say which kind of output you mean, because it changes how the robot
keeps you safe:

```js
$std.template.html("<p>Hi {{ name }}</p>"); // for a web page or HTML email
$std.template.text("Hi {{ name }}");         // for a plain email, SMS, or receipt
```

- **`html`** turns dangerous characters in your data into safe ones automatically (so a customer
  named `<b>` can't break — or sneak code into — your page). Use it for **anything shown in a
  browser or HTML email**.
- **`text`** puts the value in exactly as-is. Use it for **plain email, SMS, and receipts**.

```js
$std.template.html("<p>{{ name }}</p>").render({ name: "<b>&co" });
// "<p>&lt;b&gt;&amp;co</p>"   ← made safe for a web page

$std.template.text("Hi {{ name }}").render({ name: "<b>&co" });
// "Hi <b>&co"                 ← left exactly alone
```

## Fill in the blanks with `.render(data)`

Give `render` a plain object. Each key fills the matching blank:

```js
var invoice = $std.template.html(
  "<h1>Invoice for {{ customer }}</h1><p>Total: {{ total }}</p>"
);

invoice.render({ customer: "Acme Ltd", total: "100.00" });
// "<h1>Invoice for Acme Ltd</h1><p>Total: 100.00</p>"
```

You can **reuse** one template with different data as many times as you like — it never changes.

## A little logic: loops 🔁

Use `{% for %}` to repeat a piece for each item — perfect for invoice lines:

```js
$std.template.text("{% for item in items %}- {{ item }}\n{% endfor %}")
  .render({ items: ["Coffee", "Tea", "Cake"] });
// "- Coffee\n- Tea\n- Cake\n"
```

## Missing blanks are gentle, not scary 🌤️

If the data is missing a blank, the robot just leaves it **empty** — it won't throw a tantrum:

```js
$std.template.text("Hi {{ name }}, your code is {{ code }}").render({ name: "Sam" });
// "Hi Sam, your code is "    ← no `code`? no problem
```

Want a nicer stand-in instead of nothing? Use `.missing("…")`:

```js
$std.template.text("Balance: {{ amount }}").missing("—").render({});
// "Balance: —"
```

`.missing()` hands you a **new** template with that setting; the original is untouched.

## "What blanks does this need?" with `.fields()` 🔎

Handy when a person pastes in their own template and you want to ask them for the right data:

```js
$std.template.text("Dear {{ first }} {{ last }}, order {{ order_id }} shipped.").fields();
// ["first", "last", "order_id"]   ← the blanks it uses, sorted
```

## If the template is written wrong ⚠️

A broken template (say you forgot to close a `{{ blank`) throws an error **right away**, when you
create it — so you find the typo immediately, not later. Wrap it in `try/catch` if you want to
handle it:

```js
try {
  $std.template.text("Hi {{ name ");   // oops, never closed
} catch (e) {
  // caught it — the robot keeps running
}
```

## Always the same answer ♻️

`template` never peeks at the clock or a random number. The same template + the same data always
give the **exact same words** — so it works everywhere, including the robot's strict
"deterministic" mode.

## Cheat sheet

```js
$std.template.html(src)          // compile — auto-escapes values (web / HTML email)
$std.template.text(src)          // compile — values left as-is (plain email / SMS / receipt)

tpl.render(data)                 // fill the blanks from an object → a string
tpl.missing("—")                 // new template: show this for a missing blank
tpl.fields()                     // the blank names it uses (sorted)

// blanks:  {{ value }}          drop in a value
//          {% for x in xs %}…{% endfor %}   repeat for each item
```
