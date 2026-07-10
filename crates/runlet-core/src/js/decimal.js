(function () {
  // One native bridge: __decimal(op, lhs, rhs, aux). `rhs`/`aux` carry per-op auxiliary
  // arguments (a second operand, a places count, a rounding mode, a step, a weights array).
  function call(op, a, b, aux) {
    var raw = __decimal(op, a, b === undefined ? "" : String(b), aux === undefined ? "" : String(aux));
    var res = JSON.parse(raw);
    if (res && res.error) throw new Error(res.error);
    return res.v;
  }

  function Dec(value) {
    this.v = value;
  }

  // Turn anything (number, string, or another Dec) into a decimal string.
  function coerce(x) {
    if (x instanceof Dec) return x.v;
    if (x === undefined || x === null) return "0";
    return String(x);
  }

  Dec.prototype.add = function (o) { return new Dec(call("add", this.v, coerce(o))); };
  Dec.prototype.sub = function (o) { return new Dec(call("sub", this.v, coerce(o))); };
  Dec.prototype.mul = function (o) { return new Dec(call("mul", this.v, coerce(o))); };
  Dec.prototype.div = function (o) { return new Dec(call("div", this.v, coerce(o))); };
  Dec.prototype.neg = function () { return new Dec(call("neg", this.v)); };
  Dec.prototype.abs = function () { return new Dec(call("abs", this.v)); };
  // Round to `places` decimal places (default 0) using `mode` (default "half_up").
  Dec.prototype.round = function (places, mode) {
    return new Dec(call("round", this.v, places === undefined ? 0 : places, mode));
  };
  // Round to the nearest multiple of `step` (e.g. "0.05" cash rounding), using `mode`.
  Dec.prototype.round_to = function (step, mode) {
    return new Dec(call("round_to", this.v, coerce(step), mode));
  };
  Dec.prototype.cmp = function (o) { return parseInt(call("cmp", this.v, coerce(o)), 10); };
  Dec.prototype.eq = function (o) { return this.cmp(o) === 0; };
  Dec.prototype.lt = function (o) { return this.cmp(o) < 0; };
  Dec.prototype.lte = function (o) { return this.cmp(o) <= 0; };
  Dec.prototype.gt = function (o) { return this.cmp(o) > 0; };
  Dec.prototype.gte = function (o) { return this.cmp(o) >= 0; };
  Dec.prototype.is_zero = function () { return this.cmp(0) === 0; };
  Dec.prototype.is_negative = function () { return this.cmp(0) < 0; };
  Dec.prototype.is_positive = function () { return this.cmp(0) > 0; };
  // Bounded scalar helpers — composed in JS over cmp/mul (no Rust).
  Dec.prototype.min = function (o) { var d = make(o); return this.lte(d) ? this : d; };
  Dec.prototype.max = function (o) { var d = make(o); return this.gte(d) ? this : d; };
  Dec.prototype.clamp = function (lo, hi) { return this.max(lo).min(hi); };
  // `p` percent of the value: this * p / 100.
  Dec.prototype.pct = function (p) { return this.mul(p).div(100); };
  Dec.prototype.toString = function () { return this.v; };
  Dec.prototype.to_number = function () { return Number(this.v); };
  // Lets json()/JSON.stringify serialize a decimal as its exact string value.
  Dec.prototype.toJSON = function () { return this.v; };

  // Internal interop hooks the `list` verbs read (not author-facing, absent from base.d.ts):
  // __order_key → an exact ordering value (the Decimal itself); __id_key → a canonical identity
  // string for grouping/dedup/equality.
  Dec.prototype.__order_key = function () { return this; };
  Dec.prototype.__id_key = function () { return this.v; };

  function make(value) {
    if (value instanceof Dec) return value;
    return new Dec(call("parse", coerce(value)));
  }

  // Expose the constructor + a marker so money.js can build ratio Decimals and detect them.
  make._Dec = Dec;
  globalThis.Decimal = make;
})();
