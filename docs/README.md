# jsbox — Friendly Guide 📦

Welcome! This is the easy guide to using **jsbox**. No scary words. Promise.

## What is jsbox? (the 10-second version)

Imagine a little **robot in a box** 🤖. You hand the robot:

1. a **note** that says what to do (a small JavaScript function), and
2. some **stuff to work with** (your data).

The robot does the work _inside the box_ (so it can never make a mess on your
computer), and hands you back an **answer**. That's it!

You talk to the robot by sending a message to `POST /execute`. To send **many jobs at
once** — each answered on its own, one bad job never spoils the others — use `POST /batch`.
A batch can also do one **setup** step first (`before` — e.g. fetch a rubric everyone shares)
and one **wrap-up** step at the end (`after` — e.g. add up all the scores into one `summary`).
See the [Batch execution](../README.md#batch-execution) reference for the details.

## Start here 👇

Read these in order. Each one is short.

1. **[Getting Started](01-getting-started.md)** — your very first script, and the
   shape of every answer.
2. **[`http` — talk to the internet](02-api.md)** — fetch data from other websites.
3. **[Build your own capability](03-capabilities.md)** — reach a database, cache, queue,
   mail relay, or any service through the one primitive `$std.io.call(name, action, payload)`.
   The three extension paths + the box-direct local shortcut. 🔌
4. **[`$` — Money & Exact Numbers](05-decimal.md)** — currency-safe money with `$("19.99",
   "USD")` (tax, penny-safe splits) plus `Decimal` for exact non-money math. Always on. 💵
5. **[`s3` — signed upload/download links](06-s3.md)** — let a browser upload files
   straight to your bucket (S3, R2, MinIO…). 🔗
6. **[`$std` — the built-in toolbox](09-sys.md)** — hashing, signing, and
   use-but-never-see secrets. Always on (no setup) for `crypto`. 🧰
7. **[`datetime` — dates & times done right](10-datetime.md)** — parse, calendar math,
   period boundaries, and **timezone-correct** answers. Always on. 📅
8. **[Hasura — GraphQL the easy way](11-hasura.md)** — the `hasura/client` module:
   query Hasura with one line and never miss a hidden GraphQL error. 🚀
9. **[`text` — tidy strings the easy way](12-text.md)** — clean, pad, slug, and mask
   strings with Python-style names (`lower`, `strip`, `zfill`, `slugify`). Always on. ✂️
10. **[`list` & `dict` — tidy tables and records](13-lists-and-dicts.md)** — filter, sort,
   group, and sum by field name, no functions needed (`where`, `sort_by`, `group_by`, `sum`,
   `get`). Always on. 📋
11. **[When Things Go Wrong (Errors)](99-errors.md)** — what the robot hands back when
   something fails, and how to read it. 🚦

## The super-powers 🦸

Your robot starts with **no** super-powers. You turn each one on by adding a
little `config` to your message. That keeps things safe.

There are **three** built-in super-powers:

| Super-power | What it does                          | Turn it on with          |
| ----------- | ------------------------------------- | ------------------------ |
| `http`      | Talk to other websites                | `config.allowed_hosts`   |
| `s3`        | Signed upload/download links          | `config.s3`              |
| `io`        | Talk to a named service or database   | `config.io: ["nickname"]` |

Anything else — a database, a cache, a queue, a mail relay, your own service — you reach
through the **one primitive** `$std.io.call("nickname", action, payload)` after listing the
nickname in `config.io`. `config.io` is a **flat list of nicknames** (`["orders", "cache"]`).
Each nickname points at a resource the grown-up (operator) set up — either a **co-located
local service** bound in the box's config, or one held by a little key-keeper helper
(**`fabricd`**) that runs next to jsbox and does the actual connecting. The keys and passwords
live there, never in your request and never in the robot's box. See
**[Build your own capability](03-capabilities.md)**.

(`$` / `$std.money` — currency-safe money — `$std.decimal` — exact numbers — `$std.datetime` — dates &
times — and **`$std.crypto`** are the exceptions: they're **always on**, no config. Only
`$std.env` / `$std.secrets` need `config.sys`.)

If you don't turn a super-power on, the robot simply doesn't have it. (For example, with no
`allowed_hosts`, `http` is `undefined` — it isn't there at all.)

## Going further 🛠️

For builders and operators (a bit more advanced):

- **[Authoring modules](modules.md)** — write reusable `import`able helper libraries
  (with npm/esbuild) that your handlers can share.
- **[Deployment & hardening](deployment.md)** — the production checklist: what to set
  before you point real traffic at it, and why.
- **Behavioral contract (specs)** — the testable "what the system guarantees" lives in
  [`openspec/specs/`](../openspec/specs/) (capabilities, execution, resilience, registries,
  observability). Browse with `openspec list --specs` / `openspec show <name>`.
- **Design notes (rationale)** — the architecture deep-dives, the "why": [resilience](design/resilience.md)
  (timeouts, bulkheads, circuit breaker), [resource egress](design/resource-egress.md)
  (the three built-ins, the `fabricd` broker, box-direct local egress, and why credentials
  never enter the box), [network fabric](design/network-fabric.md) (remote `fabricd` over QUIC),
  [multitenant trust](design/multitenant-trust.md) (trusted-identity mode),
  [pooled capabilities](design/pooled-capabilities.md) (PgBouncer),
  [script registry](design/script-registry.md), and
  [injectable modules](design/injectable-modules.md).
