# Examples

The box ships **three built-in primitives** (`http`, `s3`, `io`) and lets you bring your own
for everything else. `io` is the operator-named logical-egress primitive: a script calls
`$std.io.call("<name>", action, payload)` and the box routes that name to a backend it does
**not** need to understand. The box is *kind-blind* and holds no credentials — so **your `io`
backend can be anything, in any language.** These examples show that spectrum, least → most
isolation.

| Example | Language | Path | Shows |
|---------|----------|------|-------|
| [`python-fastapi-box-direct`](python-fastapi-box-direct) | Python (FastAPI) | **box-direct** — box POSTs `{action, payload}` to a co-located loopback HTTP service | the simplest possible backend: any HTTP server |
| [`go-quic-broker`](go-quic-broker) | Go (quic-go) | **broker** — a `fabricd` stand-in over the `runlet-wire` QUIC protocol | a network broker that holds the creds the box does not |
| [`kv-capability`](kv-capability) | Rust | **in-process** `CapabilityDef` compiled into your own binary | embedding a driver directly, no broker |
| [`tauri-embed`](tauri-embed) | Rust | embedding the `LogicHost` in a desktop app | running the engine outside an HTTP server |

## The key idea: box-direct and broker are the *same wire*

The `{action, payload}` envelope a box POSTs box-direct is identical to the `Call` it sends a
broker. So the **script never changes** when a resource moves between the two — only the box
config does:

```jsonc
// box-direct: name -> a co-located loopback URL (box holds nothing, no broker)
"local_resources": { "cache": { "url": "http://127.0.0.1:8090/cache" } }

// broker: the same name, resolved remotely (broker holds kind + endpoint + credentials)
"broker_quic": { "replicas": ["broker:4443"], "server_cert_pin": "…", "auth_token": "…" }
```

Run the **Python** and **Go** examples back to back with the identical handler and identical
`config.io: ["cache", "orders"]` — the script cannot tell which one served it. That is the
whole point: the sandbox depends on a nickname, and the operator decides what stands behind it.

The contract each path implements is owned by this repo:

- **box-direct HTTP** — `crates/runlet/src/local_io.rs`
- **broker wire (QUIC + framing + messages)** — `crates/runlet-wire/src/{wire.rs,quic.rs}`

See also the fork-me capability guide: [`docs/03-capabilities.md`](../docs/03-capabilities.md).
