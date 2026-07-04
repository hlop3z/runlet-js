#!/usr/bin/env python3
"""A/B stress harness for jsbox resilience (docs/design/resilience.md).

Drives concurrent load against `/execute` under a degraded-DB scenario (a slow query
through PgBouncer) and compares two server configurations:

  A (baseline)  â€” bulkhead effectively off, no statement_timeout ceiling.
  B (resilient) â€” Tier 1 bulkhead + Tier 0 statement_timeout clamp enabled.

Hypotheses (resilience.md): under overload B sheds excess as fast 429s and holds tail
latency + stays responsive, while A queues on the DB and tail latency climbs.

The harness manages the server lifecycle itself (one variant at a time), so the only
variable is the config. It is NOT pass/fail â€” it prints a comparison and a verdict.

The `db` capability is brokered by `fabricd` now, so each variant runs the two-process
topology: `fabricd` (the db resource + the Tier 0 `max_statement_timeout_ms` ceiling) over
a UDS + the box (bulkhead/fairness knobs).

Env knobs (all optional):
  RUNLET_BIN/FABRICD_BIN   binary paths (default: cargo build once, run from target/debug)
  PGBOUNCER_HOST/PGBOUNCER_PORT   db target (default localhost:6432)
  STRESS_CONCURRENCY  concurrent workers (default 40)
  STRESS_DURATION     load seconds per variant (default 8)
  STRESS_SLEEP        pg_sleep seconds per request (default 2)
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
PGB_HOST = os.environ.get("PGBOUNCER_HOST", "localhost")
PGB_PORT = int(os.environ.get("PGBOUNCER_PORT", "6432"))
CONCURRENCY = int(os.environ.get("STRESS_CONCURRENCY", "40"))
DURATION = float(os.environ.get("STRESS_DURATION", "8"))
SLEEP_S = float(os.environ.get("STRESS_SLEEP", "2"))

# The db operator resource â€” lives in fabricd's `resources` table; the requests only name
# it via `config.io`. `statement_timeout_ms: 0` asks for "unlimited" so the Tier 0 ceiling
# (fabricd's `max_statement_timeout_ms`, the B variant) is what bounds the slow query.
DB_RESOURCE = {
    "kind": "db",
    "host": PGB_HOST,
    "port": PGB_PORT,
    "user": "test",
    "password": "test",
    "database": "testdb",
    "statement_timeout_ms": 0,
}
DB_IO = {"io": {"db": ["stress-db"]}}

# The load: a slow DB query (simulates a degraded database / saturated pool). Each
# request holds a connection for SLEEP_S server-side.
# The flood is tagged partition "noisy"; the victim "good" â€” so with Tier 5 enabled
# (variant B) the noisy partition sheds on its own cap and the good one keeps its share.
WORK_BODY = {
    "script": f"function handler(ctx) {{ db.query('SELECT pg_sleep({SLEEP_S})'); return json('ok', null); }}",
    "config": DB_IO,
    "partition": "noisy",
}
TRIVIAL_BODY = {"script": "function handler(ctx) { return json(1, null); }"}
# A well-behaved partition's normal, fast query â€” interleaved during the overload to
# measure noisy-neighbor impact (does the slow-query flood drag down a good request?).
VICTIM_BODY = {
    "script": "function handler(ctx) { var r = db.query('SELECT 1 AS ok'); return json(r.rows[0].ok, null); }",
    "config": DB_IO,
    "partition": "good",
}


# -- HTTP ---------------------------------------------------------------------


def _post_timed(body: dict, timeout: float = 30.0):
    """POST /execute, returning (latency_s, http_status, error_code|None)."""
    data = json.dumps(body).encode()
    req = urllib.request.Request(
        f"{BASE_URL}/execute", data=data, headers={"Content-Type": "application/json"}
    )
    start = time.time()
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            body_json = json.loads(resp.read())
            return time.time() - start, resp.getcode(), _code(body_json)
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


# -- Server lifecycle ---------------------------------------------------------


def _wait_for_server(up: bool, tries: int = 40) -> bool:
    for _ in range(tries):
        _, status, _ = _post_timed(TRIVIAL_BODY, timeout=2)
        if (status != 0) == up:
            return True
        time.sleep(0.25)
    return False


def _binaries() -> tuple:
    """Resolve the (runlet, fabricd) binary paths, building both once when not given via env
    (two concurrent `cargo run`s would race on the target/ build lock)."""
    repo = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    if not (RUNLET_BIN and FABRICD_BIN):
        subprocess.run(
            ["cargo", "build", "--quiet", "-p", "runlet", "-p", "fabricd"],
            cwd=repo, check=True,
        )
    bindir = os.path.join(repo, "target", "debug")
    return (
        RUNLET_BIN or os.path.join(bindir, "runlet"),
        FABRICD_BIN or os.path.join(bindir, "fabricd"),
    )


def start_stack(engine_overrides: dict, ceiling_ms: int) -> list:
    """Start `fabricd` (the db resource + the Tier 0 ceiling) over a UDS, then the box with
    the given engine overrides pointed at that socket. Returns the started processes."""
    repo = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    run_dir = os.path.join(repo, ".stress-run")
    os.makedirs(run_dir, exist_ok=True)
    runlet_bin, fabricd_bin = _binaries()
    socket = os.path.join(run_dir, "fabricd.sock")
    if os.path.exists(socket):
        os.remove(socket)
    fabricd_cfg = {
        "socket": socket,
        "max_statement_timeout_ms": ceiling_ms,
        "resources": {"stress-db": DB_RESOURCE},
    }
    with open(os.path.join(run_dir, "fabricd.json"), "w", encoding="utf-8") as fh:
        json.dump(fabricd_cfg, fh)
    box_cfg = {
        "debug": True,
        "server": {"host": "127.0.0.1", "port": 3000},
        "engine": engine_overrides,
        "fabricd_socket": socket,
    }
    with open(os.path.join(run_dir, "config.json"), "w", encoding="utf-8") as fh:
        json.dump(box_cfg, fh)
    procs = [subprocess.Popen(
        [fabricd_bin], cwd=run_dir,
        env={**os.environ, "FABRICD_CONFIG": os.path.join(run_dir, "fabricd.json")},
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
    )]
    for _ in range(40):  # wait for fabricd to bind before starting the box
        if os.path.exists(socket):
            break
        time.sleep(0.25)
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


# -- Load + stats -------------------------------------------------------------


def percentile(sorted_vals, pct: float) -> float:
    if not sorted_vals:
        return 0.0
    idx = min(len(sorted_vals) - 1, int(pct / 100.0 * len(sorted_vals)))
    return sorted_vals[idx]


def run_load(concurrency: int, duration: float) -> dict:
    """Fire `concurrency` workers looping the slow query for `duration` seconds, while a
    single victim thread interleaves a normal fast query to measure noisy-neighbor impact.
    """
    deadline = time.time() + duration
    results = []  # (latency, status, code) â€” the slow-query flood

    def worker():
        local = []
        while time.time() < deadline:
            local.append(_post_timed(WORK_BODY))
        return local

    victim = []  # (latency, status, code) â€” the well-behaved partition under the flood

    def victim_prober():
        # Let the flood ramp first, then probe every ~250ms.
        time.sleep(0.5)
        while time.time() < deadline:
            victim.append(_post_timed(VICTIM_BODY, timeout=15))
            time.sleep(0.25)

    with concurrent.futures.ThreadPoolExecutor(max_workers=concurrency + 1) as ex:
        futs = [ex.submit(worker) for _ in range(concurrency)]
        probe_fut = ex.submit(victim_prober)
        for fut in futs:
            results.extend(fut.result())
        probe_fut.result()

    return _summarize(results, victim, duration)


def _summarize(results, victim, duration: float) -> dict:
    lats = sorted(r[0] for r in results)
    codes = {}
    for _, status, code in results:
        key = code or ("OK" if status == 200 else f"HTTP_{status}")
        codes[key] = codes.get(key, 0) + 1
    shed = codes.get("OVERLOADED", 0)
    total = len(results)
    vic_lats = sorted(r[0] for r in victim)
    vic_ok = sum(1 for _, status, code in victim if status == 200 and code is None)
    vic_shed = sum(1 for _, _, code in victim if code == "OVERLOADED")
    return {
        "total": total,
        "throughput": total / duration if duration else 0,
        "useful": total - shed,
        "p50": percentile(lats, 50),
        "p95": percentile(lats, 95),
        "p99": percentile(lats, 99),
        "max": lats[-1] if lats else 0,
        "shed_429": shed,
        "shed_pct": (100.0 * shed / total) if total else 0,
        "codes": codes,
        "vic_n": len(victim),
        "vic_ok": vic_ok,
        "vic_shed": vic_shed,
        "vic_p50": percentile(vic_lats, 50),
        "vic_p99": percentile(vic_lats, 99),
    }


# -- Report -------------------------------------------------------------------


def _row(label, a, b, fmt):
    print(f"  {label:<22} {fmt(a):>14} {fmt(b):>14}")


def report(a: dict, b: dict):
    secs = lambda v: f"{v:.2f}s"
    num = lambda v: f"{v:.0f}"
    pct = lambda v: f"{v:.0f}%"
    print("\n" + "=" * 54)
    print(
        f"  A/B stress â€” {CONCURRENCY} concurrent, {DURATION:.0f}s, pg_sleep({SLEEP_S:.0f}) via PgBouncer"
    )
    print("=" * 54)
    print(f"  {'metric':<22} {'A baseline':>14} {'B resilient':>14}")
    print("  " + "-" * 50)
    _row("requests", a["total"], b["total"], num)
    _row("flood latency p50", a["p50"], b["p50"], secs)
    _row("flood latency p99", a["p99"], b["p99"], secs)
    _row("flood latency max", a["max"], b["max"], secs)
    _row("shed as 429", a["shed_pct"], b["shed_pct"], pct)
    print("  " + "-" * 50)
    print("  good-request (victim) under the flood:")
    _row("victim latency p50", a["vic_p50"], b["vic_p50"], secs)
    _row("victim latency p99", a["vic_p99"], b["vic_p99"], secs)
    _row("victim succeeded", a["vic_ok"], b["vic_ok"], lambda v: f"{v}")
    _row("victim shed (429)", a["vic_shed"], b["vic_shed"], lambda v: f"{v}")
    print("\n  flood code breakdown:")
    print(f"    A: {a['codes']}")
    print(f"    B: {b['codes']}")
    print("\n  verdict:")
    if a["p99"] > 0:
        print(
            f"    Tail latency  A p99 {a['p99']:.2f}s  ->  B p99 {b['p99']:.2f}s "
            f"({a['p99'] / max(b['p99'], 1e-3):.0f}x lower under overload)"
        )
    if b["shed_429"] > 0 and a["shed_429"] == 0:
        print(
            f"    Tier 1 âœ“  B fails fast â€” sheds {b['shed_pct']:.0f}% as 429s; A queues (none shed)"
        )
    print(
        f"    Noisy neighbor  victim (partition 'good')  A succeeded {a['vic_ok']}  vs  B succeeded {b['vic_ok']}"
    )
    if b["vic_ok"] > 0:
        print(
            f"    Tier 5 âœ“  the good partition keeps its share â€” {b['vic_ok']} victim requests got"
        )
        print(
            f"              through under the flood (A: {a['vic_ok']}, dragged to p99 {a['vic_p99']:.2f}s)."
        )
    elif b["vic_shed"] > 0:
        print(
            "    Tier 5 gap: the good partition was shed too â€” is max_concurrent_per_partition set?"
        )
    print()


# -- Main ---------------------------------------------------------------------


def experiment(label: str, engine: dict, ceiling_ms: int) -> dict:
    print(f"  [{label}] starting stack: engine={engine} max_statement_timeout_ms={ceiling_ms}")
    procs = start_stack(engine, ceiling_ms)
    try:
        time.sleep(1)  # settle
        return run_load(CONCURRENCY, DURATION)
    finally:
        stop_stack(procs)


def main():
    if _post_timed(TRIVIAL_BODY, timeout=2)[1] != 0:
        print(
            "ERROR: a server is already on :3000 â€” stop it; this harness manages its own."
        )
        raise SystemExit(1)
    # A: bulkhead effectively off, no statement_timeout ceiling.
    a = experiment("A baseline", {"max_concurrent_executions": 1000}, 0)
    # B: Tier 1 bulkhead + Tier 5 fairness (box) + Tier 0 clamp (fabricd).
    b = experiment(
        "B resilient",
        {"max_concurrent_executions": 8, "max_concurrent_per_partition": 4},
        1000,
    )
    report(a, b)


if __name__ == "__main__":
    main()
