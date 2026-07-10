(function () {
  // The first-class `list` value-util (beside `$`/`money`/`Decimal`/`datetime`/`text`/`dict`). An
  // immutable wrapper over a plain array of records with a snake_case, FIELD-NAME-FIRST author
  // surface: every shaping verb takes a field-name string or a match-by-example object — never a
  // callback — because the audience (ERP / e-commerce self-serve authors) does not write arrow
  // functions. The names are the SQL / Shopify-Liquid vocabulary these authors already half-know.
  //
  // Pure — no clock, no randomness, no ambient state — so it is injected identically under every
  // profile and there is nothing for the determinism sanitizer to remove (which is also why it
  // exposes no random-order verb like shuffle/sample). Column aggregates compose over the injected
  // `Decimal` global so a currency column is summed EXACTLY, never as a float. `group_by` returns a
  // `dict`; both globals are resolved at call time so injection order is flexible.

  // Caller-controlled length is capped before allocation so a single call cannot OOM the isolate —
  // the same fail-closed spirit as the engine's `max_*_bytes` limits and `text.js`'s MAX_OUTPUT.
  var MAX_OUTPUT = 1000000; // 1,000,000 elements

  function Lst(arr) {
    this.arr = arr; // the underlying plain array (never mutated in place)
  }

  function isLst(x) { return x instanceof Lst; }

  // Guard a to-be-produced length against the output cap.
  function checkLen(n) {
    if (n > MAX_OUTPUT) {
      throw new Error("list: length " + n + " exceeds max " + MAX_OUTPUT);
    }
  }

  function wrap(arr) { return new Lst(arr); }

  // Read `field` off a record, tolerating null/undefined records.
  function field_of(record, field) {
    if (field === undefined) return record;
    return record === null || record === undefined ? undefined : record[field];
  }

  // ---- Decimal-backed numeric coercion for aggregates -------------------
  // Returns a Decimal for a numeric value, or null to SKIP (absent/non-numeric), so blanks never
  // corrupt a sum. Reads the `Decimal` global at call time.
  function num_of(v) {
    if (v === null || v === undefined) return null;
    var Decimal = globalThis.Decimal;
    if (Decimal && Decimal._Dec && v instanceof Decimal._Dec) return v;
    if (typeof v === "number") return isFinite(v) ? Decimal(v) : null;
    if (typeof v === "string") {
      var t = v.trim();
      if (t === "" || !isFinite(Number(t))) return null;
      return Decimal(t);
    }
    return null;
  }

  // ---- unwrap / interop -------------------------------------------------
  // A defensive copy so a caller cannot mutate our backing array.
  Lst.prototype.to_array = function () { return this.arr.slice(); };
  Lst.prototype.toJSON = function () { return this.arr; };
  Lst.prototype.toString = function () { return this.arr.toString(); };
  Lst.prototype.valueOf = function () { return this.arr; };
  Lst.prototype[Symbol.iterator] = function () { return this.arr[Symbol.iterator](); };

  // ---- positional access + length ---------------------------------------
  Lst.prototype.len = function () { return this.arr.length; };
  Lst.prototype.at = function (i) {
    var n = Number(i);
    if (n < 0) n = this.arr.length + n;
    return this.arr[n];
  };
  Lst.prototype.get = Lst.prototype.at;
  Lst.prototype.first = function () {
    return this.arr.length === 0 ? null : this.arr[0];
  };
  Lst.prototype.last = function () {
    return this.arr.length === 0 ? null : this.arr[this.arr.length - 1];
  };

  // ---- field-name-first shaping (no callbacks) --------------------------
  // Keep records where every field:value pair in `match` strictly equals the record's field.
  Lst.prototype.where = function (match) {
    var m = match || {};
    var keys = Object.keys(m);
    var out = [];
    for (var i = 0; i < this.arr.length; i++) {
      var rec = this.arr[i];
      var ok = true;
      for (var k = 0; k < keys.length; k++) {
        if (field_of(rec, keys[k]) !== m[keys[k]]) { ok = false; break; }
      }
      if (ok) out.push(rec);
    }
    return wrap(out);
  };

  // Stable sort by a named field (ascending; "desc" for descending). No field → sort the elements
  // themselves. Operates on a copy so the receiver is never mutated.
  Lst.prototype.sort_by = function (field, direction) {
    var dir = direction === "desc" ? -1 : 1;
    var copy = this.arr.slice();
    copy.sort(function (a, b) {
      var av = field_of(a, field);
      var bv = field_of(b, field);
      if (av < bv) return -1 * dir;
      if (av > bv) return 1 * dir;
      return 0; // ties keep input order (stable sort)
    });
    return wrap(copy);
  };

  // A new list of one field's value from each record.
  Lst.prototype.column = function (field) {
    var out = [];
    for (var i = 0; i < this.arr.length; i++) out.push(field_of(this.arr[i], field));
    return wrap(out);
  };

  // Distinct scalars by value (first occurrence wins).
  Lst.prototype.unique = function () {
    var seen = new Set();
    var out = [];
    for (var i = 0; i < this.arr.length; i++) {
      var v = this.arr[i];
      if (!seen.has(v)) { seen.add(v); out.push(v); }
    }
    return wrap(out);
  };

  // Distinct records by a named field's value (first occurrence wins).
  Lst.prototype.unique_by = function (field) {
    var seen = new Set();
    var out = [];
    for (var i = 0; i < this.arr.length; i++) {
      var key = field_of(this.arr[i], field);
      if (!seen.has(key)) { seen.add(key); out.push(this.arr[i]); }
    }
    return wrap(out);
  };

  // ---- list → dict bridge -----------------------------------------------
  // Group records into a dict of lists, keyed by the stringified field value, input order kept.
  Lst.prototype.group_by = function (field) {
    var groups = {};
    var order = [];
    for (var i = 0; i < this.arr.length; i++) {
      var key = String(field_of(this.arr[i], field));
      if (!Object.prototype.hasOwnProperty.call(groups, key)) {
        groups[key] = [];
        order.push(key);
      }
      groups[key].push(this.arr[i]);
    }
    var out = {};
    for (var j = 0; j < order.length; j++) out[order[j]] = wrap(groups[order[j]]);
    return globalThis.dict(out);
  };

  // ---- exact-Decimal aggregates over a named column ---------------------
  Lst.prototype.sum = function (field) {
    var total = globalThis.Decimal(0);
    for (var i = 0; i < this.arr.length; i++) {
      var d = num_of(field_of(this.arr[i], field));
      if (d !== null) total = total.add(d);
    }
    return total;
  };
  Lst.prototype.avg = function (field) {
    var total = globalThis.Decimal(0);
    var n = 0;
    for (var i = 0; i < this.arr.length; i++) {
      var d = num_of(field_of(this.arr[i], field));
      if (d !== null) { total = total.add(d); n++; }
    }
    return n === 0 ? null : total.div(n);
  };
  Lst.prototype.min = function (field) {
    var best = null;
    for (var i = 0; i < this.arr.length; i++) {
      var d = num_of(field_of(this.arr[i], field));
      if (d !== null && (best === null || d.lt(best))) best = d;
    }
    return best;
  };
  Lst.prototype.max = function (field) {
    var best = null;
    for (var i = 0; i < this.arr.length; i++) {
      var d = num_of(field_of(this.arr[i], field));
      if (d !== null && (best === null || d.gt(best))) best = d;
    }
    return best;
  };
  Lst.prototype.count = function () { return this.arr.length; };

  // ---- factory ----------------------------------------------------------
  // list(input): an existing list is returned as-is; an array is snapshotted (defensive copy);
  // null/undefined → empty; any other value → a single-element list.
  function make(input) {
    if (isLst(input)) return input;
    if (Array.isArray(input)) {
      checkLen(input.length);
      return new Lst(input.slice());
    }
    if (input === null || input === undefined) return new Lst([]);
    return new Lst([input]);
  }
  globalThis.list = make;
})();
