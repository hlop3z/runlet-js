#!/usr/bin/env python3
"""Integration tests for jsbox."""

import json
import os
import shutil
import subprocess
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import urllib.error
import urllib.request
from urllib.parse import urlparse

BASE_URL = os.environ.get("JSBOX_URL", "http://127.0.0.1:3000")

# Local httpbin clone (`httpbin` service in docker-compose) â€” the HTTP `api` tests run
# against it so the suite never depends on httpbin.org uptime. go-httpbin echoes
# headers/args as ARRAYS of strings, hence the [0] indexing in assertions. Reaching a
# localhost/LAN target needs the server started with `debug: true` (SSRF private-IP
# block) â€” the harness-generated config sets it.
HTTPBIN_URL = os.environ.get("HTTPBIN_URL", "http://localhost:8095").rstrip("/")
HTTPBIN_HOST = urlparse(HTTPBIN_URL).hostname or "localhost"

# -- Test runner -------------------------------------------------------------

class Runner:
    """Minimal test runner with pass/fail tracking."""

    def __init__(self):
        self.passed = 0
        self.failed = 0

    @property
    def total(self):
        return self.passed + self.failed

    def section(self, title: str):
        print(f"\n\033[1m  {title}\033[0m\n")

    def test(self, name: str, body: dict, check):
        resp = _post(body)
        try:
            assert resp is not None, "no response"
            assert check(resp), "assertion failed"
            self.passed += 1
            print(f"  \033[32mPASS\033[0m {name}")
        except Exception as exc:
            self.failed += 1
            print(f"  \033[31mFAIL\033[0m {name}")
            print(f"       {exc}")
            if resp:
                print(f"       {json.dumps(resp)}")

    def check(self, name: str, ok: bool):
        """Record a boolean assertion (for tests that post outside the default BASE_URL,
        e.g. a dedicated trusted-mode box, where `test()` can't be used)."""
        if ok:
            self.passed += 1
            print(f"  \033[32mPASS\033[0m {name}")
        else:
            self.failed += 1
            print(f"  \033[31mFAIL\033[0m {name}")

    def summary(self):
        print("\n" + "-" * 36)
        if self.failed == 0:
            print(f"  \033[32mOK\033[0m {self.passed}/{self.total} tests passed")
        else:
            print(f"  \033[31mFAIL\033[0m {self.passed} passed, {self.failed} failed out of {self.total}")
        print()


# -- HTTP helpers ------------------------------------------------------------

def _post(body: dict, headers: dict | None = None) -> dict | None:
    data = json.dumps(body).encode()
    hdrs = {"Content-Type": "application/json"}
    if headers:
        hdrs.update(headers)
    req = urllib.request.Request(f"{BASE_URL}/execute", data=data, headers=hdrs)
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            return _parse_response(resp.getcode(), resp.read())
    except urllib.error.HTTPError as err:
        return _parse_response(err.code, err.read())
    except Exception:
        return None


def _post_status(url: str, body: dict, headers: dict | None = None):
    """POST to an explicit URL, returning `(http_status, parsed_envelope)`. Unlike `_post`
    (which targets BASE_URL and hides the status), this keeps the status so a caller can assert
    on the code â€” used by the trusted-mode box which runs on its own port."""
    data = json.dumps(body).encode()
    hdrs = {"Content-Type": "application/json"}
    if headers:
        hdrs.update(headers)
    req = urllib.request.Request(url, data=data, headers=hdrs)
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            return resp.getcode(), _parse_response(resp.getcode(), resp.read())
    except urllib.error.HTTPError as err:
        return err.code, _parse_response(err.code, err.read())
    except Exception:
        return None, None


def _post_full(body: dict, headers: dict | None = None):
    """POST /execute returning `(http_status, parsed_envelope, response_headers)`. Unlike `_post`
    (which hides the status), this keeps the status line and headers so the status-projection tests
    can assert `4xx`/`5xx` routing and the `Retry-After` header. `response_headers` is the
    case-insensitive message object (use `.get("Retry-After")`)."""
    data = json.dumps(body).encode()
    hdrs = {"Content-Type": "application/json"}
    if headers:
        hdrs.update(headers)
    req = urllib.request.Request(f"{BASE_URL}/execute", data=data, headers=hdrs)
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            return resp.getcode(), _parse_response(resp.getcode(), resp.read()), resp.headers
    except urllib.error.HTTPError as err:
        return err.code, _parse_response(err.code, err.read()), err.headers
    except Exception:
        return None, None, None


def _post_batch(items: list, headers: dict | None = None) -> dict | None:
    """POST a list of items to `/batch`; returns the parsed body — `{results, meta}` on an
    admitted batch, or a batch-level `{data, error, meta}` envelope on a 400 (empty/caps/malformed).
    Hides the HTTP status like `_post` (partial failure is a 200 with per-item errors)."""
    data = json.dumps({"items": items}).encode()
    hdrs = {"Content-Type": "application/json"}
    if headers:
        hdrs.update(headers)
    req = urllib.request.Request(f"{BASE_URL}/batch", data=data, headers=hdrs)
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            return _parse_response(resp.getcode(), resp.read())
    except urllib.error.HTTPError as err:
        return _parse_response(err.code, err.read())
    except Exception:
        return None


def _post_batch_full(body: dict, headers: dict | None = None) -> dict | None:
    """POST a full `/batch` body (may carry `before`/`shared`/`after` alongside `items`); returns the
    parsed body. Hides the HTTP status like `_post_batch`."""
    data = json.dumps(body).encode()
    hdrs = {"Content-Type": "application/json"}
    if headers:
        hdrs.update(headers)
    req = urllib.request.Request(f"{BASE_URL}/batch", data=data, headers=hdrs)
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            return _parse_response(resp.getcode(), resp.read())
    except urllib.error.HTTPError as err:
        return _parse_response(err.code, err.read())
    except Exception:
        return None


def _parse_response(status: int, raw: bytes) -> dict:
    """Parse a server response. A well-formed jsbox response is the JSON envelope; a
    non-JSON body (e.g. axum's plain-text deserialize rejection) is surfaced as a
    sentinel so tests can assert on the contract gap instead of crashing."""
    try:
        return json.loads(raw)
    except Exception:
        return {"_http_status": status, "_non_json_body": raw.decode("utf-8", "replace")}


def _get_text(path: str) -> tuple[int, str] | None:
    """GET a plain-text endpoint (e.g. /metrics). Returns (status, body) or None."""
    req = urllib.request.Request(f"{BASE_URL}{path}")
    try:
        with urllib.request.urlopen(req, timeout=10) as resp:
            return resp.getcode(), resp.read().decode("utf-8", "replace")
    except urllib.error.HTTPError as err:
        return err.code, err.read().decode("utf-8", "replace")
    except Exception:
        return None


# -- Script helpers ----------------------------------------------------------

def h(body: str, ctx=None, config=None) -> dict:
    """Build a request body from a handler function body."""
    req = {"script": f"function handler(ctx) {{ {body} }}"}
    if ctx is not None:
        req["context"] = ctx
    if config is not None:
        req["config"] = config
    return req


def h_raw(script: str, ctx=None, config=None) -> dict:
    """Build a request body from raw script source."""
    req = {"script": script}
    if ctx is not None:
        req["context"] = ctx
    if config is not None:
        req["config"] = config
    return req


# -- Assertion helpers -------------------------------------------------------

def data_eq(expected):
    """Assert data == expected and no error."""
    return lambda r: r["data"] == expected and r["error"] is None


def data_is_none():
    return lambda r: r["data"] is None


def has_error():
    return lambda r: r["error"] is not None


def error_contains(text: str):
    return lambda r: r["error"] is not None and text in str(r["error"])


def data_none_with_error():
    return lambda r: r["data"] is None and r["error"] is not None


def _err_code(r):
    """The system-error `code`, or None. Safe when `error` is null or absent
    (r.get('error', {}) returns None when the key exists with a null value)."""
    if not r:
        return None
    err = r.get("error")
    return err.get("code") if isinstance(err, dict) else None


# -- Test definitions --------------------------------------------------------

def test_functionality(t: Runner):
    t.section("Functionality")
    t.test("sum of two numbers",       h("return json(ctx.a + ctx.b, null);", {"a": 10, "b": 20}), data_eq(30))
    t.test("string result",            h('return json("hello " + ctx.name, null);', {"name": "Alice"}), data_eq("hello Alice"))
    t.test("object with map",          h("return json({items: ctx.list.map(function(x){return x*2}), count: ctx.list.length}, null);", {"list": [1, 2, 3]}),
           lambda r: r["data"]["items"] == [2, 4, 6] and r["data"]["count"] == 3)
    t.test("array result",             h("return json([1,2,3], null);"), data_eq([1, 2, 3]))
    t.test("null when no return",      h("json(null, null);"), data_is_none())
    t.test("boolean result",           h("return json(ctx.x > 5, null);", {"x": 10}), data_eq(True))
    t.test("nested context",           h("return json(ctx.user.name, null);", {"user": {"name": "Bob"}}), data_eq("Bob"))
    t.test("default empty context",    h("return json(Object.keys(ctx).length, null);"), data_eq(0))


