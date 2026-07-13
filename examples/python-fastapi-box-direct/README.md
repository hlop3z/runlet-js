# Box-direct egress in Python (FastAPI)

**Your `io` backend can be anything.** This example is a plain FastAPI service that the box
POSTs to over loopback whenever a script calls `$std.io.call("<name>", …)`. No Rust, no
driver, no broker — just an HTTP endpoint that speaks a tiny envelope.

```
 ┌─────────────┐   $std.io.call("cache","get",{key})   ┌──────────┐   POST /cache          ┌───────────────┐
 │  JS handler │ ───────────────────────────────────▶ │   box    │ ─────────────────────▶ │ FastAPI (app.py) │
 │  (sandbox)  │ ◀─────────────────────────────────── │ (runlet) │ ◀───────────────────── │  in-memory KV    │
 └─────────────┘        {"value": "hello world"}       └──────────┘   {"value":"hello …"}  └───────────────┘
                                                        the script only ever sees the *name* "cache";
                                                        the URL lives in the box config, never the script.
```

## The contract (all of it)

The box POSTs to the exact URL you bind in `local_resources[name].url`:

```
POST /cache
Content-Type: application/json
X-Runlet-Tenant: <tenant>    # only in trusted-header mode
X-Runlet-Actor:  <subject>   # only in trusted-header mode

{"action": "get", "payload": "{\"key\":\"greeting\"}"}
```

- `payload` is **double-encoded** — a JSON *string* that itself contains the script's JSON args.
- **2xx** → the response body is handed back to the script *verbatim* (no wrapper).
- **non-2xx** → the box throws a retryable `IO_LOCAL_HTTP` error into the script.

That's the whole thing. See [`app.py`](app.py) for the ~80-line implementation.

## Run it

```bash
# 1. start the backend
pip install -r requirements.txt
python app.py                      # serves on 127.0.0.1:8090

# 2. start the box with these box-direct bindings. The box reads ./config.json from its
#    working directory, so run it from this example folder (build once, then run the binary):
cargo build -p runlet
cd examples/python-fastapi-box-direct && ../../target/debug/runlet

# 3. call it
curl -s localhost:3000/execute -H 'content-type: application/json' -d '{
  "script": "function handler(ctx){ $std.io.call(\"cache\",\"set\",{key:\"greeting\",value:\"hi \"+ctx.name}); var c=$std.io.call(\"cache\",\"get\",{key:\"greeting\"}); return json(c.value,null); }",
  "context": { "name": "Ada" },
  "config": { "io": ["cache", "orders"] }
}'
# => {"data":"hi Ada","error":null,"meta":{ ... "io":{"cache":[ ... ]} ... }}
```

`config.io` is the per-request **allowlist** — a name must appear here *and* be bound in the
box config, or the call is rejected before any egress. The box meters every call under
`meta.io.<name>`.

The same script + same envelope also works against a **broker** (`fabricd`, or the
[Go QUIC broker](../go-quic-broker) next door) — move the name from `local_resources` to a
broker binding and the script does not change. That is the design payoff: box-direct and
broker are the same wire.
