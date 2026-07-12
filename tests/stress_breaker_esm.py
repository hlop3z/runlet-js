#!/usr/bin/env python3
"""Stress harness for the newer jsbox features â€” Tier 3 circuit breaker and ES modules.

Companion to `stress_test.py` (which covers Tier 0/1/5). Two independent experiments,
each managing its own server so the only variable is the thing under test:

  1. Circuit breaker (Tier 3) â€” A/B against a DEAD, hanging database. The breaker is a
     *connect* breaker, so its value shows when connects time out (5s each), not when
     they are refused instantly. Target 192.0.2.1:5432 (RFC-5737 TEST-NET) black-holes
     the TCP SYN, so every connect pays the full 5s connect-timeout. The breaker lives in
     `fabricd` now (it owns the driver connections), so each variant runs the two-process
     topology: `fabricd` (dead-db resource + `db_breaker_threshold`) over a UDS + the box.
       A (off):  db_breaker_threshold = 0  â€” every request waits ~5s on connect.
       B (on):   db_breaker_threshold = 3  â€” after 3 fails the breaker opens and the
                 rest fast-fail DB_CIRCUIT_OPEN in ~ms.
     Hypothesis: B has far higher throughput and far lower tail latency under a dead DB,
     and stops burning spawn_blocking threads on the connect timeout.

  2. ES-module overhead â€” per-request latency of three handler shapes on one normally
     configured server (no `fabricd` â€” deterministic handlers need no egress): a classic
     script, an `export default` module, and a module that `import`s a registry module.
     Quantifies the cost of module compile/eval per request.

Run it INSIDE the dev container (it needs the binaries + a UDS-capable filesystem):
  docker exec -e RUNLET_BIN=/ctarget/debug/runlet -e FABRICD_BIN=/ctarget/debug/fabricd \
      -e DEAD_DB_HOST=192.0.2.1 jsbox-dev sh -c "cd /src && python3 tests/stress_breaker_esm.py"

Env knobs (optional): RUNLET_BIN, FABRICD_BIN, DEAD_DB_HOST/PORT, BREAKER_CONCURRENCY,
BREAKER_DURATION, ESM_REQUESTS, ESM_CONCURRENCY, MODULES_DIR. NOT pass/fail â€” prints a
comparison + verdict.
"""

import concurrent.futures
import json
import os
import subprocess
import time
import urllib.error
import urllib.request

BASE_URL = os.environ.get("RUNLET_URL", "http://127.0.0.1:3000")
RUNLET_BIN = os.environ.get("RUNLET_BIN", "")
FABRICD_BIN = os.environ.get("FABRICD_BIN", "")
DEAD_HOST = os.environ.get("DEAD_DB_HOST", "192.0.2.1")
DEAD_PORT = int(os.environ.get("DEAD_DB_PORT", "5432"))
BREAKER_CONCURRENCY = int(os.environ.get("BREAKER_CONCURRENCY", "16"))
BREAKER_DURATION = float(os.environ.get("BREAKER_DURATION", "10"))
ESM_REQUESTS = int(os.environ.get("ESM_REQUESTS", "500"))
ESM_CONCURRENCY = int(os.environ.get("ESM_CONCURRENCY", "8"))
MODULES_DIR = os.environ.get(
    "MODULES_DIR",
    os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "tests", "modules"),
)

# The dead-db operator resource â€” lives in fabricd's `resources` table; the request only
# names it via `config.io`.
DEAD_DB = {
    "kind": "db",
    "host": DEAD_HOST,
    "port": DEAD_PORT,
    "user": "x",
    "password": "x",
    "database": "x",
    "statement_timeout_ms": 0,
}
DEAD_QUERY = {
    "script": "function handler(ctx){ db.query('SELECT 1'); return json('ok', null); }",
    "config": {"io": {"db": ["dead-db"]}},
}
TRIVIAL = {"script": "function handler(ctx){ return json(1, null); }"}


# -- HTTP ---------------------------------------------------------------------