def test_money(t: Runner):
    t.section("Money & Decimal value-utils")
    # Decimal — exact non-money math, snake_case surface, distinct from $.
    t.test("Decimal exact add",        h("return json($std.decimal('0.1').add('0.2').toString(), null);"), data_eq("0.3"))
    t.test("Decimal distinct from $",  h("return json($std.decimal !== $, null);"), data_eq(True))
    t.test("Decimal round half_even",  h("return json($std.decimal('2.5').round(0, 'half_even').toString(), null);"), data_eq("2"))
    t.test("Decimal round_to cash",    h("return json($std.decimal('2.03').round_to('0.05').toString(), null);"), data_eq("2.05"))
    t.test("Decimal clamp",            h("return json($std.decimal('120').clamp(0, 100).toString(), null);"), data_eq("100"))
    t.test("removed isZero alias gone", h("return json(typeof $std.decimal('0').isZero !== 'function', null);"), data_eq(True))
    # Money — an invoice with tax, end to end through /execute.
    t.test("invoice with tax",
           h("var gross = $('100.00','USD').add_pct(8.25); return json({total: gross.to_string(), cents: gross.to_minor(), fmt: gross.format()}, null);"),
           lambda r: r["data"]["total"] == "108.25" and r["data"]["cents"] == 10825 and r["data"]["fmt"] == "$108.25")
    t.test("money serializes self-describing",
           h("return json({price: $('19.99','USD')}, null);"),
           lambda r: r["data"]["price"] == {"amount": "19.99", "currency": "USD", "minor_units": 1999})
    t.test("JPY minor_units no x100",
           h("return json({y: $('1000','JPY')}, null);"),
           lambda r: r["data"]["y"]["minor_units"] == 1000)
    # A refund split — penny-safe allocation summing to the total exactly.
    t.test("refund split sums exactly",
           h("var p = $('100.00','USD').allocate_to(3).map(function(m){return m.to_string()}); return json(p, null);"),
           data_eq(["33.34", "33.33", "33.33"]))
    t.test("weighted split leftover cent",
           h("var p = $('0.05','USD').allocate([70,30]).map(function(m){return m.to_string()}); return json(p, null);"),
           data_eq(["0.04", "0.01"]))
    # div overload: money/scalar -> money; money/money -> a Decimal ratio.
    t.test("money div scalar -> money",
           h("return json($('99.00','USD').div(3).to_string(), null);"), data_eq("33.00"))
    t.test("money div money -> ratio",
           h("return json($('115.00','USD').div($('100.00','USD')).toString(), null);"), data_eq("1.15"))
    # Currency safety + cascade.
    t.test("cross-currency add throws",
           h("try { $('1','USD').add($('1','EUR')); return json(null, 'no throw'); } catch(e){ return json({caught:true}, null); }"),
           lambda r: r["data"] == {"caught": True})
    t.test("config.currency supplies currency",
           h("return json($('19.99'), null);", config={"currency": "EUR"}),
           lambda r: r["data"]["currency"] == "EUR" and r["data"]["minor_units"] == 1999)
    t.test("no currency resolvable throws",
           h("try { $('19.99'); return json(null, 'no throw'); } catch(e){ return json({caught:true}, null); }"),
           lambda r: r["data"] == {"caught": True})


def test_datetime(t: Runner):
    t.section("datetime value-util")
    # Parsing + components, resolved in UTC by default.
    t.test("parse + components",
           h("var d = $std.datetime.parse('2026-07-10T13:30:00Z'); return json({y:d.year(), mo:d.month(), da:d.day(), wd:d.weekday(), q:d.quarter()}, null);"),
           lambda r: r["data"] == {"y": 2026, "mo": 7, "da": 10, "wd": 5, "q": 3})
    # Immutability — add() returns a new value; the receiver is unchanged.
    t.test("immutable add",
           h("var d = $std.datetime.parse('2026-07-10T00:00:00Z'); var n = d.add({days:1}); return json({before:d.day(), after:n.day()}, null);"),
           lambda r: r["data"] == {"before": 10, "after": 11})
    # ISO serialization — a value renders as its RFC 3339 UTC (Z) string in json(...).
    t.test("serializes RFC 3339 UTC",
           h("return json({d: $std.datetime.parse('2026-07-10T13:30:00Z')}, null);"),
           lambda r: r["data"]["d"] == "2026-07-10T13:30:00Z")
    # Calendar month clamping — Jan 31 + 1 month -> Feb 28 (2026 non-leap).
    t.test("month add clamps end-of-month",
           h("var d = $std.datetime.from({year:2026,month:1,day:31}).add({months:1}); return json({mo:d.month(), da:d.day()}, null);"),
           lambda r: r["data"] == {"mo": 2, "da": 28})
    # Timezone view — the boundary is computed in Tokyo; the canonical instant is preserved.
    t.test("timezone boundary in zone",
           h("var d = $std.datetime.parse('2026-07-15T12:00:00Z'); var tk = d.in_zone('Asia/Tokyo'); return json({preserved: tk.epoch_ms()===d.epoch_ms(), hour: tk.hour(), start: tk.start_of('month').iso()}, null);"),
           lambda r: r["data"]["preserved"] is True and r["data"]["hour"] == 21 and r["data"]["start"] == "2026-07-01T00:00:00+09:00")
    t.test("unknown zone throws",
           h("try { $std.datetime.parse('2026-07-10T00:00:00Z').in_zone('Mars/Phobos'); return json(null,'no throw'); } catch(e){ return json({caught:true}, null); }"),
           lambda r: r["data"] == {"caught": True})


def test_template(t: Runner):
    t.section("template value-util")
    # HTML mode auto-escapes interpolated values; literal markup in the template is untouched.
    t.test("html mode escapes values",
           h("return json({out: $std.template.html('<p>{{ name }}</p>').render({name:'<b>&x'})}, null);"),
           lambda r: r["data"]["out"] == "<p>&lt;b&gt;&amp;x</p>")
    # Text mode emits values verbatim (plain email / SMS).
    t.test("text mode verbatim",
           h("return json({out: $std.template.text('Hi {{ name }}').render({name:'<b>&x'})}, null);"),
           lambda r: r["data"]["out"] == "Hi <b>&x")
    # Statements + expressions render (a real invoice-line loop).
    t.test("loop statement renders",
           h("return json({out: $std.template.text('{% for i in items %}{{ i }},{% endfor %}').render({items:[1,2,3]})}, null);"),
           lambda r: r["data"]["out"] == "1,2,3,")
    # Missing merge tags render empty by default; a placeholder substitutes when set.
    t.test("missing lenient + placeholder",
           h("var tpl = $std.template.text('A{{ gap }}B'); return json({empty: tpl.render({}), dash: tpl.missing('-').render({})}, null);"),
           lambda r: r["data"] == {"empty": "AB", "dash": "A-B"})
    # .fields() reports the top-level merge tags (sorted), for "what data does this need?".
    t.test("fields lists merge tags",
           h("return json({f: $std.template.text('{{ first }} {{ last }} {{ first }}').fields()}, null);"),
           lambda r: r["data"]["f"] == ["first", "last"])
    # A malformed template throws a catchable Error at construction (not a runtime crash).
    t.test("malformed template throws",
           h("try { $std.template.text('{{ unclosed '); return json(null,'no throw'); } catch(e){ return json({caught:true}, null); }"),
           lambda r: r["data"] == {"caught": True})


def test_list_interop(t: Runner):
    t.section("list value-util interop (money / datetime wrappers)")
    # A money column sums to a money value (currency preserved), not a bare Decimal, and never 0.
    t.test("sum of a money column returns money",
           h("var rows=[{price:$('0.10','USD')},{price:$('0.20','USD')}]; var s=$std.list(rows).sum('price'); return json({fmt:s.format(), cur:s.currency()}, null);"),
           lambda r: r["data"] == {"fmt": "$0.30", "cur": "USD"})
    # Mixing currencies in a summed column throws (no silent conversion).
    t.test("mixed-currency sum throws",
           h("try { $std.list([{p:$('1','USD')},{p:$('1','EUR')}]).sum('p'); return json(null,'no throw'); } catch(e){ return json({caught:true}, null); }"),
           lambda r: r["data"] == {"caught": True})
    # sort_by orders money numerically, not lexically ("100.00" would sort before "19.99" as a string).
    t.test("sort_by money is numeric not lexical",
           h("var rows=[{t:$('100.00','USD')},{t:$('19.99','USD')},{t:$('5.00','USD')}]; return json($std.list(rows).sort_by('t').column('t').to_array().map(function(m){return m.to_string();}), null);"),
           lambda r: r["data"] == ["5.00", "19.99", "100.00"])
    # group_by keeps currency distinct — USD 19.99 and EUR 19.99 are separate groups.
    t.test("group_by keeps currency distinct",
           h("return json($std.list([{p:$('19.99','USD')},{p:$('19.99','EUR')}]).group_by('p').keys().len(), null);"),
           data_eq(2))
    # unique dedupes equal money by amount+currency; a differing currency survives.
    t.test("unique dedupes equal money",
           h("return json($std.list([$('1','USD'),$('1','USD'),$('1','EUR')]).unique().len(), null);"),
           data_eq(2))
    # min/max over a money column return money values (currency kept).
    t.test("min/max over money return money",
           h("var rows=[{t:$('5','USD')},{t:$('2','USD')}]; var mn=$std.list(rows).min('t'); var mx=$std.list(rows).max('t'); return json({mn:mn.to_string()+' '+mn.currency(), mx:mx.to_string()+' '+mx.currency()}, null);"),
           lambda r: r["data"] == {"mn": "2 USD", "mx": "5 USD"})
    # datetime columns sort chronologically, not by string.
    t.test("sort_by datetime is chronological",
           h("var rows=[{d:$std.datetime.parse('2026-03-01T00:00:00Z')},{d:$std.datetime.parse('2026-01-15T00:00:00Z')},{d:$std.datetime.parse('2026-02-20T00:00:00Z')}]; return json($std.list(rows).sort_by('d').column('d').to_array().map(function(x){return x.month();}), null);"),
           lambda r: r["data"] == [1, 2, 3])


def test_user_errors(t: Runner):
    t.section("User-defined errors")
    t.test("push error messages",
           h('var e = {messages: []}; if (!ctx.name) e.messages.push("name required"); return json(null, e);'),
           lambda r: r["error"]["messages"][0] == "name required")
    t.test("custom error object",
           h('return json(null, {code: 400, detail: "bad input"});'),
           lambda r: r["error"]["code"] == 400 and r["error"]["detail"] == "bad input")
    t.test("data with warnings",
           h('return json({status: "ok"}, {warnings: ["low battery"]});'),
           lambda r: r["data"]["status"] == "ok" and r["error"]["warnings"][0] == "low battery")


