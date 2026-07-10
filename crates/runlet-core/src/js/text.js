(function () {
  // The first-class `text` value-util (beside `$`/`money`/`Decimal`/`datetime`). An immutable
  // string value with a snake_case, Python-flavored author surface: the method NAMES are Pythonic
  // renames of native JS string operations — the SEMANTICS are JavaScript's (UTF-16 code units for
  // counting/width; Unicode-default, locale-independent casing). It is pure — no clock, no
  // randomness, no ambient state — so it is injected identically under every profile and there is
  // nothing for the determinism sanitizer to remove. No `__sys` bridge and no Rust math: everything
  // here composes from `String.prototype`.

  // Caller-controlled width/repeat is capped before allocation so `text("x").rjust(1e9)` cannot OOM
  // the isolate — the same fail-closed spirit as the engine's `max_*_bytes` limits.
  var MAX_OUTPUT = 1000000; // 1,000,000 UTF-16 code units

  function Text(s) {
    this.value = s; // the underlying plain string
  }

  function isText(x) { return x instanceof Text; }

  // Coerce any operand (a text value or a raw value) to a plain string.
  function strOf(x) { return isText(x) ? x.value : String(x); }

  function wrap(s) { return new Text(s); }

  // Guard a to-be-produced length against the output cap.
  function checkWidth(n) {
    if (n > MAX_OUTPUT) {
      throw new Error("text: requested width " + n + " exceeds max " + MAX_OUTPUT);
    }
  }

  // ---- case (renamed, Unicode-default) ----------------------------------
  Text.prototype.lower = function () { return wrap(this.value.toLowerCase()); };
  Text.prototype.upper = function () { return wrap(this.value.toUpperCase()); };
  Text.prototype.capitalize = function () {
    var s = this.value;
    return wrap(s ? s.charAt(0).toUpperCase() + s.slice(1).toLowerCase() : s);
  };
  // title-case: capitalize each whitespace-separated word (native has no title-case).
  Text.prototype.title = function () {
    return wrap(this.value.replace(/\S+/g, function (w) {
      return w.charAt(0).toUpperCase() + w.slice(1).toLowerCase();
    }));
  };
  Text.prototype.swap_case = function () {
    return wrap(this.value.replace(/[a-zA-Z]/g, function (c) {
      return c === c.toLowerCase() ? c.toUpperCase() : c.toLowerCase();
    }));
  };

  // ---- strip (optional char set, else whitespace) -----------------------
  // Build a character class from an explicit set of strip characters.
  function stripClass(chars) {
    var escaped = chars.replace(/[.*+?^${}()|[\]\\-]/g, "\\$&");
    return "[" + escaped + "]";
  }
  Text.prototype.strip = function (chars) {
    if (chars === undefined) return wrap(this.value.trim());
    var re = new RegExp("^" + stripClass(chars) + "+|" + stripClass(chars) + "+$", "g");
    return wrap(this.value.replace(re, ""));
  };
  Text.prototype.lstrip = function (chars) {
    if (chars === undefined) return wrap(this.value.replace(/^\s+/, ""));
    return wrap(this.value.replace(new RegExp("^" + stripClass(chars) + "+"), ""));
  };
  Text.prototype.rstrip = function (chars) {
    if (chars === undefined) return wrap(this.value.replace(/\s+$/, ""));
    return wrap(this.value.replace(new RegExp(stripClass(chars) + "+$"), ""));
  };

  // ---- prefix / suffix --------------------------------------------------
  Text.prototype.starts_with = function (p) { return this.value.indexOf(strOf(p)) === 0; };
  Text.prototype.ends_with = function (s) {
    var v = this.value, suf = strOf(s);
    return v.indexOf(suf, v.length - suf.length) !== -1 && v.length >= suf.length;
  };
  Text.prototype.removeprefix = function (p) {
    var pre = strOf(p);
    return wrap(this.value.indexOf(pre) === 0 ? this.value.slice(pre.length) : this.value);
  };
  Text.prototype.removesuffix = function (s) {
    var suf = strOf(s), v = this.value;
    var has = suf.length > 0 && v.indexOf(suf, v.length - suf.length) === v.length - suf.length;
    return wrap(has ? v.slice(0, v.length - suf.length) : v);
  };

  // ---- replace (replaces ALL, matching Python's str.replace) ------------
  Text.prototype.replace = function (old, neu) {
    var from = strOf(old);
    if (from === "") return wrap(this.value);
    return wrap(this.value.split(from).join(strOf(neu)));
  };

  // ---- count (non-overlapping; empty needle → len+1, like Python) -------
  Text.prototype.count = function (sub) {
    var needle = strOf(sub);
    if (needle === "") return this.value.length + 1;
    return this.value.split(needle).length - 1;
  };

  // ---- split / splitlines (return plain strings) ------------------------
  // Left-to-right split with optional Python-style maxsplit (remainder kept in the last piece).
  Text.prototype.split = function (sep, maxsplit) {
    var s = this.value, by = strOf(sep);
    if (maxsplit === undefined || maxsplit < 0) return s.split(by);
    var out = [], start = 0, n = 0;
    while (n < maxsplit) {
      var idx = s.indexOf(by, start);
      if (idx === -1) break;
      out.push(s.slice(start, idx));
      start = idx + by.length;
      n++;
    }
    out.push(s.slice(start));
    return out;
  };
  // Right-to-left split; without maxsplit it equals split.
  Text.prototype.rsplit = function (sep, maxsplit) {
    var s = this.value, by = strOf(sep);
    if (maxsplit === undefined || maxsplit < 0) return s.split(by);
    var out = [], end = s.length, n = 0;
    while (n < maxsplit) {
      var idx = s.lastIndexOf(by, end - by.length);
      if (idx === -1 || by.length === 0) break;
      out.unshift(s.slice(idx + by.length, end));
      end = idx;
      n++;
    }
    out.unshift(s.slice(0, end));
    return out;
  };
  Text.prototype.splitlines = function () {
    if (this.value === "") return [];
    return this.value.split(/\r\n|\r|\n/);
  };

  // ---- padding / alignment (bounded) ------------------------------------
  // Python-style zero-fill honoring a leading sign: "-42".zfill(5) → "-0042".
  Text.prototype.zfill = function (width) {
    var w = Number(width);
    checkWidth(w);
    var s = this.value;
    if (s.length >= w) return wrap(s);
    var sign = (s.charAt(0) === "+" || s.charAt(0) === "-") ? s.charAt(0) : "";
    var body = sign ? s.slice(1) : s;
    var pad = w - s.length;
    return wrap(sign + new Array(pad + 1).join("0") + body);
  };
  Text.prototype.ljust = function (width, fill) {
    var w = Number(width);
    checkWidth(w);
    return wrap(this.value.padEnd(w, fill === undefined ? " " : String(fill)));
  };
  Text.prototype.rjust = function (width, fill) {
    var w = Number(width);
    checkWidth(w);
    return wrap(this.value.padStart(w, fill === undefined ? " " : String(fill)));
  };
  Text.prototype.center = function (width, fill) {
    var w = Number(width);
    checkWidth(w);
    var s = this.value;
    if (s.length >= w) return wrap(s);
    var f = fill === undefined ? " " : String(fill);
    var total = w - s.length;
    var left = Math.floor(total / 2);
    var right = total - left;
    return wrap(new Array(left + 1).join(f) + s + new Array(right + 1).join(f));
  };

  // ---- character-class predicates (non-empty; Unicode-aware) ------------
  Text.prototype.is_digit = function () { return /^\p{Nd}+$/u.test(this.value); };
  Text.prototype.is_alpha = function () { return /^\p{L}+$/u.test(this.value); };
  Text.prototype.is_alnum = function () { return /^[\p{L}\p{Nd}]+$/u.test(this.value); };
  Text.prototype.is_space = function () { return this.value.length > 0 && /^\s+$/.test(this.value); };

  // ---- ERP shaping verbs ------------------------------------------------
  // NFD-fold diacritics → lowercase → collapse non-alphanumerics to single hyphens, no edge hyphens.
  Text.prototype.slugify = function () {
    var s = this.value.normalize("NFD").replace(/[̀-ͯ]/g, "");
    s = s.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, "");
    return wrap(s);
  };
  // Lossy DISPLAY masking (not encoding): keep a tail, mask the rest. Default keep 4, char "*".
  Text.prototype.mask = function (opts) {
    var o = opts || {};
    var keep = o.keep === undefined ? 4 : Number(o.keep);
    var ch = o.char === undefined ? "*" : String(o.char).charAt(0) || "*";
    var s = this.value;
    if (keep <= 0) return wrap(new Array(s.length + 1).join(ch));
    if (keep >= s.length) return wrap(s);
    var masked = new Array(s.length - keep + 1).join(ch);
    return wrap(masked + s.slice(s.length - keep));
  };
  Text.prototype.redact = Text.prototype.mask;
  // Trim then collapse internal whitespace runs to single spaces.
  Text.prototype.collapse = function () {
    return wrap(this.value.replace(/\s+/g, " ").trim());
  };
  // Shorten to at most `limit` code units; append the ellipsis marker (counted toward the limit)
  // when truncation happens.
  Text.prototype.truncate = function (limit, opts) {
    var lim = Number(limit);
    var mark = (opts && opts.ellipsis !== undefined) ? String(opts.ellipsis) : "…";
    var s = this.value;
    if (s.length <= lim) return wrap(s);
    if (lim <= 0) return wrap("");
    if (mark.length >= lim) return wrap(s.slice(0, lim));
    return wrap(s.slice(0, lim - mark.length) + mark);
  };

  // ---- length + interop -------------------------------------------------
  Text.prototype.len = function () { return this.value.length; };
  Text.prototype.to_string = function () { return this.value; };
  Text.prototype.toString = function () { return this.value; };
  Text.prototype.valueOf = function () { return this.value; };
  // json()/JSON.stringify serialize the plain string, never a wrapper object.
  Text.prototype.toJSON = function () { return this.value; };

  // Internal interop hooks the `list` verbs read (not author-facing, absent from base.d.ts):
  // __order_key → the string (lexical ordering); __id_key → the string (grouping/dedup/equality).
  Text.prototype.__order_key = function () { return this.value; };
  Text.prototype.__id_key = function () { return this.value; };

  // ---- factory ----------------------------------------------------------
  // text(input) wraps a value as a string; an existing text value is returned as-is.
  function make(input) {
    if (isText(input)) return input;
    return new Text(String(input === undefined || input === null ? "" : input));
  }
  globalThis.text = make;
})();
