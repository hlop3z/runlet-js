"""A box-direct egress backend, in ~80 lines of Python.

This is a *co-located loopback service* the box POSTs to when a script calls
`$std.io.call("<name>", action, payload)` and that name is bound box-direct in the box's
`local_resources` config. It demonstrates the whole point of `io`: **the backend can be
anything** — any language, any framework — as long as it speaks the tiny HTTP contract below.

--------------------------------------------------------------------------------------------
The box-direct HTTP contract (see crates/runlet/src/local_io.rs)
--------------------------------------------------------------------------------------------
Request  (box -> this service):
    POST <the exact url from local_resources[name].url>   (no path is appended)
    Content-Type: application/json
    X-Runlet-Tenant: <tenant>   (only in trusted-header mode; omitted otherwise)
    X-Runlet-Actor:  <subject>  (only in trusted-header mode; omitted otherwise)
    body: {"action": "<verb>", "payload": "<the script's args as a JSON *string*>"}
          note: `payload` is DOUBLE-ENCODED — a JSON string that itself contains JSON.

Response (this service -> box):
    2xx      -> the raw response body is handed VERBATIM back to the script (no wrapper).
                Return whatever JSON value the calling script expects.
    non-2xx  -> the box synthesizes a retryable `IO_LOCAL_HTTP` error thrown to the script;
                your body is discarded. Use this to signal a hard failure.

The logical name (`cache`, `orders`, …) is NEVER in the body — it is only the config key that
maps a name to a URL. To serve several names from one process, give each its own path (below)
and point each `local_resources[name].url` at the matching path.
"""

from __future__ import annotations

import json
from typing import Annotated

from fastapi import FastAPI, Header, HTTPException, Request

app = FastAPI(title="runlet box-direct example")

# Trivial in-memory state so the demo actually *does* something. Namespaced by the trusted
# tenant header when present, so you can see identity flow through box-direct egress.
_STORE: dict[str, dict] = {}
_ORDERS: dict[str, list] = {}


def _bucket(store: dict, tenant: str | None):
    """Return the per-tenant slice of a store (single-tenant falls back to a shared bucket)."""
    return store.setdefault(tenant or "_shared", {} if store is _STORE else [])


async def _envelope(request: Request) -> tuple[str, dict]:
    """Parse the {action, payload} envelope. `payload` is a JSON string -> decode it once."""
    body = await request.json()
    action = body.get("action")
    payload = json.loads(body.get("payload") or "null")
    if not action:
        raise HTTPException(status_code=400, detail="missing action")
    return action, (payload or {})


# --- logical name "cache" -> bound to http://127.0.0.1:8090/cache -------------------------
@app.post("/cache")
async def cache(
    request: Request,
    x_runlet_tenant: Annotated[str | None, Header()] = None,
    x_runlet_actor: Annotated[str | None, Header()] = None,
):
    action, payload = await _envelope(request)
    bucket = _bucket(_STORE, x_runlet_tenant)
    if action == "set":
        bucket[payload["key"]] = payload["value"]
        return {"ok": True}
    if action == "get":
        return {"value": bucket.get(payload["key"])}
    if action == "delete":
        return {"deleted": bucket.pop(payload["key"], None) is not None}
    # A non-2xx becomes an IO_LOCAL_HTTP error thrown into the script.
    raise HTTPException(status_code=400, detail=f"unknown cache action: {action}")


# --- logical name "orders" -> bound to http://127.0.0.1:8090/orders -----------------------
@app.post("/orders")
async def orders(
    request: Request,
    x_runlet_tenant: Annotated[str | None, Header()] = None,
    x_runlet_actor: Annotated[str | None, Header()] = None,
):
    action, payload = await _envelope(request)
    log = _bucket(_ORDERS, x_runlet_tenant)
    if action == "insert":
        row = {"id": len(log) + 1, "by": x_runlet_actor, **payload}
        log.append(row)
        return row
    if action == "list":
        return {"orders": log}
    raise HTTPException(status_code=400, detail=f"unknown orders action: {action}")


if __name__ == "__main__":
    import uvicorn

    uvicorn.run(app, host="127.0.0.1", port=8090)