def test_exceptions(t: Runner):
    t.section("Exception handling")
    t.test("throw returns error",      h('throw new Error("boom");'),       data_none_with_error())
    t.test("missing handler",          h_raw("var x = 1;"),                 has_error())
    t.test("syntax error",             h_raw("function handler(ctx { }"),   has_error())


def test_sandbox(t: Runner):
    t.section("Sandbox hardening")
    t.test("infinite loop times out",  h("while(true){}"),                              error_contains("timed out"))
    t.test("memory bomb stopped",      h("var a=[]; while(true) a.push(new Array(100000));"), has_error())
    t.test("eval() blocked",           h('return json(eval("1+1"), null);'),            data_none_with_error())
    t.test("deep recursion stopped",   h("function f(n){return f(n+1)} return json(f(0), null);"), data_none_with_error())


def test_json_bridge(t: Runner):
    t.section("json() bridge")
    t.test("data only",               h("return json(42);"),               data_eq(42))
    t.test("null data and error",      h("return json(null, null);"),       lambda r: r["data"] is None and r["error"] is None)


def test_meta(t: Runner):
    t.section("Meta")
    simple = h("return json(1, null);")
    t.test("has script_bytes",         simple, lambda r: r["meta"]["script_bytes"] > 0)
    t.test("has context_bytes",        h("return json(1, null);", {"a": 1}), lambda r: r["meta"]["context_bytes"] > 0)
    t.test("total = script + context", simple, lambda r: r["meta"]["total_input_bytes"] == r["meta"]["script_bytes"] + r["meta"]["context_bytes"])
    t.test("has exec_time_us",         simple, lambda r: r["meta"]["exec_time_us"] >= 0)


