# 9. `$sys` — the robot's built-in toolbox 🧰

[← Back to the guide](README.md)

Some helpers don't talk to the internet, a database, or anything outside the box — they're
just handy **tools the robot always carries**: making fingerprints of text (hashes),
signing things. Those live under one name: **`$sys`**.

Think of `$sys` as the robot's tool belt:

- **`$sys.crypto`** — hashing, signing, IDs, and encoders. 🔐

`$sys.crypto` is **always on** — no config, just like `$`. Two more show up only when the
operator fills them in:

> 📅 Looking for dates and times? They moved out of `$sys` into their own always-on tool,
> **`datetime`** — see [the `datetime` guide](10-datetime.md).

- **`$sys.env`** — little settings the operator hands your script (like `REGION`). ⚙️
- **`$sys.secrets`** — passwords/keys you can _use_ but never _see_. 🤫

> Why the funny `$` in front? It tells you "this is a built-in, not your own variable" —
> the same hint `$` (decimal math) gives. Nobody accidentally writes `var $sys = ...`, so
> the name is always safe.

---

## `$sys.crypto` — fingerprints, signatures, and IDs 🔐

### Make a fingerprint (hash)

A **hash** turns any text into a fixed scramble. The same text always gives the same
scramble, but you can't turn the scramble back into the text. Great for checking "did this
change?" or building a cache key.

```js
$sys.crypto.sha256("hello"); // "2cf24dba5fb0a30e..."  (always the same for "hello")
$sys.crypto.sha512("hello"); // a longer one
```

### Sign something (HMAC)

An **HMAC** is a fingerprint made with a **secret key**. Anyone with the same key can check
it, but nobody can fake it without the key. This is how webhooks (Stripe, GitHub) prove a
message really came from them.

```js
// hmac(algorithm, key, message, encoding?)
$sys.crypto.hmac("sha256", "my-key", "the message"); // hex by default
$sys.crypto.hmac("sha256", "my-key", "the message", "base64"); // or "base64url"
```

`algorithm` is `"sha256"` or `"sha512"`. `encoding` is `"hex"` (default), `"base64"`, or
`"base64url"`.

### A random ID

```js
$sys.crypto.uuid(); // "0197c2f3-7d80-7b3a-9e4f-2a1b3c4d5e6f"  (a fresh one every time)
```

It's a **UUIDv7** — the front of the ID is the current time, so IDs made later sort
after IDs made earlier. Great for database keys.

```js
```

### Encoders (turn text into safe shapes) 🔡

Sometimes you need text in a different shape — to put in a URL, an auth header, or a queue
message. Each encoder has `.encode` and `.decode`:

```js
$sys.crypto.base64.encode("hi there"); // "aGkgdGhlcmU="
$sys.crypto.base64.decode("aGkgdGhlcmU="); // "hi there"

$sys.crypto.base64url.encode("hi"); // URL-safe base64 (no = padding)
$sys.crypto.hex.encode("AB"); // "4142"
$sys.crypto.url.encode("a b&c"); // "a%20b%26c"  (safe to drop in a URL)
```

| Encoder     | For…                             |
| ----------- | -------------------------------- |
| `base64`    | general "make it text" packing   |
| `base64url` | IDs/tokens that go **in a URL**  |
| `hex`       | fingerprints, byte-ish values    |
| `url`       | escaping a value for a URL/query |

---

## `$sys.env` — settings from the operator ⚙️

The operator can hand your script little named settings, so the **same script** runs in
dev, staging, and production without editing the code. They turn it on with
`config.sys.env`:

```jsonc
"config": { "sys": { "env": { "REGION": "us-east-1", "TIER": "pro" } } }
```

```js
$sys.env.REGION; // "us-east-1"
$sys.env.NOPE; // undefined  (a key that wasn't set)
```

These are plain, readable values — fine to return in your answer.

---

## `$sys.secrets` — use a password without ever seeing it 🤫

This is the special one. A **secret** (like an API key) is something your script needs to
**use** but should never be able to **leak** — not even by accident, and not even on
purpose. jsbox is built to run scripts from lots of different people, so this rule is
strict.

The operator provides secrets with `config.sys.secrets`:

```jsonc
"config": { "sys": { "secrets": { "SIGNING_KEY": "sk_live_•••••" } } }
```

In your script, `$sys.secrets.SIGNING_KEY` is **not** the password — it's a sealed
**handle**. You can hand the handle to the one tool that's allowed to use it — **HMAC**:

```js
// ✅ This works: the real key is used inside the robot to sign; you get the signature.
var signature = $sys.crypto.hmac("sha256", $sys.secrets.SIGNING_KEY, body);
```

But there is **no way to read the password itself**. Every attempt gives you a harmless
placeholder, never the real value:

```js
String($sys.secrets.SIGNING_KEY); // "[secret:SIGNING_KEY]"
`${$sys.secrets.SIGNING_KEY}`; // "[secret:SIGNING_KEY]"
return json({ k: $sys.secrets.SIGNING_KEY }, null); // -> { "k": "[secret:SIGNING_KEY]" }
```

And you **can't** sneak it out through an encoder — that throws on purpose:

```js
$sys.crypto.base64.encode($sys.secrets.SIGNING_KEY); // ❌ throws: secrets can't be encoded
```

> 🔒 **How it's safe:** the real password never enters your JavaScript at all — it lives
> inside the robot and only comes out to do the one-way HMAC. So there's nothing to leak.
> (One honest note: pick **strong, random** secrets — a short, guessable key could be
> brute-forced from its signatures, which is true of HMAC everywhere, not just here.)

---

## Turning it on 🔘

`$sys.crypto` is **always there** — no config. `$sys.env` and
`$sys.secrets` are empty `{}` until the operator adds them:

```jsonc
"config": {
  "sys": {
    "env": { "REGION": "us-east-1" },
    "secrets": { "SIGNING_KEY": "sk_live_•••••" }
  }
}
```

---

## Cheat sheet 📝

- `$sys.crypto.sha256(t)` / `.sha512(t)` → fingerprint a string.
- `$sys.crypto.hmac("sha256", key, msg)` → sign (key can be a `$sys.secrets.X` handle).
- `$sys.crypto.uuid()` → a fresh ID (UUIDv7, time-ordered).
- `$sys.crypto.base64 / base64url / hex / url` → `.encode()` / `.decode()`.
- `$sys.env.KEY` → operator settings. `$sys.secrets.KEY` → use (HMAC), never read.
- Dates & times? They live in **`datetime`** now — see [10-datetime.md](10-datetime.md).

**Next:** [`datetime` — dates & times done right →](10-datetime.md)

[← Back to the guide](README.md)
