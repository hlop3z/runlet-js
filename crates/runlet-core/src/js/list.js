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

  // ---- value-util interop protocol --------------------------------------
  // The shaping/aggregate verbs treat value-util wrappers (money/decimal/datetime/text) by their
  // CANONICAL value, never by JS reference identity or default string coercion. Each wrapper exposes
  // two internal hooks (__order_key / __id_key); these resolvers prefer them and fall back to the
  // raw value for plain scalars. Kept here so no verb branches on wrapper type directly.

  // A money value (branded via money._Money, mirroring Decimal._Dec), or null.
  function money_of(v) {
    var M = globalThis.money;
    var Ctor = M && M._Money;
    return Ctor && v instanceof Ctor ? v : null;
  }

  // An ordering key: a wrapper's __order_key (exact Decimal / epoch ms / string) or the raw scalar.
  function order_of(v) {
    return v !== null && v !== undefined && typeof v.__order_key === "function"
      ? v.__order_key()
      : v;
  }
  // Compare two ordering keys; Decimal keys compare exactly, everything else natively.
  function cmp_order(a, b) {
    var oa = order_of(a), ob = order_of(b);
    var Decimal = globalThis.Decimal;
    if (Decimal && Decimal._Dec && oa instanceof Decimal._Dec && ob instanceof Decimal._Dec) {
      return oa.cmp(ob);
    }
    return oa < ob ? -1 : oa > ob ? 1 : 0;
  }

  // An identity key for grouping/dedup/equality: a wrapper's __id_key (a distinguishing string,
  // currency-inclusive for money) or the raw scalar.
  function id_of(v) {
    return v !== null && v !== undefined && typeof v.__id_key === "function" ? v.__id_key() : v;
  }
  function id_eq(a, b) { return id_of(a) === id_of(b); }

  // An aggregatable value for a column: a money value, else a Decimal, else null (skip).
  function agg_of(v) {
    var m = money_of(v);
    return m !== null ? m : num_of(v);
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
  // Keep records where every field:value pair in `match` equals the record's field by CANONICAL
  // value (so a money/decimal/datetime/text field matches an equal wrapper, not just the same ref).
  Lst.prototype.where = function (match) {
    var m = match || {};
    var keys = Object.keys(m);
    var out = [];
    for (var i = 0; i < this.arr.length; i++) {
      var rec = this.arr[i];
      var ok = true;
      for (var k = 0; k < keys.length; k++) {
        if (!id_eq(field_of(rec, keys[k]), m[keys[k]])) { ok = false; break; }
      }
      if (ok) out.push(rec);
    }
    return wrap(out);
  };

  // Stable sort by a named field (ascending; "desc" for descending). No field → sort the elements
  // themselves. Wrapper fields order by their canonical value (decimal/money numerically, datetime
  // chronologically), never lexically. Operates on a copy so the receiver is never mutated.
  Lst.prototype.sort_by = function (field, direction) {
    var dir = direction === "desc" ? -1 : 1;
    var copy = this.arr.slice();
    copy.sort(function (a, b) {
      return cmp_order(field_of(a, field), field_of(b, field)) * dir; // 0 ⇒ stable (input order)
    });
    return wrap(copy);
  };

  // A new list of one field's value from each record.
  Lst.prototype.column = function (field) {
    var out = [];
    for (var i = 0; i < this.arr.length; i++) out.push(field_of(this.arr[i], field));
    return wrap(out);
  };

  // Distinct scalars by canonical value (first occurrence wins). Equal wrapper values (money by
  // amount+currency, decimal/datetime/text by value) collapse; distinct currencies stay distinct.
  Lst.prototype.unique = function () {
    var seen = new Set();
    var out = [];
    for (var i = 0; i < this.arr.length; i++) {
      var v = this.arr[i];
      var key = id_of(v);
      if (!seen.has(key)) { seen.add(key); out.push(v); }
    }
    return wrap(out);
  };

  // Distinct records by a named field's canonical value (first occurrence wins).
  Lst.prototype.unique_by = function (field) {
    var seen = new Set();
    var out = [];
    for (var i = 0; i < this.arr.length; i++) {
      var key = id_of(field_of(this.arr[i], field));
      if (!seen.has(key)) { seen.add(key); out.push(this.arr[i]); }
    }
    return wrap(out);
  };

  // ---- list → dict bridge -----------------------------------------------
  // Group records into a dict of lists, keyed by the field's CANONICAL value (money keeps its
  // currency in the key, so USD 19.99 ≠ EUR 19.99), input order kept within each group.
  Lst.prototype.group_by = function (field) {
    var groups = {};
    var order = [];
    for (var i = 0; i < this.arr.length; i++) {
      var key = String(id_of(field_of(this.arr[i], field)));
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

  // ---- exact aggregates over a named column -----------------------------
  // Over a number/numeric-string/Decimal column these fold to an exact Decimal; over a `money`
  // column they return a `money` PRESERVING the currency and THROW on mixed currencies (reusing
  // money's same-currency guard). A column is treated as money once any money value is seen; other
  // values are then skipped. Non-aggregatable values (blanks/non-numeric) are always skipped.
  Lst.prototype.sum = function (field) {
    var money_total = null;
    var dec_total = globalThis.Decimal(0);
    for (var i = 0; i < this.arr.length; i++) {
      var v = field_of(this.arr[i], field);
      var m = money_of(v);
      if (m !== null) { money_total = money_total === null ? m : money_total.add(m); continue; }
      if (money_total !== null) continue; // numeric stray in a money column
      var d = num_of(v);
      if (d !== null) dec_total = dec_total.add(d);
    }
    return money_total !== null ? money_total : dec_total;
  };
  Lst.prototype.avg = function (field) {
    var money_total = null, money_n = 0;
    var dec_total = globalThis.Decimal(0), dec_n = 0;
    for (var i = 0; i < this.arr.length; i++) {
      var v = field_of(this.arr[i], field);
      var m = money_of(v);
      if (m !== null) { money_total = money_total === null ? m : money_total.add(m); money_n++; continue; }
      if (money_total !== null) continue;
      var d = num_of(v);
      if (d !== null) { dec_total = dec_total.add(d); dec_n++; }
    }
    if (money_total !== null) return money_total.div(money_n);
    return dec_n === 0 ? null : dec_total.div(dec_n);
  };
  // min/max over an aggregatable column; money columns compare same-currency (throw on mismatch)
  // and return money, Decimal columns return Decimal.
  function extremum(arr, field, keep_left) {
    var best = null;
    for (var i = 0; i < arr.length; i++) {
      var val = agg_of(field_of(arr[i], field));
      if (val === null) continue;
      if (best === null || keep_left(val.cmp(best))) best = val;
    }
    return best;
  }
  Lst.prototype.min = function (field) {
    return extremum(this.arr, field, function (c) { return c < 0; });
  };
  Lst.prototype.max = function (field) {
    return extremum(this.arr, field, function (c) { return c > 0; });
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