def test_status_projection(t: Runner):
    """The HTTP status line is a truthful projection of the outcome (docs/99-errors.md): `2xx`
    **iff** `error` is null, `retryable => 5xx` (+ `Retry-After`), non-retryable `=> 4xx`, and
    **never `429`**. Covers every path reachable without a `fabricd` sidecar; capability-error
    projection (a driver throw ⇒ `503`/`4xx`) is asserted in the driver sections, which self-skip
    when no sidecar is present."""
    t.section("HTTP status projection")

    def rget(hdrs, name):
        return hdrs.get(name) if hdrs is not None else None

    # 2xx is success and only success.
    st, r, hd = _post_full(h("return json(42, null);"))
    t.check("null-error success is 200", st == 200 and bool(r) and r.get("data") == 42)
    t.check("success carries no Retry-After", rget(hd, "Retry-After") is None)

    # Handler opt-in: retryable:true => 503 + Retry-After, body verbatim.
    st, r, hd = _post_full(h('return json(null, { message: "later", retryable: true });'))
    t.check("handler retryable:true => 503", st == 503)
    t.check("handler retryable:true carries Retry-After", rget(hd, "Retry-After") is not None)
    t.check("handler error body is verbatim",
            _err_code(r) is None and r["error"]["message"] == "later" and r["error"]["retryable"] is True)

    # Handler opt-in: retryable:false => 422 (park), body verbatim.
    st, r, hd = _post_full(h('return json(null, { message: "nope", retryable: false });'))
    t.check("handler retryable:false => 422", st == 422 and r["error"]["message"] == "nope")
    t.check("park carries no Retry-After", rget(hd, "Retry-After") is None)

    # Un-annotated handler error defaults to 422 (park), never 200; body unchanged.
    st, r, _ = _post_full(h('return json(null, { message: "name required" });'))
    t.check("un-annotated handler error => 422 (park, not 200)",
            st == 422 and st != 200 and r["error"]["message"] == "name required")

    # Non-retryable engine errors park at 4xx (previously an uncaught throw was 200).
    st, r, _ = _post_full(h_raw("function handler(ctx { }"))
    t.check("syntax error => 422", st == 422 and _err_code(r) == "SYNTAX_ERROR")
    st, r, _ = _post_full(h('throw new Error("boom");'))
    t.check("uncaught throw => 422 (not 200)", st == 422 and _err_code(r) == "SCRIPT_ERROR")
    st, r, _ = _post_full(h_raw("var x = 1;"))
    t.check("missing handler => 422", st == 422)

    # Oversize input is a caller fault that parks at 413 (not a generic 400).
    big_script = "function handler(ctx){ return json('" + ("x" * (2 * 1024 * 1024)) + "', null); }"
    st, r, _ = _post_full({"script": big_script})
    t.check("oversize script => 413", st == 413 and _err_code(r) == "SCRIPT_TOO_LARGE")
    st, r, _ = _post_full(h("return json(1, null);", ctx={"blob": "x" * (5 * 1024 * 1024)}))
    t.check("oversize context => 413", st == 413 and _err_code(r) == "CONTEXT_TOO_LARGE")

    # A wall-clock TIMEOUT follows timeout_retryable (default true => 503 + Retry-After).
    st, r, hd = _post_full(h("while(true){}"))
    t.check("timeout (default retryable) => 503", st == 503 and _err_code(r) == "TIMEOUT")
    t.check("timeout carries Retry-After", rget(hd, "Retry-After") is not None)

    # Status/envelope agreement across a representative sample: 5xx <=> retryable:true,
    # 4xx <=> retryable:false (system-error envelopes carry `retryable`).
    for label, body in [
        ("timeout", h("while(true){}")),
        ("syntax", h_raw("function handler(ctx { }")),
        ("no-handler", h_raw("var y = 1;")),
    ]:
        st, r, _ = _post_full(body)
        retry = bool((r.get("error") or {}).get("retryable")) if r else False
        t.check(f"status class agrees with envelope.retryable ({label})",
                st is not None and (st // 100 == 5) == retry)


def test_http_api(t: Runner):
    t.section("HTTP api")
    wildcard = {"allowed_hosts": ["*"]}
    httpbin  = {"allowed_hosts": [HTTPBIN_HOST]}
    blocked  = {"allowed_hosts": ["example.com"]}
    url = HTTPBIN_URL

    t.test("disabled when no config",
           h("return json(typeof $std.http, null);"),
           data_eq("undefined"))
    t.test("available with wildcard",
           h("return json(typeof $std.http, null);", config=wildcard),
           data_eq("object"))
    # A `*` wildcard host is intentionally INERT in SSRF-relaxed debug mode. The box runs with
    # debug:true so these api tests can reach the private-IP httpbin; under that relaxation `*`
    # would collapse the host allowlist down to the IP filter alone, so it is never honored
    # (host.rs: `allow_wildcard_hosts && !allow_private`). The request is blocked in-band â†’ the
    # private host is unreachable via `*` (status 0), even though a specific-host config reaches it.
    t.test("wildcard host inert under debug (private-IP relax) -> blocked",
           h(f"var r = $std.http.get('{url}/get', {{foo:'bar'}}); return json({{status:r.status}}, null);", config=wildcard),
           lambda r: r["data"]["status"] == 0)
    t.test("get with specific host",
           h(f"var r = $std.http.get('{url}/get'); return json(r.status, null);", config=httpbin),
           data_eq(200))
    t.test("get blocked by host",
           h(f"var r = $std.http.get('{url}/get'); return json(r, null);", config=blocked),
           lambda r: r["data"]["status"] == 0)
    t.test("post with body",
           h(f'var r = $std.http.post("{url}/post", {{hello:"world"}}); return json(r.status, null);', config=httpbin),
           data_eq(200))
    t.test("delete works",
           h(f"var r = $std.http.delete('{url}/delete'); return json(r.status, null);", config=httpbin),
           data_eq(200))

    # Headers (go-httpbin echoes header values as arrays of strings)
    t.test("get with auth header",
           h(f"var r = $std.http.get('{url}/get', null, {{'Authorization': 'Bearer test123'}}); return json(r.data.headers.Authorization[0], null);", config=httpbin),
           data_eq("Bearer test123"))
    t.test("post with custom header",
           h(f'var r = $std.http.post("{url}/post", {{a:1}}, {{"X-Custom": "foo"}}); return json(r.data.headers["X-Custom"][0], null);', config=httpbin),
           data_eq("foo"))
    t.test("content-type cannot be overridden",
           h(f'var r = $std.http.post("{url}/post", {{a:1}}, {{"Content-Type": "text/plain"}}); return json(r.data.headers["Content-Type"][0], null);', config=httpbin),
           data_eq("application/json"))
    t.test("delete with header",
           h(f"var r = $std.http.delete('{url}/delete', {{'X-Req-Id': '42'}}); return json(r.data.headers['X-Req-Id'][0], null);", config=httpbin),
           data_eq("42"))

    # SSRF scheme allowlist: a non-http(s) URL is refused up front with HTTP_SSRF_BLOCKED,
    # before any host/IP check and independent of the client's supported-scheme set.
    t.test("non-http scheme blocked up front",
           h(f"var r = $std.http.get('file:///etc/passwd'); return json({{status:r.status, code:r.error && r.error.code}}, null);", config=httpbin),
           lambda r: r["data"]["status"] == 0 and r["data"]["code"] == "HTTP_SSRF_BLOCKED")
    # The scheme allowlist is re-checked on every redirect hop: a redirect to a non-http(s)
    # target (host allowed, scheme not) is not followed, so the 302 is returned unfollowed
    # rather than the client dereferencing the cross-protocol Location.
    redirect_target = f"gopher://{HTTPBIN_HOST}/"
    t.test("cross-protocol redirect not followed",
           h(f"var r = $std.http.get('{url}/redirect-to?url=' + encodeURIComponent('{redirect_target}') + '&status_code=302'); return json(r.status, null);", config=httpbin),
           data_eq(302))


# -- Script registry (execute by key) ----------------------------------------

def test_registry(t: Runner):
    """Exercise `key` mode: XOR validation always; execution if the registry is loaded."""
    t.section("Script registry (execute by key)")

    # Request-shape validation works regardless of how the server was started.
    t.test("script+key rejected (400 SCRIPT_XOR_KEY)",
           {"script": "function handler(ctx) { return json(1, null); }", "key": "greet"},
           lambda r: r["error"]["code"] == "SCRIPT_XOR_KEY")
    t.test("neither script nor key rejected",
           {"context": {"a": 1}},
           lambda r: r["error"]["code"] == "SCRIPT_XOR_KEY")
    t.test("unknown key -> SCRIPT_NOT_FOUND",
           {"key": "no/such/script"},
           lambda r: r["error"]["code"] == "SCRIPT_NOT_FOUND")

    # Key-mode execution needs the server started with scripts_dir=tests/scripts
    # (the harness-started server is; an externally started one may not be).
    probe = _post({"key": "greet"})
    if probe is not None and probe.get("data") == "hello world":
        t.test("execute by key",
               {"key": "greet", "context": {"name": "Alice"}},
               data_eq("hello Alice"))
        t.test("nested key",
               {"key": "acme/billing/pricing", "context": {"qty": 3, "price": 5}},
               lambda r: r["data"]["total"] == 15)
        t.test("key-mode config still per-request (db disabled)",
               {"key": "greet"},
               lambda r: r["error"] is None)
        t.test("meta echoes key + resolved script_bytes",
               {"key": "greet"},
               lambda r: r["meta"]["key"] == "greet" and r["meta"]["script_bytes"] > 0)
        t.test("inline requests carry no meta.key",
               h("return json(1, null);"),
               lambda r: "key" not in r["meta"])
    else:
        print("\n  \033[33mSKIP\033[0m registry execution tests (server not started with scripts_dir=tests/scripts)\n")


# -- Adversarial: try to break the registry + request contract ---------------

def test_registry_hardening(t: Runner):
    """Actively attack the execute-by-key surface: traversal, type confusion, edges."""
    t.section("Registry hardening (adversarial)")

    # Path traversal via key must never escape the registry â€” `key` is a map lookup,
    # never a filesystem path at request time. Each of these is a clean 404, not a
    # file read, a 500, or a panic.
    for evil in ["../greet", "../../../etc/passwd", "..\\..\\greet", "/etc/passwd",
                 "acme/../greet", "greet/../greet", "./greet"]:
        t.test(f"traversal key rejected: {evil}",
               {"key": evil},
               lambda r: r["error"]["code"] == "SCRIPT_NOT_FOUND")

    # The extensionless key is the contract; the filename must miss.
    t.test("key with .js extension misses",
           {"key": "greet.js"},
           lambda r: r["error"]["code"] == "SCRIPT_NOT_FOUND")

    # Degenerate keys: empty string is a present-but-unknown key (404), not "neither".
    t.test("empty-string key -> 404 not XOR",
           {"key": ""},
           lambda r: r["error"]["code"] == "SCRIPT_NOT_FOUND")
    t.test("very long key -> clean 404",
           {"key": "a/" * 5000},
           lambda r: r["error"]["code"] == "SCRIPT_NOT_FOUND")

    # Type confusion: wrong JSON types for script/key must be rejected with the SAME
    # structured {data,error,meta} envelope as every other error (code MALFORMED_REQUEST),
    # never axum's default plain-text rejection and never a panic/hang.
    def malformed(r):
        return (r is not None and "_non_json_body" not in r
                and r.get("data") is None
                and _err_code(r) == "MALFORMED_REQUEST"
                and r["error"]["type"] == "request")
    t.test("numeric key -> MALFORMED_REQUEST envelope", {"key": 123}, malformed)
    t.test("array script -> MALFORMED_REQUEST envelope", {"script": ["function handler(){}"]}, malformed)
    t.test("object key -> MALFORMED_REQUEST envelope", {"key": {"nested": "x"}}, malformed)
    t.test("meta present on malformed body", {"key": 123},
           lambda r: r is not None and "trace_id" in r.get("meta", {}))

    # meta.key must echo on the error paths too (audit trail survives failure).
    t.test("meta.key echoes on SCRIPT_NOT_FOUND",
           {"key": "no/such/thing"},
           lambda r: r["meta"]["key"] == "no/such/thing")
    t.test("meta.key echoes on XOR rejection",
           {"script": "function handler(){}", "key": "greet"},
           lambda r: r["meta"]["key"] == "greet" and r["error"]["code"] == "SCRIPT_XOR_KEY")

    # Registered scripts must travel the IDENTICAL engine path as inline â€” prove the
    # failure modes match by registering nothing special and exercising a known script.
    probe = _post({"key": "greet"})
    if not (probe is not None and probe.get("data") == "hello world"):
        print("\n  \033[33mSKIP\033[0m registry engine-path tests (no scripts_dir)\n")
        return
    # A registered script gets the same sandbox: context still flows, config still
    # per-request, and a huge context is still rejected the same way as inline.
    big = "x" * (5 * 1024 * 1024)
    t.test("oversize context rejected in key mode",
           {"key": "greet", "context": {"blob": big}},
           lambda r: r["error"]["code"] == "CONTEXT_TOO_LARGE")
    t.test("key mode cannot reach db without config",
           {"key": "greet"},
           lambda r: r["error"] is None and r["data"] == "hello world")


def test_isolation_under_concurrency(t: Runner):
    """Fire interleaved requests that pollute globals; prove fresh-context isolation."""
    t.section("Isolation under concurrency (adversarial)")
    import concurrent.futures

    # Each request sets a global and reads it back; if contexts leaked across the pool,
    # a request would observe another's value. Run many in parallel and check every one
    # sees only its own id.
    def one(i):
        # This probes isolation, not capacity. A bulkhead shed (OVERLOADED) or a transient
        # transport blip is NOT a leak â€” retry it (with a gentle backoff under sustained load)
        # rather than counting it as a failure. The ONLY real failure is a leak: an admitted
        # request that observed another request's global value.
        for attempt in range(40):
            body = h(f"globalThis.__leak = {i}; return json(globalThis.__leak, null);")
            r = _post(body)
            if r is None or _err_code(r) == "OVERLOADED":
                time.sleep(0.02 * (1 + attempt // 8))
                continue
            return "ok" if r.get("data") == i else "leak"
        return "shed"  # never admitted within the retry budget â€” capacity, not a leak

    with concurrent.futures.ThreadPoolExecutor(max_workers=16) as ex:
        results = list(ex.map(one, range(200)))
    leaks = results.count("leak")
    admitted = results.count("ok")
    # Isolation invariant: zero leaks. Sheds don't fail the test (they're a capacity artifact
    # on constrained runners), but require a real majority to be admitted so the concurrency is
    # actually exercised â€” never vacuously green because everything was shed.
    t.test("no global leakage across 200 concurrent runs",
           h("return json(1, null);"),
           lambda _r: leaks == 0 and admitted >= 100)

    # A prior request that defines a function must not be visible to the next.
    _post(h("globalThis.__planted = function(){ return 'pwned'; }; return json(1, null);"))
    t.test("planted global not visible to next request",
           h("return json(typeof globalThis.__planted, null);"),
           data_eq("undefined"))


# -- Resilience: bulkhead (Tier 1) ------------------------------------------

def test_bulkhead(t: Runner):
    """Saturate the bulkhead and prove excess load fast-fails OVERLOADED (a retryable `503` +
    `Retry-After`, never `429`) while the server stays responsive (the SLO-protecting behavior)."""
    t.section("Bulkhead / overload (resilience)")
    import concurrent.futures

    # A request that holds its permit for a few hundred ms of CPU work. Calibrated so the
    # ~6 admitted requests finish inside the 4s engine wall-clock even on a slow shared CI
    # runner (debug build, 6-way contention over few cores): 15M iterations blew the budget
    # there (all admitted -> TIMEOUT, zero "ok"), 4M passes with >2x margin on a 2-CPU cgroup
    # while still holding permits far longer than the burst ramp, so shedding stays exercised.
    slow = h("var x=0; for (var i=0;i<4000000;i++){ x+=i; } return json(x>0, null);")

    def fire(_):
        st, r, _hd = _post_full(slow)
        if r is None:
            return ("none", st)
        if _err_code(r) == "OVERLOADED":
            return ("shed", st)
        if r.get("data") is True:
            return ("ok", st)
        # Keep the real error code visible so a failure shows WHAT the non-ok
        # outcomes were (PARTITION_OVERLOADED? TIMEOUT?), not an opaque "other".
        return (_err_code(r) or "other", st)

    with concurrent.futures.ThreadPoolExecutor(max_workers=24) as ex:
        outcomes = list(ex.map(fire, range(24)))
    codes = [c for c, _st in outcomes]
    from collections import Counter
    print(f"  \033[36mINFO\033[0m burst outcomes: {dict(Counter(codes))}")

    # The bulkhead only sheds load when the configured bound is below the burst size.
    # If the server runs the default (auto, high) bound, nothing is shed â€” probe, don't fail.
    if "shed" not in codes:
        print(f"  \033[33mPROBE\033[0m bulkhead not exercised (no shedding; bound >= burst). outcomes={set(codes)}\n")
    else:
        shed_statuses = {st for c, st in outcomes if c == "shed"}
        t.test("bulkhead sheds excess as OVERLOADED",
               h("return json(1,null);"), lambda _r: "shed" in codes)
        # The retry-classification fix: a shed request is a retryable 503, never 429 (whose
        # 4xx digit would make a status-line worker wrongly park it).
        t.test("shed responses are 503, never 429",
               h("return json(1,null);"), lambda _r: shed_statuses == {503})
        t.test("some requests still succeed under overload",
               h("return json(1,null);"), lambda _r: "ok" in codes)
    # Either way, the server must be responsive immediately after the burst.
    t.test("server responsive right after overload burst",
           h("return json('alive', null);"), data_eq("alive"))


def test_partition_fairness(t: Runner):
    """Tier 5: a noisy partition's flood sheds on its OWN per-partition cap
    (PARTITION_OVERLOADED) while a well-behaved partition still gets through."""
    t.section("Per-partition fairness (Tier 5)")
    import concurrent.futures

    slow = "function handler(ctx){ var x=0; for(var i=0;i<20000000;i++){x+=i;} return json(x>0,null); }"
    fast = "function handler(ctx){ return json('ok', null); }"
    noisy_codes, good_outcomes = [], []

    def noisy_worker():
        for _ in range(3):
            noisy_codes.append(_err_code(_post({"script": slow, "partition": "noisy"})))

    def good_worker():
        time.sleep(0.15)  # let the noisy flood ramp first
        for _ in range(4):
            r = _post({"script": fast, "partition": "good"})
            good_outcomes.append((_err_code(r), r.get("data") if r else None))
            time.sleep(0.1)

    with concurrent.futures.ThreadPoolExecutor(max_workers=12) as ex:
        flood = [ex.submit(noisy_worker) for _ in range(6)]
        victim = ex.submit(good_worker)
        for f in flood:
            f.result()
        victim.result()

    partition_shed = sum(1 for c in noisy_codes if c == "PARTITION_OVERLOADED")
    good_ok = sum(1 for code, data in good_outcomes if code is None and data == "ok")

    # Tier 5 is opt-in; if the server has no per-partition cap, nothing sheds â€” probe + skip
    # the fairness asserts, but still check the meta/header plumbing below.
    if partition_shed > 0:
        t.test("noisy partition sheds on its own cap (PARTITION_OVERLOADED)",
               h("return json(1,null);"), lambda _r: partition_shed > 0)
        t.test("good partition still gets through under the noisy flood",
               h("return json(1,null);"), lambda _r: good_ok > 0)
    else:
        print("  \033[33mPROBE\033[0m Tier 5 not active (no max_concurrent_per_partition) â€” asserts skipped\n")

    # Partition-key plumbing works regardless of whether the cap is set:
    r = _post({"script": fast}, headers={"X-Partition-Key": "acme"})
    t.test("X-Partition-Key header echoed in meta.partition",
           h("return json(1,null);"),
           lambda _r: r is not None and r.get("meta", {}).get("partition") == "acme")
    r2 = _post({"script": fast, "partition": "beta"})
    t.test("partition body field echoed in meta.partition",
           h("return json(1,null);"),
           lambda _r: r2 is not None and r2.get("meta", {}).get("partition") == "beta")
    r3 = _post({"script": fast, "partition": "ignored"}, headers={"X-Partition-Key": "header-wins"})
    t.test("header takes precedence over body partition field",
           h("return json(1,null);"),
           lambda _r: r3 is not None and r3.get("meta", {}).get("partition") == "header-wins")


def test_batch(t: Runner):
    """POST /batch: order preservation + per-item id echo, partial-failure isolation, item
    isolation, batch-level caps (empty / over-max / malformed item), and the D6 response-size
    truncation. Per-item quota/authz (trusted mode) are covered by the Rust unit tests; this
    section exercises the non-trusted, deterministic surface end-to-end."""
    t.section("Batch endpoint (/batch)")

    def results_of(resp):
        return resp.get("results", []) if isinstance(resp, dict) else []

    # Order preservation + per-item id echo.
    resp = _post_batch([
        {"script": "function handler(){ return json(1, null); }", "id": "a"},
        {"script": "function handler(){ return json(2, null); }", "id": "b"},
        {"script": "function handler(){ return json(3, null); }", "id": "c"},
    ])
    rs = results_of(resp)
    t.check("results preserve order and echo per-item id",
            [r.get("data") for r in rs] == [1, 2, 3]
            and [r.get("id") for r in rs] == ["a", "b", "c"]
            and resp.get("meta", {}).get("items") == 3
            and resp.get("meta", {}).get("ok") == 3)

    # Partial failure is isolated — a throwing item errors, the others still succeed; still 200.
    resp = _post_batch([
        {"script": "function handler(){ return json(1, null); }"},
        {"script": "function handler(){ throw new Error('boom'); }"},
        {"script": "function handler(){ return json(3, null); }"},
    ])
    rs = results_of(resp)
    t.check("partial failure is isolated (one item errors, the rest succeed)",
            len(rs) == 3
            and rs[0].get("data") == 1
            and rs[1].get("error") is not None
            and rs[2].get("data") == 3
            and resp.get("meta", {}).get("ok") == 2
            and resp.get("meta", {}).get("failed") == 1)

    # Each item runs in a fresh global scope.
    resp = _post_batch([
        {"script": "globalThis.leak = 42; function handler(){ return json('set', null); }"},
        {"script": "function handler(){ return json(typeof globalThis.leak, null); }"},
    ])
    rs = results_of(resp)
    t.check("items are isolated (a global set by one is invisible to another)",
            len(rs) == 2 and rs[1].get("data") == "undefined")

    # Per-item envelope is the single-execute envelope (data/error/meta keys).
    resp = _post_batch([{"script": "function handler(){ return json(7, null); }"}])
    rs = results_of(resp)
    t.check("per-item envelope carries data/error/meta",
            len(rs) == 1 and set(rs[0].keys()) >= {"data", "error", "meta"} and rs[0]["data"] == 7)

    # Empty batch → batch-level 400.
    t.check("empty batch is rejected (EMPTY_BATCH)",
            _err_code(_post_batch([])) == "EMPTY_BATCH")

    # Over the item cap (default 25) → batch-level 400 before any item runs.
    over = [{"script": "function handler(){ return json(1, null); }"} for _ in range(26)]
    t.check("over-max batch is rejected (BATCH_TOO_LARGE)",
            _err_code(_post_batch(over)) == "BATCH_TOO_LARGE")

    # Malformed item (both script and key) fails only itself.
    resp = _post_batch([
        {"script": "function handler(){ return json(1, null); }", "key": "also-a-key"},
        {"script": "function handler(){ return json(2, null); }"},
    ])
    rs = results_of(resp)
    t.check("malformed item fails only itself (SCRIPT_XOR_KEY)",
            len(rs) == 2 and _err_code(rs[0]) == "SCRIPT_XOR_KEY" and rs[1].get("data") == 2)

    # D6: an item that would push the response past the cap is truncated to a size-limit envelope
    # (the harness box sets a small batch.max_response_bytes) — the earlier item still returns.
    resp = _post_batch([
        {"script": "function handler(){ return json(1, null); }"},
        {"script": "function handler(){ return json('x'.repeat(5000), null); }"},
    ])
    rs = results_of(resp)
    t.check("response-size cap truncates the offending item (BATCH_RESPONSE_TRUNCATED)",
            len(rs) == 2 and rs[0].get("data") == 1
            and _err_code(rs[1]) == "BATCH_RESPONSE_TRUNCATED")

    # Fairness (D2): a large slow batch on one partition is bounded to its fair share of the pool and
    # cannot starve a single request on another partition — the rest of the batch queues.
    import concurrent.futures
    slow_item = {"script": "function handler(){ var x=0; for(var i=0;i<20000000;i++){x+=i;} return json(x>0, null); }"}

    def big_batch():
        _post_batch([dict(slow_item) for _ in range(10)], headers={"X-Partition-Key": "batch-noisy"})

    with concurrent.futures.ThreadPoolExecutor(max_workers=2) as ex:
        bg = ex.submit(big_batch)
        time.sleep(0.1)  # let the batch ramp and occupy its fair-share slots
        good = _post({"script": "function handler(){ return json('ok', null); }"},
                     headers={"X-Partition-Key": "batch-good"})
        bg.result()
    t.check("a large batch does not starve another partition's single request",
            good is not None and good.get("data") == "ok")

    # --- Lifecycle: before / shared / after (batch-lifecycle-phases, RQ1-RQ3) ---

    # before → items → after end-to-end: before produces a value merged over the shared seed, every
    # item reads it via ctx.shared, and after reduces the full per-item results into the summary.
    resp = _post_batch_full({
        "before": {"script": "function handler(){ return json({ n: 10, k: 2 }, null); }"},
        "shared": {"k": 1, "seed_only": 9},
        "items": [
            {"script": "function handler(ctx){ return json(ctx.shared.n, null); }"},
            {"script": "function handler(ctx){ return json(ctx.shared.n, null); }"},
            {"script": "function handler(ctx){ return json(ctx.shared.n, null); }"},
        ],
        "after": {"script": "function handler(ctx){ var s=0; ctx.results.forEach(function(r){ s+=r.data; }); return json({ sum: s, seed: ctx.shared.seed_only, k: ctx.shared.k }, null); }"},
    })
    rs = results_of(resp)
    summary = resp.get("summary") if isinstance(resp, dict) else None
    t.check("lifecycle before/shared/after: items read shared, after reduces to summary",
            len(rs) == 3
            and [r.get("data") for r in rs] == [10, 10, 10]
            and isinstance(summary, dict)
            and summary.get("sum") == 30            # after summed all three item results
            and summary.get("seed") == 9            # the shared seed is visible
            and summary.get("k") == 2)              # before wins the seed/before key collision

    # Backward compat: a plain batch (no lifecycle) carries no summary/summary_error.
    resp = _post_batch([{"script": "function handler(){ return json(1, null); }"}])
    t.check("a plain batch omits summary/summary_error",
            isinstance(resp, dict) and "summary" not in resp and "summary_error" not in resp)

    # before is a barrier: a throwing before aborts the whole batch non-200 with no items run.
    resp = _post_batch_full({
        "before": {"script": "function handler(){ throw new Error('boom'); }"},
        "items": [{"script": "function handler(){ return json(1, null); }"}],
        "after": {"script": "function handler(){ return json('never', null); }"},
    })
    t.check("a throwing before is a barrier (non-200, no results, no summary)",
            isinstance(resp, dict) and resp.get("error") is not None
            and "results" not in resp and "summary" not in resp)

    # after is best-effort: a throwing after keeps the 200 with results intact + a summary_error.
    resp = _post_batch_full({
        "items": [
            {"script": "function handler(){ return json(1, null); }"},
            {"script": "function handler(){ return json(2, null); }"},
        ],
        "after": {"script": "function handler(){ throw new Error('reduce failed'); }"},
    })
    rs = results_of(resp)
    t.check("a throwing after keeps 200 + results, reports summary_error",
            len(rs) == 2 and [r.get("data") for r in rs] == [1, 2]
            and resp.get("summary") is None
            and _err_code({"error": resp.get("summary_error")}) is not None)


def test_metrics(t: Runner):
    """The /metrics endpoint exposes Prometheus counters/gauges that move with traffic."""
    t.section("Observability (/metrics)")

    def _scrape() -> str | None:
        res = _get_text("/metrics")
        return res[1] if res is not None and res[0] == 200 else None

    def _counter(text: str | None, needle: str):
        for line in (text or "").splitlines():
            if line.startswith(needle):
                try:
                    return int(line.rsplit(" ", 1)[1])
                except Exception:
                    return None
        return None

    body = _scrape()
    t.test("/metrics returns 200 Prometheus text",
           h("return json(1,null);"),
           lambda _r: body is not None and "runlet_executions_total" in body)
    t.test("/metrics exposes bulkhead + breaker series",
           h("return json(1,null);"),
           lambda _r: body is not None
           and "runlet_bulkhead_permits_total" in body
           and "runlet_db_breaker_trips_total" in body)
    t.test("/metrics exposes the execution latency histogram",
           h("return json(1,null);"),
           lambda _r: body is not None
           and "runlet_execution_duration_seconds_bucket{le=\"+Inf\"}" in body
           and "runlet_execution_duration_seconds_count" in body)
    t.test("/metrics exposes the per-capability latency family",
           h("return json(1,null);"),
           lambda _r: body is not None
           and body.count("# TYPE runlet_capability_op_duration_seconds histogram") == 1
           and "runlet_capability_op_duration_seconds_count{capability=\"db\"}" in body)

    success_label = 'runlet_executions_total{outcome="success"}'
    hist_label = "runlet_execution_duration_seconds_count"
    before = _counter(body, success_label)
    before_hist = _counter(body, hist_label)
    _post(h("return json('ok', null);"))
    after_text = _scrape()
    after = _counter(after_text, success_label)
    after_hist = _counter(after_text, hist_label)
    t.test("success counter advances after an execution",
           h("return json(1,null);"),
           lambda _r: before is not None and after is not None and after > before)
    t.test("latency histogram count advances after an execution",
           h("return json(1,null);"),
           lambda _r: before_hist is not None and after_hist is not None and after_hist > before_hist)

    err_label = 'runlet_executions_total{outcome="script_error"}'
    before_err = _counter(after_text, err_label)
    _post(h("throw new Error('boom');"))
    err_text = _scrape()
    after_err = _counter(err_text, err_label)
    t.test("script_error counter advances after a throw",
           h("return json(1,null);"),
           lambda _r: before_err is not None and after_err is not None and after_err > before_err)

    before_rej = _counter(err_text, "runlet_rejections_total ")
    _post({"context": {}})  # neither script nor key -> SCRIPT_XOR_KEY rejection
    rej_text = _scrape()
    after_rej = _counter(rej_text, "runlet_rejections_total ")
    t.test("rejection counter advances after a bad request",
           h("return json(1,null);"),
           lambda _r: before_rej is not None and after_rej is not None and after_rej > before_rej)


def test_esm(t: Runner):
    """ES-module handlers: a handler authored with `export` (default or named) and a handler
    that `import`s a registered module. Backed by tests/modules/acme/pricing.mjs."""
    t.section("ES modules (export / import)")

    # A classic script handler still works (script-mode is detected by the absence of a
    # top-level `export`) â€” the back-compat guarantee.
    t.test("classic script handler still runs",
           h_raw("function handler(ctx){ return json(ctx.a * 2, null); }", {"a": 21}),
           data_eq(42))

    # `export default function handler` â€” the canonical ESM shape.
    t.test("export default handler",
           h_raw("export default function handler(ctx){ return json('hi:'+ctx.name, null); }",
                 {"name": "Ada"}),
           data_eq("hi:Ada"))

    # `export function handler` â€” named export also resolves.
    t.test("named export handler",
           h_raw("export function handler(ctx){ return json('named', null); }"),
           data_eq("named"))

    # A handler `import`s a registered module and uses its exports.
    t.test("handler imports a registry module",
           h_raw("import { quote, withTax } from 'acme/pricing';\n"
                 "export default function handler(ctx){ return json(withTax(quote(ctx.n, 10)), null); }",
                 {"n": 5}),
           data_eq(55))

    # Named + default + value imports all bind.
    t.test("module exports a constant",
           h_raw("import { TAX_RATE } from 'acme/pricing';\n"
                 "export default function handler(ctx){ return json(TAX_RATE, null); }"),
           data_eq(0.1))

    # Importing an unregistered specifier fails to resolve â€” the security property: a script
    # can reach only registered modules, never an arbitrary path.
    t.test("import of unknown module -> MODULE_NOT_FOUND",
           h_raw("import { x } from 'no/such/module';\n"
                 "export default function handler(ctx){ return json(1, null); }"),
           lambda r: _err_code(r) == "MODULE_NOT_FOUND")
    t.test("import path-traversal specifier -> MODULE_NOT_FOUND",
           h_raw("import { x } from '../../../etc/passwd';\n"
                 "export default function handler(ctx){ return json(1, null); }"),
           lambda r: _err_code(r) == "MODULE_NOT_FOUND")

    # A module-shaped source with no exported handler is a clear HANDLER_NOT_DEFINED.
    t.test("module without exported handler -> error",
           h_raw("export const notHandler = 1;"),
           lambda r: _err_code(r) == "HANDLER_NOT_DEFINED")


def test_hasura(t: Runner):
    """The `hasura/client` injectable module (modules/hasura/client.mjs). Hermetic: each
    handler stubs `globalThis.http` so the module's request-shaping and error-handling are
    exercised without a live Hasura â€” the module reads whatever `http.post` returns."""
    t.section("Hasura module (hasura/client)")

    # Probe: the module must be registered (merged modules_dir). Self-skip otherwise so a
    # run against a server without modules_dir reports SKIP instead of a wall of failures.
    probe = _post(h_raw(
        "import { hasura } from 'hasura/client';\n"
        "export default function handler(ctx){ return json(typeof hasura, null); }"))
    if _err_code(probe) == "MODULE_NOT_FOUND":
        print("\n  \033[33mSKIP\033[0m hasura module tests (no modules_dir with hasura/client)\n")
        return

    # Request shaping: /v1/graphql URL (trailing slash stripped), JSON content-type,
    # admin-secret + role headers, and variables passed through untouched.
    t.test("query shapes the request + returns data",
           h_raw(
               "import { hasura } from 'hasura/client';\n"
               "export default function handler(ctx){\n"
               "  var cap = {};\n"
               "  globalThis.http = { post: function(url, body, headers){\n"
               "    cap = { url: url, body: body, headers: headers };\n"
               "    return { status: 200, data: { data: { users: [{ id: 7 }] } } };\n"
               "  }};\n"
               "  var h = hasura({ endpoint: 'https://hasura.test/', adminSecret: 'sek', role: 'viewer' });\n"
               "  var data = h.query('query { users { id } }', { x: 1 });\n"
               "  return json({ id: data.users[0].id, url: cap.url,\n"
               "    secret: cap.headers['x-hasura-admin-secret'], role: cap.headers['x-hasura-role'],\n"
               "    ctype: cap.headers['content-type'], qHas: typeof cap.body.query === 'string',\n"
               "    varX: cap.body.variables.x }, null);\n"
               "}"),
           data_eq({"id": 7, "url": "https://hasura.test/v1/graphql", "secret": "sek",
                    "role": "viewer", "ctype": "application/json", "qHas": True, "varX": 1}))

    # A forwarded user JWT wins over the admin secret (Bearer set, no admin-secret header).
    t.test("token forwards as Bearer and suppresses admin secret",
           h_raw(
               "import { hasura } from 'hasura/client';\n"
               "export default function handler(ctx){\n"
               "  var cap = {};\n"
               "  globalThis.http = { post: function(url, body, headers){ cap.h = headers;\n"
               "    return { status: 200, data: { data: { ok: true } } }; }};\n"
               "  hasura({ endpoint: 'https://hasura.test', token: 'jwt123', adminSecret: 'sek' }).raw('query { ok }');\n"
               "  return json({ auth: cap.h['authorization'], hasSecret: ('x-hasura-admin-secret' in cap.h) }, null);\n"
               "}"),
           data_eq({"auth": "Bearer jwt123", "hasSecret": False}))

    # GraphQL error inside an HTTP 200 â†’ query() throws with .code + .graphql attached.
    t.test("GraphQL error in a 200 body throws (not silent)",
           h_raw(
               "import { hasura } from 'hasura/client';\n"
               "export default function handler(ctx){\n"
               "  globalThis.http = { post: function(){ return { status: 200, data: { errors: [\n"
               "    { message: 'boom', extensions: { code: 'validation-failed' } }] } }; }};\n"
               "  var h = hasura({ endpoint: 'https://hasura.test' });\n"
               "  try { h.query('query { x }'); return json('no-throw', null); }\n"
               "  catch(e){ return json({ msg: e.message, code: e.code, n: e.graphql.length }, null); }\n"
               "}"),
           data_eq({"msg": "boom", "code": "validation-failed", "n": 1}))

    # Transport failure (api's in-band status:0) â†’ raw() normalizes to an errors envelope,
    # query() throws carrying the transport code.
    t.test("transport failure normalizes + throws",
           h_raw(
               "import { hasura } from 'hasura/client';\n"
               "export default function handler(ctx){\n"
               "  globalThis.http = { post: function(){ return { status: 0, error: { code: 'HTTP_CONNECT', retryable: true } }; }};\n"
               "  var h = hasura({ endpoint: 'https://hasura.test' });\n"
               "  var env = h.raw('query { x }');\n"
               "  var threw = '';\n"
               "  try { h.query('query { x }'); } catch(e){ threw = e.code; }\n"
               "  return json({ envCode: env.errors[0].extensions.code, threw: threw }, null);\n"
               "}"),
           data_eq({"envCode": "HTTP_CONNECT", "threw": "HTTP_CONNECT"}))

    # No endpoint anywhere (no opts, no $std.env) â†’ a clear, actionable throw.
    t.test("missing endpoint throws a helpful error",
           h_raw(
               "import { hasura } from 'hasura/client';\n"
               "export default function handler(ctx){\n"
               "  try { hasura(); return json('no-throw', null); }\n"
               "  catch(e){ return json(e.message.indexOf('HASURA_ENDPOINT') >= 0, null); }\n"
               "}"),
           data_eq(True))


# -- Main --------------------------------------------------------------------

def _wait_for_server() -> bool:
    for _ in range(20):
        if _post(h("return json(1, null);")) is not None:
            return True
        time.sleep(0.5)
    return False


def _start_servers() -> list:
    """Start a single `runlet` box in `.test-run/` for the box-owned tests. Driver-backed egress is
    gone from this suite (real-driver conformance lives in the `fabricd` repo, see
    docs/design/tenant-egress and docs/design/resource-egress.md); the box links no driver and needs
    no sidecar. Returns the started process (caller terminates it)."""
    repo = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    run_dir = os.path.join(repo, ".test-run")
    os.makedirs(run_dir, exist_ok=True)
    # Merge the test-fixture modules (tests/modules) with the shipped operator modules (modules/,
    # e.g. hasura/client.mjs) into one scratch modules_dir, so both are importable without
    # duplicating the shipped module as a fixture (single source of truth).
    merged_modules = os.path.join(run_dir, "modules")
    if os.path.isdir(merged_modules):
        shutil.rmtree(merged_modules)
    os.makedirs(merged_modules)
    for src in (os.path.join(repo, "tests", "modules"), os.path.join(repo, "modules")):
        if os.path.isdir(src):
            shutil.copytree(src, merged_modules, dirs_exist_ok=True)

    # Box config: scripts/modules + low bounds. NO fabricd sidecar, NO `resources`, NO credentials.
    # debug=true relaxes the SSRF private-IP block so the `api` tests can reach the local httpbin.
    box_cfg = {
        "debug": True,
        "scripts_dir": os.path.join(repo, "tests", "scripts"),
        "modules_dir": merged_modules,
        "engine": {"max_concurrent_executions": 6, "max_concurrent_per_partition": 2},
        # Small batch response cap so the /batch section can exercise the D6 truncation guard with
        # cheap items; the item/input caps keep their generous defaults.
        "batch": {"max_response_bytes": 4096},
    }
    with open(os.path.join(run_dir, "config.json"), "w", encoding="utf-8") as fh:
        json.dump(box_cfg, fh)

    subprocess.run(["cargo", "build", "-p", "runlet"], cwd=repo, check=True)
    bindir = os.path.join(repo, "target", "debug")
    runlet = subprocess.Popen(
        [os.path.join(bindir, "runlet")], cwd=run_dir,
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    return [runlet]


def _start_trusted_box(port: int = 3010):
    """Start a dedicated `runlet` in trusted-header mode on a loopback port, for the N5 acting-org
    gate. No `fabricd` is needed â€” the gate fires before any egress session, and the probe script is
    deterministic. Loopback needs no `assert_network_isolation`. Returns `(proc, base_url)` or
    `(None, None)` if the box could not be built/started (the caller self-skips)."""
    repo = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    run_dir = os.path.join(repo, ".test-run", "trusted")
    os.makedirs(run_dir, exist_ok=True)
    cfg = {"server": {"host": "127.0.0.1", "port": port}, "trusted": {"enabled": True}}
    with open(os.path.join(run_dir, "config.json"), "w", encoding="utf-8") as fh:
        json.dump(cfg, fh)
    try:
        subprocess.run(["cargo", "build", "-p", "runlet"], cwd=repo, check=True)
    except Exception:
        return None, None
    binpath = os.path.join(repo, "target", "debug", "runlet")
    proc = subprocess.Popen(
        [binpath], cwd=run_dir, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    url = f"http://127.0.0.1:{port}/execute"
    probe = h("return json(1, null);")
    acting = {"x-workspace-id": "ws_probe", "x-tenant-scope": "acting"}
    for _ in range(40):
        st, _r = _post_status(url, probe, acting)
        if st is not None:
            return proc, url
        time.sleep(0.5)
    proc.terminate()
    return None, None


def _start_echo_server(port: int):
    """A tiny loopback HTTP service that echoes the JSON body it receives. Stands in for a co-located
    box-direct capability service (byo-capabilities D8/D9): the box POSTs `{action, payload}` and the
    service reflects it back, so the calling script can assert the same-envelope round-trip."""

    class _Echo(BaseHTTPRequestHandler):
        def do_POST(self):  # noqa: N802 (http.server API)
            length = int(self.headers.get("Content-Length", "0"))
            body = self.rfile.read(length)
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.end_headers()
            self.wfile.write(body)  # reflect the exact {action, payload} envelope

        def log_message(self, *args):  # silence stderr noise
            pass

    server = ThreadingHTTPServer(("127.0.0.1", port), _Echo)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    return server


def _start_boxdirect_box(port: int, echo_port: int):
    """Start a `runlet` bound loopback with a box-direct `local_resources` binding to the echo service.
    Returns `(proc, base_url)` or `(None, None)` on build/start failure (caller self-skips)."""
    repo = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    run_dir = os.path.join(repo, ".test-run", "boxdirect")
    os.makedirs(run_dir, exist_ok=True)
    cfg = {
        "server": {"host": "127.0.0.1", "port": port},
        # A box-direct binding: the logical name `echo` resolves to the co-located loopback service,
        # with no broker. `debug` stays OFF — the loopback target is reached because it is an
        # operator-declared box-direct binding (loopback-only, boot-guard validated), not via the
        # blanket SSRF relax.
        "local_resources": {"echo": {"url": f"http://127.0.0.1:{echo_port}"}},
    }
    with open(os.path.join(run_dir, "config.json"), "w", encoding="utf-8") as fh:
        json.dump(cfg, fh)
    try:
        subprocess.run(["cargo", "build", "-p", "runlet"], cwd=repo, check=True)
    except Exception:
        return None, None
    binpath = os.path.join(repo, "target", "debug", "runlet")
    proc = subprocess.Popen(
        [binpath], cwd=run_dir, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    url = f"http://127.0.0.1:{port}/execute"
    for _ in range(40):
        st, _r = _post_status(url, h("return json(1, null);"))
        if st is not None:
            return proc, url
        time.sleep(0.5)
    proc.terminate()
    return None, None


def _boxdirect_boot_rejects_remote(port: int) -> bool:
    """Boot guard (D8): a box configured with a **remote** box-direct binding must refuse to start.
    Returns True if the process exits (fails closed) rather than serving."""
    repo = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    run_dir = os.path.join(repo, ".test-run", "boxdirect-remote")
    os.makedirs(run_dir, exist_ok=True)
    cfg = {
        "server": {"host": "127.0.0.1", "port": port},
        "local_resources": {"remote": {"url": "http://93.184.216.34:8080"}},
    }
    with open(os.path.join(run_dir, "config.json"), "w", encoding="utf-8") as fh:
        json.dump(cfg, fh)
    binpath = os.path.join(repo, "target", "debug", "runlet")
    if not os.path.exists(binpath):
        return False
    proc = subprocess.Popen(
        [binpath], cwd=run_dir, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        # A valid boot would keep running; the boot guard makes it exit non-zero promptly.
        return proc.wait(timeout=10) != 0
    except Exception:
        proc.kill()
        return False


def test_box_direct_local(t: Runner):
    """byo-capabilities D8/D9: `io.call(name, ...)` for an operator-declared box-direct binding POSTs
    the identical `{action, payload}` envelope to a co-located loopback service (no broker), meters it
    under `meta.io.<name>`, and the loopback-only boot guard refuses a remote binding."""
    t.section("Box-direct local egress (byo-capabilities)")
    echo_port = 8123
    server = _start_echo_server(echo_port)
    try:
        proc, url = _start_boxdirect_box(3013, echo_port)
        if proc is None:
            print("  \033[33mSKIP\033[0m box-direct box failed to build/start â€” asserts skipped\n")
            return
        try:
            # The script addresses the logical name only; it never sees the endpoint.
            script = h("var r = $std.io.call('echo', 'ping', {x: 1}); "
                       "return json({action: r.action, payload: r.payload}, null);",
                       config={"io": ["echo"]})
            st, r = _post_status(url, script)
            t.check("box-direct round-trip returns the same {action, payload} envelope",
                    st == 200 and r is not None and r.get("data", {}).get("action") == "ping"
                    and '"x":1' in (r.get("data", {}).get("payload") or ""))
            t.check("box-direct call is metered under meta.io.echo",
                    r is not None and isinstance(r.get("meta", {}).get("io", {}).get("echo"), list))

            # An unlisted name is rejected by the allowlist gate before any egress.
            _st2, r2 = _post_status(url, h("$std.io.call('nope', 'ping', {}); return json('x', null);",
                                           config={"io": ["echo"]}))
            t.check("unlisted io name is rejected (RESOURCE_NOT_FOUND)",
                    r2 is not None and r2.get("data") is None and r2.get("error") is not None)
        finally:
            proc.terminate()
            try:
                proc.wait(timeout=10)
            except Exception:
                proc.kill()

        t.check("boot guard refuses a remote box-direct binding (fail closed)",
                _boxdirect_boot_rejects_remote(3014))
    finally:
        server.shutdown()


def test_trusted_acting_scope(t: Runner):
    """Nexus N5: in trusted-header mode a tenant-scoped request must carry `x-tenant-scope: acting`
    (the edge's acting-org assertion) or it is rejected `403 ACTING_SCOPE_REQUIRED` before any
    execution. Runs against a dedicated trusted-mode box on its own port."""
    t.section("Trusted-mode acting-org assurance (nexus N5)")
    proc, url = _start_trusted_box()
    if proc is None:
        print("  \033[33mSKIP\033[0m trusted-mode box failed to build/start â€” asserts skipped\n")
        return
    try:
        script = h("return json(1, null);")
        tenant = {"x-workspace-id": "ws_a"}

        st, r = _post_status(url, script, {**tenant, "x-tenant-scope": "acting"})
        t.check("acting-org request executes (200, data == 1)",
                st == 200 and r is not None and r.get("data") == 1)

        st, r = _post_status(url, script, tenant)
        t.check("missing scope rejected 403 ACTING_SCOPE_REQUIRED",
                st == 403 and _err_code(r) == "ACTING_SCOPE_REQUIRED")

        st, r = _post_status(url, script, {**tenant, "x-tenant-scope": "home"})
        t.check("non-acting scope rejected 403 ACTING_SCOPE_REQUIRED",
                st == 403 and _err_code(r) == "ACTING_SCOPE_REQUIRED")
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=10)
        except Exception:
            proc.kill()


def _start_telemetry_box(port: int = 3011):
    """Start a dedicated `runlet` with tracing enabled, pointed at an OTLP endpoint nothing is
    listening on. Exercises three things at once: W3C `traceparent` propagation into
    `meta.trace_id`, fail-open export (the request must still succeed with the collector down), and
    structured JSON logs on stdout. Returns `(proc, url, log_path)` or `(None, None, None)`."""
    repo = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    run_dir = os.path.join(repo, ".test-run", "telemetry")
    os.makedirs(run_dir, exist_ok=True)
    cfg = {
        "server": {"host": "127.0.0.1", "port": port},
        # Nothing listens on :4317 â€” the tonic channel is lazy, so export just fails in the
        # background (drop-on-full) while requests proceed (fail-open, design D6).
        "telemetry": {
            "otlp_endpoint": "http://127.0.0.1:4317",
            "sample_ratio": 1.0,
            "service_name": "runlet-test",
        },
    }
    with open(os.path.join(run_dir, "config.json"), "w", encoding="utf-8") as fh:
        json.dump(cfg, fh)
    try:
        subprocess.run(["cargo", "build", "-p", "runlet"], cwd=repo, check=True)
    except Exception:
        return None, None, None
    binpath = os.path.join(repo, "target", "debug", "runlet")
    log_path = os.path.join(run_dir, "stdout.log")
    logf = open(log_path, "w", encoding="utf-8")  # noqa: SIM115 (held for the child's lifetime)
    proc = subprocess.Popen(
        [binpath], cwd=run_dir, stdout=logf, stderr=subprocess.STDOUT)
    url = f"http://127.0.0.1:{port}/execute"
    probe = h("return json(1, null);")
    for _ in range(40):
        st, _r = _post_status(url, probe)
        if st is not None:
            return proc, url, log_path
        time.sleep(0.5)
    proc.terminate()
    return None, None, None


def test_telemetry_tracing(t: Runner):
    """OpenTelemetry tracing + structured logs: a propagated W3C `traceparent` becomes
    `meta.trace_id`; without one the box starts its own 32-hex root id; the request succeeds even
    though the collector is unreachable (fail-open); and stdout is structured JSON."""
    t.section("OpenTelemetry tracing + structured logs")
    proc, url, log_path = _start_telemetry_box()
    if proc is None:
        print("  \033[33mSKIP\033[0m telemetry box failed to build/start â€” asserts skipped\n")
        return
    try:
        script = h("return json(1, null);")
        is_hex32 = lambda s: len(s) == 32 and all(c in "0123456789abcdef" for c in s)

        # 1. Propagation: the box continues the edge trace, so meta.trace_id == the traceparent id.
        tp_trace = "0af7651916cd43dd8448eb211c80319c"
        st, r = _post_status(url, script, {"traceparent": f"00-{tp_trace}-b7ad6b7169203331-01"})
        tid = (r or {}).get("meta", {}).get("trace_id", "")
        t.check("traceparent continued into meta.trace_id",
                st == 200 and tid == tp_trace)

        # 2. No traceparent: a fresh box-rooted 32-hex trace id, and the request still succeeds
        #    (fail-open â€” the OTLP collector is down).
        st2, r2 = _post_status(url, script)
        tid2 = (r2 or {}).get("meta", {}).get("trace_id", "")
        t.check("box starts its own trace when no traceparent (fail-open success)",
                st2 == 200 and is_hex32(tid2) and tid2 != tp_trace)

        # 3. Structured logging: stdout carries at least one valid JSON log object.
        time.sleep(0.5)
        json_lines = 0
        with open(log_path, encoding="utf-8") as fh:
            for line in fh:
                line = line.strip()
                if not line:
                    continue
                try:
                    json.loads(line)
                    json_lines += 1
                except ValueError:
                    pass
        t.check("server emits structured JSON logs to stdout", json_lines > 0)
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=10)
        except Exception:
            proc.kill()


def _start_events_box(port: int = 3012):
    """Start a dedicated trusted-mode `runlet` with per-tenant events enabled, capturing stdout so
    the test can read the emitted usage/audit event stream. Returns `(proc, url, log_path)` or
    `(None, None, None)`."""
    repo = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    run_dir = os.path.join(repo, ".test-run", "events")
    os.makedirs(run_dir, exist_ok=True)
    cfg = {
        "server": {"host": "127.0.0.1", "port": port},
        "trusted": {"enabled": True},
        "events": {"enabled": True, "buffer": 1024},
    }
    with open(os.path.join(run_dir, "config.json"), "w", encoding="utf-8") as fh:
        json.dump(cfg, fh)
    try:
        subprocess.run(["cargo", "build", "-p", "runlet"], cwd=repo, check=True)
    except Exception:
        return None, None, None
    binpath = os.path.join(repo, "target", "debug", "runlet")
    log_path = os.path.join(run_dir, "stdout.log")
    logf = open(log_path, "w", encoding="utf-8")  # noqa: SIM115 (held for the child's lifetime)
    proc = subprocess.Popen([binpath], cwd=run_dir, stdout=logf, stderr=subprocess.STDOUT)
    url = f"http://127.0.0.1:{port}/execute"
    probe = h("return json(1, null);")
    for _ in range(40):
        st, _r = _post_status(url, probe, {"x-workspace-id": "ws_probe", "x-tenant-scope": "acting"})
        if st is not None:
            return proc, url, log_path
        time.sleep(0.5)
    proc.terminate()
    return None, None, None


def _read_events(log_path: str) -> list:
    """Parse the emitted event stream from the box's stdout: JSON lines carrying `event_id`
    (distinct from the JSON app-log lines)."""
    events = []
    with open(log_path, encoding="utf-8") as fh:
        for line in fh:
            line = line.strip()
            if not line:
                continue
            try:
                obj = json.loads(line)
            except ValueError:
                continue
            if isinstance(obj, dict) and "event_id" in obj:
                events.append(obj)
    return events


def test_per_tenant_events(t: Runner):
    """Per-tenant usage + audit events: an executed request emits one `usage` + one `allowed`
    audit event attributed to the tenant; a scope-denied request emits one `denied` audit event
    with the reason and no usage event. Every event carries `event_id` + `trace_id`."""
    t.section("Per-tenant usage + audit events")
    proc, url, log_path = _start_events_box()
    if proc is None:
        print("  \033[33mSKIP\033[0m events box failed to build/start â€” asserts skipped\n")
        return
    try:
        script = h("return json(1, null);")
        # Executed request (acting scope) â†’ usage + allowed audit.
        st, _r = _post_status(url, script, {"x-workspace-id": "ws_ev", "x-tenant-scope": "acting"})
        t.check("acting request executes (200)", st == 200)
        # Denied request (no acting scope) â†’ denied audit, no usage.
        st2, _r2 = _post_status(url, script, {"x-workspace-id": "ws_ev"})
        t.check("missing scope rejected (403)", st2 == 403)

        time.sleep(0.6)  # let the writer task flush
        events = _read_events(log_path)
        usage = [e for e in events if e.get("type") == "usage" and e.get("tenant") == "ws_ev"]
        allowed = [e for e in events
                   if e.get("type") == "audit" and e.get("decision") == "allowed"
                   and e.get("tenant") == "ws_ev"]
        denied = [e for e in events
                  if e.get("type") == "audit" and e.get("decision") == "denied"
                  and e.get("reason") == "ACTING_SCOPE_REQUIRED"]
        t.check("usage event emitted for the executed request", len(usage) >= 1)
        t.check("allowed audit event emitted", len(allowed) >= 1)
        t.check("denied audit event carries ACTING_SCOPE_REQUIRED", len(denied) >= 1)
        t.check("every event carries event_id + trace_id",
                bool(events) and all("event_id" in e and "trace_id" in e for e in events))
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=10)
        except Exception:
            proc.kill()


def main():
    procs: list = []

    if not _wait_for_server():
        print("Starting runlet...")
        procs = _start_servers()
        if not _wait_for_server():
            print("ERROR: Server failed to start")
            sys.exit(1)

    print(f"\n\033[1mRunning tests against {BASE_URL}\033[0m")

    t = Runner()
    test_functionality(t)
    test_money(t)
    test_datetime(t)
    test_template(t)
    test_list_interop(t)
    test_user_errors(t)
    test_exceptions(t)
    test_sandbox(t)
    test_json_bridge(t)
    test_meta(t)
    test_status_projection(t)
    test_http_api(t)

    test_registry(t)
    test_registry_hardening(t)
    test_isolation_under_concurrency(t)
    test_bulkhead(t)
    test_partition_fairness(t)
    test_batch(t)
    test_metrics(t)
    test_esm(t)
    test_hasura(t)

    # Box-owned egress + trusted-mode + telemetry + events: each spins up its own dedicated box on a
    # loopback port, so they run only when this harness owns the local build/run (skipped when
    # pointed at an already-running / remote server via JSBOX_URL).
    if procs:
        test_box_direct_local(t)
        test_trusted_acting_scope(t)
        test_telemetry_tracing(t)
        test_per_tenant_events(t)
    else:
        print("\n  \033[33mSKIP\033[0m box-direct + trusted-mode + telemetry + events tests (external server; harness didn't start it)\n")

    t.summary()

    for proc in procs:
        proc.terminate()

    sys.exit(t.failed)


if __name__ == "__main__":
    main()
