# 99. When Things Go Wrong (Errors) 🚦

[← Back to the guide](README.md)

Sometimes a script can't finish — a database is down, an email bounces, or your code
has a typo. The robot never just crashes on you. It hands back a **clear, labeled
error** you can act on, instead of a scary blob of text. This page is the decoder ring. 🔑

## Two kinds of "error"

Remember `json(data, error)`? The **second slot** is for errors. An error shows up in
the answer in one of two ways:

1. **Your error** — you call `return json(null, { message: "name required" })`. Whatever
   you put there comes back **exactly as you wrote it**. The robot doesn't touch it.
2. **The robot's error** — something broke that you didn't hand-write (a timeout, a
   database hiccup, a typo that crashes your script). The robot fills `error` with a
   neat **labeled object** (below).

So `error` is `null` when all is well, **your shape** when you set it, or the robot's
**labeled shape** when the robot caught the problem.

**One rule ties it all together: an answer with an `error` is never a `200`.** A `200`
means "success, no error" — full stop. If `error` is set (yours or the robot's), the HTTP
status is a `4xx` or `5xx` that matches `retryable` (below), so a dumb retry worker can
route on the status line alone. Want your own error to say "retry me"? Add a top-level
`retryable: true` to it: `return json(null, { message: "later", retryable: true })` →
the status becomes `503` (with a `Retry-After`). Leave it off and your error parks at
`422`. Either way your `error` body is passed back **exactly as you wrote it** — the robot
only *reads* that one key, it never rewrites your shape.

## What the robot's error looks like 🔖

```json
{
  "data": null,
  "error": {
    "type": "capability",
    "source": "db",
    "code": "DB_CONSTRAINT",
    "message": "database request failed",
    "retryable": false,
    "owner": "developer",
    "details": { "sqlstate": "23505" }
  },
  "meta": { "trace_id": "be04...", "...": "..." }
}
```

You never have to read the message text to know what happened — every field is there
so a **program** can decide what to do:

| Field       | What it tells you                                                                              |
| ----------- | ---------------------------------------------------------------------------------------------- |
| `type`      | the big bucket: `request`, `runtime`, `script`, or `capability` (see below)                    |
| `source`    | who it came from: `engine`, `handler`, or a tool (`db`/`mail`/`s3`/`api`/`redis`/`amq`/`auth`) |
| `code`      | a stable label you can switch on, like `DB_CONSTRAINT`. Never changes meaning.                 |
| `message`   | a short, safe sentence for humans. (Secrets/PII never go here.)                                |
| `retryable` | `true` = trying again might help; `false` = it won't, don't bother                             |
| `owner`     | **who should fix it**: `caller`, `developer`, or `operator`                                    |
| `details`   | extra machine-readable bits, like `{ "sqlstate": "23505" }`                                    |
| `debug`     | the nerdy stuff (stack trace + raw text) — only when turned on (see the bottom)                |

## The four buckets (`type`) 🪣

| `type`       | Means                                                                      | What you do                                        |
| ------------ | -------------------------------------------------------------------------- | -------------------------------------------------- |
| `request`    | the **message** you sent was bad (too big)                                 | fix the request                                    |
| `runtime`    | the **engine** couldn't run your script (typo, ran too long, no `handler`) | fix the script                                     |
| `script`     | your code **threw** an error (`throw`, or a bug like a `TypeError`)        | fix the script; `message` is your error's own text |
| `capability` | a **tool** failed (database down, email bounced)                           | if `retryable`, try again; otherwise look closer   |

## Who should fix it? (`owner`) 🧑‍🔧

`owner` is the handiest field for bigger setups — it says **who to call**:

- **`caller`** — whoever sent the request (it was malformed).
- **`developer`** — the script author (a bug, bad SQL, too many operations).
- **`operator`** — the people running the servers (a database/broker is down).

So a dead database pages the ops team, but your `TypeError` doesn't. 🙂

## The traffic light (HTTP status) 🚥

The HTTP status is a truthful projection of the answer, so a gateway or a dumb queue
worker can route on the **status line alone**: the *first digit* is the whole story —
**`2xx` = ack, `4xx` = park (don't retry), `5xx` = retry.** The class is a pure function
of `retryable`; `owner` only picks *which* code inside the class.

| You get | Means                                                                                                          |
| ------- | -------------------------------------------------------------------------------------------------------------- |
| **200** | success — and **only** success (`error` is `null`). An answer with an `error` is never `200`.                  |
| **400** | your request was bad (`request` type: `MALFORMED_REQUEST`, `SCRIPT_XOR_KEY`)                                   |
| **404** | the `key` you asked for isn't registered (`SCRIPT_NOT_FOUND`)                                                  |
| **409** | operator misconfig a retry can't fix (`AUTH_REQUEST`, `S3_FORBIDDEN`, `RESOURCE_KIND_MISMATCH`)                |
| **413** | your script or context is over the size limit (`SCRIPT_TOO_LARGE` / `CONTEXT_TOO_LARGE`)                       |
| **422** | can't process it and a retry won't help (typo, no `handler`, memory/op cap, a thrown script error, your park)  |
| **500** | the robot itself broke (`INTERNAL`, rare!) — retry after the `Retry-After`                                     |
| **503** | transient — a tool is down, at capacity, over quota, or a retryable timeout — retry after the `Retry-After`    |

**No `429`.** Rate-limit/quota/capacity conditions are all `503`: `429` has a `4xx`
*digit* but retry *meaning*, so a one-digit worker would wrongly park it. The `Retry-After`
header (seconds) rides every `503`/`500` and carries the *when* — a short backoff for
capacity, a longer one seeded from a circuit-breaker cool-down.

## Try-again-later (`retryable`) 🔁

A database deadlock or a network blip is **`retryable: true`** — waiting and retrying
might work, so it's a **`5xx`** (`503`, or `500` for the robot's own `INTERNAL`) with a
`Retry-After`. A bad query or a constraint violation is **`retryable: false`** — retrying
fails the same way, so it's a **`4xx`** (park). The status line and this field never
disagree.

A wall-clock **`TIMEOUT`** is the one ambiguous case (a slow dependency vs a slow
algorithm — the robot can't tell), so the operator sets `timeout_retryable` (default
`true` → `503`; `false` → `422`). `MEMORY_LIMIT` and the op-count cap are deterministic —
the same input hits them every time — so they stay **non-retryable (`422`)** no matter
what.

## Catching tool errors yourself

`db`, `mail`, `s3`, `redis`, and `amq` **throw** when they fail. If you don't catch one it
becomes the answer, and its `retryable` decides the status (a retryable `DB_DEADLOCK` →
`503`, a permanent `DB_CONSTRAINT` → `4xx`). Or `try/catch` and turn it into your own
friendly answer — now it's *your* error (add `retryable: true` to opt into `503`):

```js
function handler(ctx) {
  try {
    db.execute("INSERT INTO users(email) VALUES($1)", [ctx.email]);
    return json({ ok: true }, null);
  } catch (e) {
    // this is YOUR error now — it passes through exactly as you write it
    return json(null, { message: "could not save user", detail: e.message });
  }
}
```

`http` is the one exception: it **never throws**. A failed request comes back as data
(`{ status: 0, error: { ... } }`), so you just check `res.status`.

## The decoder tables 🗂️

Want to handle specific cases? Switch on `code`. Here's every code, by tool.

### Your request (`type: "request"`)

| `code`              | retry | owner  | When                                                                |
| ------------------- | ----- | ------ | ------------------------------------------------------------------- |
| `SCRIPT_TOO_LARGE`  | no    | caller | Script bigger than `max_script_size` (413).                         |
| `CONTEXT_TOO_LARGE` | no    | caller | Context bigger than `max_context_size` (413).                       |
| `SCRIPT_XOR_KEY`    | no    | caller | Request has both `script` and `key`, or neither — send exactly one (400). |
| `SCRIPT_NOT_FOUND`  | no    | caller | The `key` isn't in the server's script registry (404).              |
| `MALFORMED_REQUEST` | no    | caller | Body isn't valid JSON, has wrong field types, or is too large (400). |
| `RESOURCE_NOT_FOUND` | no   | caller | A `config.io` nickname the operator never set up (or not for your tenant) (400). |
| `RESOURCE_KIND_MISMATCH` | no | operator | The nickname exists but is a different kind (asked for `db`, it's `redis`). Operator misconfig ⇒ 409. |

### The engine (`type: "runtime"`)

| `code`                 | retry | owner     | When                                                                                           |
| ---------------------- | ----- | --------- | ---------------------------------------------------------------------------------------------- |
| `SYNTAX_ERROR`         | no    | developer | The script didn't parse.                                                                       |
| `MODULE_NOT_FOUND`     | no    | developer | An ES-module handler `import`ed a specifier that isn't a registered module.                    |
| `HANDLER_NOT_DEFINED`  | no    | developer | No `handler(ctx)` function.                                                                    |
| `TIMEOUT`              | cfg   | developer | Ran past the time limit. `retryable` follows `timeout_retryable` (default `true` ⇒ 503; `false` ⇒ 422). |
| `MEMORY_LIMIT`         | no    | developer | The context was too big to load into the memory limit (422, always — deterministic).           |
| `MALFORMED_RESPONSE`   | no    | developer | Returned something that isn't a `json(...)` answer (422).                                      |
| `OVERLOADED`           | yes   | operator  | Server at capacity (bulkhead full) — back off, retry (503 + `Retry-After`, never 429).         |
| `PARTITION_OVERLOADED` | yes   | caller    | This partition key hit its concurrency share (per-partition fairness) — retry (503, never 429). |
| `QUOTA_EXCEEDED`       | yes   | caller    | Tenant over its plan's in-flight cap — retry as executions free up (503, never 429).           |
| `EGRESS_UNAVAILABLE`   | yes   | operator  | The request named a driver resource but the egress sidecar (`fabricd`) isn't configured or reachable (503).    |
| `EGRESS_PROTOCOL`      | no    | operator  | The egress sidecar spoke the protocol wrong — operator misconfig (409).                        |
| `INTERNAL`             | yes   | operator  | The robot's own fault (rare) — a 500 with `Retry-After`.                                        |

### Your script (`type: "script"`)

| `code`         | retry | owner     | When                                                                            |
| -------------- | ----- | --------- | ------------------------------------------------------------------------------- |
| `SCRIPT_ERROR` | no    | developer | Your code threw an error (or hit a bug). `message` is your error's text. Parks at 422 (an uncaught throw is never a `200`). |

### Tools (`type: "capability"`)

A tool failure that reaches the top of the request (you didn't `try/catch` it) now
**projects onto the status line** like any other error: a retryable code (`DB_DEADLOCK`,
`REDIS_TIMEOUT`, …) → `503` + `Retry-After`; a permanent one (`DB_CONSTRAINT`,
`S3_FORBIDDEN`, …) → `4xx`. (This changed from the old behaviour, where every tool error
was `200` — a retry worker couldn't see the difference.) The `retry`/`owner` columns below
are exactly what drives that status.

**`db`** (from the database's `SqlState`):

| `code`             | retry | owner     | When                                                                                                                                 |
| ------------------ | ----- | --------- | ------------------------------------------------------------------------------------------------------------------------------------ |
| `DB_SERIALIZATION` | yes   | operator  | Serialization failure — retry the transaction.                                                                                       |
| `DB_DEADLOCK`      | yes   | operator  | Deadlock — retry.                                                                                                                    |
| `DB_CONNECTION`    | yes   | operator  | Couldn't reach the database (drop, or can't connect).                                                                                |
| `DB_CANCELED`      | yes   | operator  | Query canceled / server-side statement timeout.                                                                                      |
| `DB_TIMEOUT`       | yes   | operator  | Query exceeded the client-side execution deadline (freed the thread even when the server-side timeout was lost through a pooler).    |
| `DB_CIRCUIT_OPEN`  | yes   | operator  | Circuit breaker is open — the database keeps failing to connect, so jsbox fast-failed instead of waiting. Retry after the cool-down. |
| `DB_CONSTRAINT`    | no    | developer | Broke a rule (unique/foreign-key/etc). `details.sqlstate`.                                                                           |
| `DB_QUERY`         | no    | developer | Bad SQL.                                                                                                                             |
| `DB_OP_LIMIT`      | no    | developer | Hit `max_ops`.                                                                                                                       |
| `DB_ERROR`         | yes   | operator  | Anything else (fallback).                                                                                                            |

**`mail`** (from the SMTP reply):

| `code`           | retry | owner     | When                                         |
| ---------------- | ----- | --------- | -------------------------------------------- |
| `MAIL_TRANSIENT` | yes   | operator  | 4xx reply (greylisting, mailbox busy).       |
| `MAIL_PERMANENT` | no    | developer | 5xx reply (rejected, bad address).           |
| `MAIL_OP_LIMIT`  | no    | developer | Hit `max_ops`.                               |
| `MAIL_ERROR`     | yes   | operator  | Anything else, incl. connect/TLS (fallback). |

**`s3`:**

| `code`         | retry | owner     | When                                                                        |
| -------------- | ----- | --------- | --------------------------------------------------------------------------- |
| `S3_UPSTREAM`  | yes   | operator  | Store errored or was unreachable (`usage`/`delete`). `details.http_status`. |
| `S3_OP_LIMIT`  | no    | developer | Hit `max_ops` while listing.                                                |
| `S3_FORBIDDEN` | no    | operator  | `delete` without `config.s3.allow_delete`. Operator misconfig ⇒ 409.        |
| `S3_ERROR`     | no    | developer | Bad key/config / signing (fallback).                                        |

**`api`** (returned **in-band** as `{ status: 0, error }`, never thrown):

| `code`                | retry | owner     | When                            |
| --------------------- | ----- | --------- | ------------------------------- |
| `HTTP_TIMEOUT`        | yes   | operator  | Request timed out.              |
| `HTTP_CONNECT`        | yes   | operator  | TCP/TLS/DNS connect failure.    |
| `HTTP_SSRF_BLOCKED`   | no    | developer | URL/host wasn't allowed.        |
| `HTTP_BODY_TOO_LARGE` | no    | developer | Response was over the size cap. |
| `HTTP_OP_LIMIT`       | no    | developer | Hit `max_ops`.                  |
| `HTTP_ERROR`          | yes   | operator  | Anything else (fallback).       |

**`redis`:**

| `code`             | retry | owner     | When                                  |
| ------------------ | ----- | --------- | ------------------------------------- |
| `REDIS_CONNECTION` | yes   | operator  | Couldn't reach Redis (or it dropped). |
| `REDIS_TIMEOUT`    | yes   | operator  | A command timed out.                  |
| `REDIS_OP_LIMIT`   | no    | developer | Hit `max_ops`.                        |
| `REDIS_ERROR`      | yes   | operator  | Anything else (fallback).             |

**`amq`** (RabbitMQ producer):

| `code`                | retry | owner     | When                                      |
| --------------------- | ----- | --------- | ----------------------------------------- |
| `AMQ_CONNECTION`      | yes   | operator  | Couldn't reach the broker.                |
| `AMQ_BATCH_TOO_LARGE` | no    | developer | Batch bigger than the amq resource's `max_batch`. |
| `AMQ_OP_LIMIT`        | no    | developer | Hit `max_ops`.                            |
| `AMQ_ERROR`           | yes   | operator  | Publish/protocol error (fallback).        |

**`auth`** (OIDC/IAM identity). An invalid token is **not** an error — it comes back
**in-band** as `{ ok: false, status, code: "AUTH_INVALID_TOKEN" }` (like `http`, never
thrown). These codes are only for the failures `auth` **throws**:

| `code`             | retry | owner     | When                                                                          |
| ------------------ | ----- | --------- | ----------------------------------------------------------------------------- |
| `AUTH_UNAVAILABLE` | yes   | operator  | Identity server unreachable / 5xx / timeout. `details.http_status`.           |
| `AUTH_REQUEST`     | no    | operator  | Misconfig: bad endpoint, discovery failed, `introspect` without client creds. Operator misconfig ⇒ 409. |
| `AUTH_OP_LIMIT`    | no    | developer | Hit `max_ops`.                                                                |

> New codes can show up over time, but they **never change meaning** and never move to a
> different `type` — so it's always safe to switch on `code`.

## Batches read the body, not the status 📦

`POST /batch` is the exception to the traffic-light rule. An admitted batch always returns
**`200`**, because its items each have their own outcome and can't share one status line.
Every item carries its own `{ data, error, meta }` envelope inside `results[]`, so a batch
consumer is an **envelope-reader** by construction: branch on each item's `error` +
`retryable`, not on the batch's HTTP status. (Batch-level rejections — bad auth, a
malformed body, over the item cap — are still non-`200`, since the whole request failed.)

## The receipt number — `trace_id` 🧾

Every answer (good or bad) carries a **`meta.trace_id`**. When something goes wrong, the
robot also writes that id to its own logs **with the full error**. So if you hit a
problem, give the operator the `trace_id` and they can find the exact details — even the
parts hidden from you.

## The extra-detail switch — `error_debug` 🔍

By default the `debug` box (a stack trace + the raw error text) is **hidden** — secure by
default, since the raw text can name internal hosts. On a purely internal service the
operator can set `error_debug: true` to get that detail inline. Either way `code`,
`owner`, `details`, and the safe `message` are always there, so programs still get what
they need. The raw text is never lost: it's always in the server logs under the `trace_id`.

**Next:** [Back to the guide →](README.md)