def _post_timed(body: dict, timeout: float = 30.0):
    """POST /execute -> (latency_s, http_status, error_code|None)."""
    data = json.dumps(body).encode()
    req = urllib.request.Request(
        f"{BASE_URL}/execute", data=data, headers={"Content-Type": "application/json"}
    )
    start = time.time()
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            return time.time() - start, resp.getcode(), _code(json.loads(resp.read()))
    except urllib.error.HTTPError as err:
        try:
            return time.time() - start, err.code, _code(json.loads(err.read()))
        except Exception:
            return time.time() - start, err.code, "NON_JSON"
    except Exception:
        return time.time() - start, 0, "NO_RESPONSE"


def _code(body_json: dict):
    err = body_json.get("error") if isinstance(body_json, dict) else None
    return err.get("code") if isinstance(err, dict) else None


def percentile(sorted_vals, pct: float) -> float:
    if not sorted_vals:
        return 0.0
    idx = min(len(sorted_vals) - 1, int(pct / 100.0 * len(sorted_vals)))
    return sorted_vals[idx]


# -- Server lifecycle ---------------------------------------------------------


def _wait_for_server(up: bool, tries: int = 40) -> bool:
    for _ in range(tries):
        _, status, _ = _post_timed(TRIVIAL, timeout=2)
        if (status != 0) == up:
            return True
        time.sleep(0.25)
    return False


def _binaries() -> tuple:
    """Resolve the (runlet, fabricd) binary paths. `runlet` builds in this repo; `fabricd`
    lives in its own repo (github.com/hlop3z/fabricd) â€” pass FABRICD_BIN, or keep a sibling
    checkout at ../fabricd (built here once when needed). The fabricd slot is None when
    neither is available (fine for the deterministic-only variant, which never uses it)."""
    repo = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    exe = ".exe" if os.name == "nt" else ""
    if not RUNLET_BIN:
        subprocess.run(["cargo", "build", "--quiet", "-p", "runlet"], cwd=repo, check=True)
    fabricd_bin = FABRICD_BIN
    if not fabricd_bin:
        sibling = os.path.join(os.path.dirname(repo), "fabricd")
        if os.path.isdir(os.path.join(sibling, "crates", "fabricd")):
            subprocess.run(
                ["cargo", "build", "--quiet", "-p", "fabricd"], cwd=sibling, check=True)
            fabricd_bin = os.path.join(sibling, "target", "debug", "fabricd" + exe)
    return (
        RUNLET_BIN or os.path.join(repo, "target", "debug", "runlet" + exe),
        fabricd_bin,
    )


def start_stack(box_config: dict, fabricd_config: dict | None) -> list:
    """Start `fabricd` (when the experiment needs db egress) over a UDS, then the box pointed
    at that socket. Returns the started processes (stop with `stop_stack`)."""
    repo = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    run_dir = os.path.join(repo, ".stress-run")
    os.makedirs(run_dir, exist_ok=True)
    runlet_bin, fabricd_bin = _binaries()
    procs = []
    if fabricd_config is not None:
        if fabricd_bin is None:
            raise SystemExit(
                "fabricd binary not found: set FABRICD_BIN or clone "
                "github.com/hlop3z/fabricd as a sibling of this repo")
        socket = os.path.join(run_dir, "fabricd.sock")
        if os.path.exists(socket):
            os.remove(socket)
        with open(os.path.join(run_dir, "fabricd.json"), "w", encoding="utf-8") as fh:
            json.dump({"socket": socket, **fabricd_config}, fh)
        procs.append(subprocess.Popen(
            [fabricd_bin], cwd=run_dir,
            env={**os.environ, "FABRICD_CONFIG": os.path.join(run_dir, "fabricd.json")},
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
        ))
        for _ in range(40):  # wait for fabricd to bind before starting the box
            if os.path.exists(socket):
                break
            time.sleep(0.25)
        box_config = {**box_config, "broker_socket": socket}
    with open(os.path.join(run_dir, "config.json"), "w", encoding="utf-8") as fh:
        json.dump(box_config, fh)
    procs.append(subprocess.Popen(
        [runlet_bin], cwd=run_dir, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL
    ))
    if not _wait_for_server(up=True):
        stop_stack(procs)
        raise RuntimeError("server failed to start")
    return procs


