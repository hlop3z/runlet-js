# A broker over QUIC, in Go

**Your broker can be anything too.** `fabricd` is the reference egress broker, but the box
depends only on the wire it owns (`crates/runlet-wire`), never on `fabricd` itself. Anything
that speaks that wire can stand in. This is a ~350-line Go program that does — it holds the
credentials the box does not, and resolves logical names (`cache`, `orders`) to backends.

```
 ┌─────────────┐  $std.io.call("cache","get",…)  ┌──────────┐   QUIC (ALPN fabricd/1)   ┌────────────────┐
 │  JS handler │ ─────────────────────────────▶ │   box    │ ═════════════════════════▶ │ Go broker (main.go) │
 │  (sandbox)  │ ◀───────────────────────────── │ (runlet) │ ◀═════════════════════════ │  resolves name→backend │
 └─────────────┘                                 └──────────┘   length-prefixed JSON     └────────────────┘
                                     box holds NO creds  ·  broker holds all creds  ·  cert pinned by SHA-256
```

## What the wire is (all of it)

Everything a broker must implement lives in [`crates/runlet-wire`](../../crates/runlet-wire):

| Concern    | Contract |
|------------|----------|
| Transport  | QUIC + TLS 1.3, **ALPN `fabricd/1`**, one **bidi stream per box request** |
| Trust      | No CA — the box **pins the server cert by SHA-256(DER)**; no mutual TLS |
| Auth       | The box presents an opaque `token` inside the `Init` frame; the broker checks it |
| Framing    | each frame = **u32 little-endian length** + that many bytes of **JSON** |
| Session    | `Init` (once) → `Ack`\|`InitError`, then N × (`Call` → `Reply`), then `Drain` → `Metrics` |
| Encoding   | serde **externally-tagged** enums: `{"Init":{…}}`, `{"Call":{…}}`, `{"Reply":{"Ok":"<json string>"}}`; unit variants are bare strings (`"Drain"`, `"Ack"`) |

`payload` inside `Call` is **double-encoded** — a JSON string containing the script's JSON
args. `Reply`'s `Ok` is likewise a JSON string handed back to the script verbatim.
See [`main.go`](main.go) — each wire type is a commented Go struct next to its Rust source.

## Run it

```bash
# 1. build + start the broker. It self-signs a cert on boot and prints a config block.
go mod tidy
go run .
#   ...prints:  server_cert_pin : 3f9a…   and a ready-to-paste "broker_quic": { … }

# 2. paste server_cert_pin (and confirm auth_token) into config.json, then start the box
#    from this folder (the box reads ./config.json from its working directory):
cargo build -p runlet     # from the repo root
cd examples/go-quic-broker && ../../target/debug/runlet

# 3. call it — same script, same names as the box-direct example
curl -s localhost:3000/execute -H 'content-type: application/json' -d '{
  "script": "function handler(ctx){ $std.io.call(\"cache\",\"set\",{key:\"g\",value:\"hi \"+ctx.name}); return json($std.io.call(\"cache\",\"get\",{key:\"g\"}).value, null); }",
  "context": { "name": "Ada" },
  "config": { "io": ["cache", "orders"] }
}'
# => {"data":"hi Ada","error":null,"meta":{ … }}
```

## Notes

- **Cert pinning is the trust root.** The broker prints `SHA-256(DER of the leaf cert)`; the
  box trusts *exactly* that cert (`broker_quic.server_cert_pin`). Rotate the cert → repin. A
  wrong pin fails the QUIC handshake, so the box never talks to an impostor broker.
- **The box holds no credentials.** It forwards only the logical names in `config.io`
  (`WireInit.resources`) plus the trusted `tenant`/`actor`. This broker maps `cache`/`orders`
  to in-memory backends; a real one resolves name → kind → endpoint → credentials from its own
  operator config. Swap the two `backend` funcs in `main.go` for real drivers and you are done.
- **Errors** flow back as an `EgressError` in `Reply.Err` (`{code, message, source, retryable,
  owner}`) and surface to the script as a thrown capability error, exactly like `fabricd`.
- **quic-go version:** pinned to the v0.48.x API (`quic.Connection` / `quic.Stream`). A newer
  quic-go renamed some of these types; if `go mod tidy` pulls a newer major, either keep the
  pin or adjust the two type names in `serveConn`.
