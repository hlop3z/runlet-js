# Build your own capability over `$std.io.call` 🔌

The box ships **three built-in super-powers**: `http` (talk to the internet), `s3` (signed
upload/download links), and `io` (talk to a named service). Everything else — a database, a cache,
a mail relay, a queue, your own service — you reach through **one tiny primitive**:

```js
$std.io.call(name, action, payload)
```

- `name` — a nickname **you** picked for a resource (e.g. `"orders"`, `"cache"`), listed in
  `config.io`. The box never sees a real host or password — just the nickname.
- `action` — a verb your service understands (e.g. `"query"`, `"get"`, `"send"`).
- `payload` — any JSON your service needs.

It returns whatever your service answers (parsed JSON), or **throws** a tagged error if the call
fails — the same shape every built-in uses (see [Errors](99-errors.md)).

```js
function handler(ctx) {
  const rows = $std.io.call("orders", "query", { sql: "SELECT * FROM orders WHERE id = $1", params: [ctx.id] });
  return json(rows, null);
}
```

You allow a nickname per request:

```json
{ "config": { "io": ["orders", "cache"] } }
```

`config.io` is a **flat list of nicknames** — a plain allowlist. A script that calls a nickname
**not** in the list is stopped with `RESOURCE_NOT_FOUND` before anything happens. The box is
"kind-blind": it does not know or care whether `"orders"` is Postgres or Mongo — that is decided
on the other side.

## Where does a nickname actually go? — the three paths

Pick by how much isolation you need. Less setup on the left, more on the right.

```
 (a) http → a local service     (b) your own binary        (c) io → a broker
     plain http, allowlisted        a Rust CapabilityDef       the box forwards the
     you run the service            + your driver/creds        nickname; the broker
     no wire protocol               in your own process        holds ALL the creds
```

### (a) Reach a service over `http`

If you already run a small service (yours, or an off-the-shelf one), just call it with the `http`
built-in. To reach a **co-located** service like `localhost:8000` **safely in production**, add it
to `config.allowed_hosts` as `host:port`:

```json
{ "config": { "allowed_hosts": ["localhost:8000"] } }
```

An explicit `host:port` entry is the one production-safe way to reach a local address — it lifts
the private-IP block **for that exact target only**. (The blanket `debug` switch also lifts it, but
for *everything* — keep that for local development, never production.) Every other guard still
applies: the host must be allowlisted, and redirects are re-checked.

### (b) Compile your own capability into your own box

`runlet` is a library. Build your own binary that composes a `CapabilityDef` (a JS wrapper + a
trust declaration + an in-process `Egress` backend). Your driver and your credentials live in
**your** process; `$std.io.call("<your-name>", …)` is serviced in-process with no broker. See
[the composable-core design](design/composable-core.md).

### (c) Route `io` to a broker (the box holds nothing)

Point the box at a **broker** (the reference `fabricd` image, or anything that speaks the wire
protocol). The box forwards only the nickname over a local socket (UDS) or a network link (QUIC);
the broker resolves it to a real host + credentials and does the work. This is the multi-tenant
path: **the box holds no remote endpoint and no credential.**

Run the reference broker beside the box and put your resource table in *its* config; the box only
ever knows nicknames.

## Box-direct: a local service without a broker or Rust

Between (a) and (c) there is a shortcut. An operator can bind a nickname **directly** to a
co-located loopback service in the box's **global config** — no broker, no Rust:

```json
{
  "local_resources": {
    "pricing": { "url": "http://localhost:8080" },
    "cache":   { "url": "http://localhost:9000" }
  }
}
```

Now `$std.io.call("pricing", action, payload)` POSTs the **same `{action, payload}` envelope** a broker
would receive, straight to `http://localhost:8080`. The script is unchanged whether `"pricing"`
resolves box-direct or later moves to a broker — the nickname is a stable pointer.

Rules (they keep the box safe):

- **Operator-only.** Bindings live in the box's global config — never in a request, never anything
  a script can influence.
- **Loopback/private only.** A box-direct target must be co-located; the box **refuses to start**
  if you point one at a public address. A remote target must go through a broker.

## The one envelope

Whether a nickname resolves box-direct or through a broker, the call carries the identical
`{action, payload}` shape. That is what makes a nickname a stable indirection: a service can move
from `localhost` to a broker to another host with **zero** script changes.

---

Method names on any capability are `snake_case` (`$std.io.call("orders", "find_one", …)`). Values that
don't fit a JS number exactly (big integers, decimals) come back as **strings** — use the always-on
[`$` / Decimal](05-decimal.md) helper for exact math.

**Next:** [`$` — Exact Decimal Math →](05-decimal.md)