def stop_stack(procs: list):
    for proc in reversed(procs):  # box first, then fabricd
        proc.terminate()
    for proc in reversed(procs):
        try:
            proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            proc.kill()
    _wait_for_server(up=False)


# -- Experiment 1: circuit breaker A/B ----------------------------------------


def _run_dead_db_load(concurrency: int, duration: float) -> dict:
    deadline = time.time() + duration
    results = []

    def worker():
        local = []
        while time.time() < deadline:
            local.append(_post_timed(DEAD_QUERY, timeout=30))
        return local

    with concurrent.futures.ThreadPoolExecutor(max_workers=concurrency) as ex:
        for fut in [ex.submit(worker) for _ in range(concurrency)]:
            results.extend(fut.result())

    lats = sorted(r[0] for r in results)
    codes = {}
    for _, status, code in results:
        key = code or ("OK" if status == 200 else f"HTTP_{status}")
        codes[key] = codes.get(key, 0) + 1
    return {
        "total": len(results),
        "throughput": len(results) / duration if duration else 0,
        "p50": percentile(lats, 50),
        "p99": percentile(lats, 99),
        "max": lats[-1] if lats else 0,
        "fast": sum(1 for v in lats if v < 0.5),
        "codes": codes,
    }


def breaker_experiment(label: str, threshold: int) -> dict:
    # Bulkhead == concurrency so NO request is shed as 429 â€” every request acquires a permit
    # and the only variable is the connect path (slow dead-connect vs fast breaker-open). A
    # low bulkhead would flood the metrics with instant 429s and mask the breaker.
    box = {
        "debug": True,
        "server": {"host": "127.0.0.1", "port": 3000},
        "engine": {"max_concurrent_executions": BREAKER_CONCURRENCY},
    }
    # The breaker knob is fabricd config now â€” it owns the driver connections.
    fabricd_cfg = {
        "db_breaker_threshold": threshold,
        "resources": {"dead-db": DEAD_DB},
    }
    print(f"  [{label}] starting (db_breaker_threshold={threshold}) ...")
    procs = start_stack(box, fabricd_cfg)
    try:
        time.sleep(1)
        return _run_dead_db_load(BREAKER_CONCURRENCY, BREAKER_DURATION)
    finally:
        stop_stack(procs)


def report_breaker(a: dict, b: dict):
    secs = lambda v: f"{v:.2f}s"
    print("\n" + "=" * 56)
    print(
        f"  Tier 3 circuit breaker â€” dead DB {DEAD_HOST}:{DEAD_PORT} (connect hangs ~5s)"
    )
    print(f"  {BREAKER_CONCURRENCY} concurrent, {BREAKER_DURATION:.0f}s")
    print("=" * 56)
    print(f"  {'metric':<22} {'A breaker off':>14} {'B breaker on':>14}")
    print("  " + "-" * 52)
    print(f"  {'requests served':<22} {a['total']:>14} {b['total']:>14}")
    print(
        f"  {'throughput (req/s)':<22} {a['throughput']:>14.1f} {b['throughput']:>14.1f}"
    )
    print(f"  {'latency p50':<22} {secs(a['p50']):>14} {secs(b['p50']):>14}")
    print(f"  {'latency p99':<22} {secs(a['p99']):>14} {secs(b['p99']):>14}")
    print(f"  {'latency max':<22} {secs(a['max']):>14} {secs(b['max']):>14}")
    print(f"  {'fast-failed (<0.5s)':<22} {a['fast']:>14} {b['fast']:>14}")
    print("\n  code breakdown:")
    print(f"    A: {a['codes']}")
    print(f"    B: {b['codes']}")
    print("\n  verdict:")
    if a["throughput"] > 0:
        print(
            f"    Throughput  A {a['throughput']:.1f}/s  ->  B {b['throughput']:.1f}/s "
            f"({b['throughput'] / max(a['throughput'], 1e-3):.0f}x higher under a dead DB)"
        )
    if a["p99"] > 0:
        print(
            f"    Tail p99    A {a['p99']:.2f}s  ->  B {b['p99']:.2f}s "
            f"({a['p99'] / max(b['p99'], 1e-3):.0f}x lower)"
        )
    if b["codes"].get("DB_CIRCUIT_OPEN", 0) > 0:
        print(
            f"    Tier 3 âœ“  B fast-failed {b['codes'].get('DB_CIRCUIT_OPEN', 0)} requests as "
            f"DB_CIRCUIT_OPEN instead of waiting ~5s on a dead connect."
        )
    print()


