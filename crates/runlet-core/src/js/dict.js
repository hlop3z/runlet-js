(function () {
  // The first-class `dict` value-util (beside `$`/`money`/`Decimal`/`datetime`/`text`/`list`). An
  // immutable wrapper over ONE plain JSON object (string keys) with a snake_case, FIELD-NAME-FIRST
  // author surface — safe nested reads and key/value reshaping, no callbacks. Backed by a plain
  // object (not a Map) so it round-trips losslessly through toJSON, matching the JSON-in/JSON-out
  // shape of business data.
  //
  // Pure — no clock, no randomness, no ambient state — so it is injected identically under every
  // profile and there is nothing for the determinism sanitizer to remove. `entries()`/`keys()`/
  // `values()` return a `list` (read off `$std` at call time), bridging dict → list.

  function Dct(obj) {
    this.obj = obj; // the underlying plain object (never mutated in place)
  }

  function isDct(x) { return x instanceof Dct; }

  // Coerce an operand (a dict or a plain object) to a plain object.
  function obj_of(x) {
    if (isDct(x)) return x.obj;
    if (x !== null && typeof x === "object" && !Array.isArray(x)) return x;
    return {};
  }

  function has_own(obj, key) { return Object.prototype.hasOwnProperty.call(obj, key); }

  // ---- unwrap / interop -------------------------------------------------
  // A defensive shallow copy so a caller cannot mutate our backing object.
  Dct.prototype.to_object = function () {
    var out = {};
    var keys = Object.keys(this.obj);
    for (var i = 0; i < keys.length; i++) out[keys[i]] = this.obj[keys[i]];
    return out;
  };
  Dct.prototype.toJSON = function () { return this.obj; };
  Dct.prototype.toString = function () { return Object.prototype.toString.call(this.obj); };
  Dct.prototype.valueOf = function () { return this.obj; };

  // ---- safe nested read -------------------------------------------------
  // get("a.b.c", default?): walk each dotted segment, returning the value at the full path or
  // `def` (undefined when omitted) on any missing / non-object hop.
  Dct.prototype.get = function (path, def) {
    var segs = String(path).split(".");
    var cur = this.obj;
    for (var i = 0; i < segs.length; i++) {
      if (cur === null || typeof cur !== "object" || !has_own(cur, segs[i])) return def;
      cur = cur[segs[i]];
    }
    return cur;
  };

  // ---- reshaping / membership (no callbacks) ----------------------------
  // A new dict with only the named keys that are present.
  Dct.prototype.pick = function () {
    var out = {};
    for (var i = 0; i < arguments.length; i++) {
      var k = arguments[i];
      if (has_own(this.obj, k)) out[k] = this.obj[k];
    }
    return new Dct(out);
  };
  // A new dict without the named keys.
  Dct.prototype.omit = function () {
    var drop = {};
    for (var i = 0; i < arguments.length; i++) drop[arguments[i]] = true;
    var out = {};
    var keys = Object.keys(this.obj);
    for (var j = 0; j < keys.length; j++) {
      if (!has_own(drop, keys[j])) out[keys[j]] = this.obj[keys[j]];
    }
    return new Dct(out);
  };
  Dct.prototype.has = function (field) { return has_own(this.obj, field); };
  // A new dict: shallow last-wins merge of the receiver with `other`.
  Dct.prototype.merge = function (other) {
    var src = obj_of(other);
    var out = {};
    var mine = Object.keys(this.obj);
    for (var i = 0; i < mine.length; i++) out[mine[i]] = this.obj[mine[i]];
    var theirs = Object.keys(src);
    for (var j = 0; j < theirs.length; j++) out[theirs[j]] = src[theirs[j]];
    return new Dct(out);
  };

  // ---- dict → list bridge -----------------------------------------------
  Dct.prototype.keys = function () { return $std.list(Object.keys(this.obj)); };
  Dct.prototype.values = function () {
    var keys = Object.keys(this.obj);
    var out = [];
    for (var i = 0; i < keys.length; i++) out.push(this.obj[keys[i]]);
    return $std.list(out);
  };
  Dct.prototype.entries = function () {
    var keys = Object.keys(this.obj);
    var out = [];
    for (var i = 0; i < keys.length; i++) out.push([keys[i], this.obj[keys[i]]]);
    return $std.list(out);
  };

  // ---- factory ----------------------------------------------------------
  // dict(input): an existing dict is returned as-is; a plain object is snapshotted (defensive
  // copy); any non-object (array, primitive, null) → an empty record.
  function make(input) {
    if (isDct(input)) return input;
    var src = obj_of(input);
    var out = {};
    var keys = Object.keys(src);
    for (var i = 0; i < keys.length; i++) out[keys[i]] = src[keys[i]];
    return new Dct(out);
  }
  $std.dict = make;
})();
