# 2. `http` — Talk to the Internet 🌐

[← Back to the guide](README.md)

`http` lets your script visit other websites and ask them for data — like a phone
that can only call numbers you've allowed.

## Turn it on first 🔑

Tell the robot which websites it's allowed to visit, using `allowed_hosts`:

```json
{
  "config": {
    "allowed_hosts": ["api.example.com"]
  }
}
```

- `["api.example.com"]` → only that website is allowed.
- `["*"]` → **any** website (the star means "all").
- `[]` or missing → `http` is turned off.

If your script tries a website that isn't on the list, the robot says no. 🚫

## The five ways to call

| Call                            | When you use it          |
| ------------------------------- | ------------------------ |
| `http.get(url, params, headers)` | Ask for something / read |
| `http.post(url, body, headers)`  | Create something new     |
| `http.put(url, body, headers)`   | Replace something        |
| `http.patch(url, body, headers)` | Change part of something |
| `http.delete(url, headers)`      | Remove something         |

`params`, `body`, and `headers` are all optional.

## Reading data (GET)

```js
function handler(ctx) {
  var res = http.get("https://api.example.com/users", { page: 1 });
  // res looks like: { status: 200, data: [ ...users... ] }
  return json(res.data, null);
}
```

What you get back has **two parts**:

- **`res.status`** — the number the website replied with. `200` means "OK!" 👍
- **`res.data`** — the actual stuff (already unpacked for you, ready to use).

The `{ page: 1 }` becomes `?page=1` on the end of the web address.

## Sending data (POST)

```js
function handler(ctx) {
  var created = http.post("https://api.example.com/users", { name: ctx.name });
  return json(created.data, null);
}
```

The second thing (`{ name: ctx.name }`) is the **body** — what you're sending.

## Adding headers (like a secret password) 🪪

Some websites need a password called a "token". You add it as **headers** (the last thing):

```js
function handler(ctx) {
  var me = http.get("https://api.example.com/me", null, {
    Authorization: "Bearer " + ctx.token,
  });
  return json(me.data, null);
}
```

> Note: you can't change the `Content-Type` header — the robot sets that one for you.

## Only real web addresses work 🛡️

`http` can only reach **`http://` and `https://`** addresses. Anything else — `file://`,
`gopher://`, `ftp://`, `data:` — is turned away before the robot even dials, and a website
that tries to *bounce* you to one of those (a redirect) is not followed either. The same
door is checked on every hop, so there's no sneaking through a redirect.

It also refuses to visit private/internal addresses (your own network, `localhost`, cloud
metadata) — even ones written in a tricky way (`2130706433`, `0x7f000001`, `127.1`) or
hidden inside an IPv6 wrapper — and it locks onto the exact address it checked, so a website
can't say one thing and connect somewhere else. This guard is built into the framework: any
capability a developer marks *script-controlled* gets it automatically, for free.

> For operators: the in-engine guard is a strong first line, but the recommended second,
> independent line is **network-layer egress control** (a firewalled netns or egress proxy)
> at deploy time — defense in depth, not a substitute. See `docs/security-hardening.md`.

## It shows up on the receipt 🧾

Every call you make is listed in `meta.http_requests` in the answer, so you can see
what happened (which website, how long it took, the status). Handy for checking your work!

**Next:** [`db` — talk to a database →](03-database.md)