# -- Experiment 2: ESM overhead -----------------------------------------------

SHAPES = {
    "script (classic)": {"script": "function handler(ctx){ return json(1, null); }"},
    "esm export default": {
        "script": "export default function handler(ctx){ return json(1, null); }"
    },
    "esm + import": {
        "script": "import { quote } from 'acme/pricing';\n"
        "export default function handler(ctx){ return json(quote(1,1), null); }"
    },
}


def _bench_shape(body: dict, n: int, concurrency: int) -> dict:
    lats = []

    def one(_):
        lat, status, code = _post_timed(body, timeout=10)
        return lat if status == 200 and code is None else None

    with concurrent.futures.ThreadPoolExecutor(max_workers=concurrency) as ex:
        for r in ex.map(one, range(n)):
            if r is not None:
                lats.append(r)
    lats.sort()
    return {
        "ok": len(lats),
        "p50": percentile(lats, 50),
        "p99": percentile(lats, 99),
        "mean": sum(lats) / len(lats) if lats else 0,
    }


def esm_experiment() -> dict:
    config = {
        "debug": True,
        "server": {"host": "127.0.0.1", "port": 3000},
        "modules_dir": MODULES_DIR,
        "engine": {},
    }
    print(f"  [ESM] starting (modules_dir={MODULES_DIR}) ...")
    procs = start_stack(config, None)  # deterministic handlers â€” no fabricd needed
    out = {}
    try:
        time.sleep(1)
        # Measure sequentially (concurrency 1): isolates per-request eval cost from the
        # server-side scheduling/contention noise that buries it at higher concurrency.
        for name, body in SHAPES.items():
            _bench_shape(body, 50, 1)  # warm up
            out[name] = _bench_shape(body, ESM_REQUESTS, 1)
    finally:
        stop_stack(procs)
    return out


def report_esm(out: dict):
    us = lambda v: f"{v * 1e6:.0f}us"
    print("\n" + "=" * 56)
    print(
        f"  ES-module overhead â€” {ESM_REQUESTS} req/shape, {ESM_CONCURRENCY} concurrent"
    )
    print("=" * 56)
    print(f"  {'shape':<22} {'ok':>6} {'p50':>10} {'p99':>10} {'mean':>10}")
    print("  " + "-" * 52)
    for name, s in out.items():
        print(
            f"  {name:<22} {s['ok']:>6} {us(s['p50']):>10} {us(s['p99']):>10} {us(s['mean']):>10}"
        )
    base = out.get("script (classic)", {}).get("mean", 0)
    print("\n  verdict (mean overhead vs classic script):")
    for name, s in out.items():
        if name == "script (classic)":
            continue
        delta = (s["mean"] - base) * 1e6
        print(f"    {name:<22} +{delta:.0f}us/request")
    print()


# -- Main ---------------------------------------------------------------------


def main():
    if _post_timed(TRIVIAL, timeout=2)[1] != 0:
        print(
            "ERROR: a server is already on :3000 â€” stop it; this harness manages its own."
        )
        raise SystemExit(1)
    a = breaker_experiment("A breaker off", 0)
    b = breaker_experiment("B breaker on", 3)
    report_breaker(a, b)
    report_esm(esm_experiment())


if __name__ == "__main__":
    main()
