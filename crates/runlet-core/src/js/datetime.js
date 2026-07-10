(function () {
  // The first-class `datetime` value-util (beside `$`/`money`/`Decimal`). An immutable value is a
  // canonical UTC instant (`ms`, epoch milliseconds) plus an optional IANA `zone` for a *view*: the
  // zone changes only how components/boundaries/formatting resolve, never the underlying instant.
  // All calendar/timezone math is served by the Rust `datetime` domain over the shared `__sys`
  // bridge — this file is the thin, immutable, snake_case wrapper.
  function call(op, payload) {
    var raw = __sys("datetime", op, JSON.stringify(payload || {}));
    var res = JSON.parse(raw);
    if (res && res.error) throw new Error(res.error);
    return res.v;
  }

  // ms = canonical UTC epoch millis; zone = IANA name of a view (undefined ⇒ UTC interpretation).
  function DateTime(ms, zone) {
    this.ms = ms;
    this.zone = zone;
  }

  function isDateTime(x) { return x instanceof DateTime; }

  // Base FFI payload for a value: the instant, plus its view zone when this is a zoned view.
  function payloadFor(self, extra) {
    var p = extra || {};
    p.ms = self.ms;
    if (self.zone !== undefined) p.zone = self.zone;
    return p;
  }

  // The component bundle, computed once per value (in its view zone) and memoized.
  function partsOf(self) {
    if (self._parts === undefined) {
      Object.defineProperty(self, "_parts", { value: call("parts", payloadFor(self)) });
    }
    return self._parts;
  }

  // Epoch millis of another operand: a datetime value or a raw epoch-millis number.
  function msOf(other) {
    return isDateTime(other) ? other.ms : Number(other);
  }

  // ---- calendar arithmetic ----------------------------------------------
  // Calendar months (years fold in); the Rust side clamps end-of-month (Jan 31 + 1mo → Feb 28/29).
  function monthsOf(delta) {
    var x = delta || {};
    return (x.years || 0) * 12 + (x.months || 0);
  }
  // Fixed-length units summed to milliseconds (no months/years — those are ambiguous length).
  function fixedMs(delta) {
    var x = delta || {};
    var weeks = x.weeks || 0, days = x.days || 0, hours = x.hours || 0;
    var minutes = x.minutes || 0, seconds = x.seconds || 0, ms = x.ms || 0;
    return ((((weeks * 7 + days) * 24 + hours) * 60 + minutes) * 60 + seconds) * 1000 + ms;
  }
  DateTime.prototype.add = function (delta) {
    var ms = call("add", { ms: this.ms, months: monthsOf(delta), fixed_ms: fixedMs(delta) });
    return new DateTime(ms, this.zone);
  };
  DateTime.prototype.sub = function (delta) {
    var ms = call("add", { ms: this.ms, months: -monthsOf(delta), fixed_ms: -fixedMs(delta) });
    return new DateTime(ms, this.zone);
  };

  // ---- difference -------------------------------------------------------
  var UNIT_MS = {
    ms: 1, seconds: 1000, minutes: 60000, hours: 3600000, days: 86400000, weeks: 604800000,
  };
  DateTime.prototype.diff = function (other) {
    return call("diff", { a: this.ms, b: msOf(other) });
  };
  DateTime.prototype.diff_in = function (other, unit) {
    var per = UNIT_MS[unit];
    if (per === undefined) throw new Error("unknown diff unit: " + unit);
    var whole = (this.ms - msOf(other)) / per;
    return whole < 0 ? Math.ceil(whole) : Math.floor(whole); // whole units, truncated toward zero
  };

  // ---- components (resolved in the view zone) ---------------------------
  DateTime.prototype.year = function () { return partsOf(this).year; };
  DateTime.prototype.month = function () { return partsOf(this).month; };
  DateTime.prototype.day = function () { return partsOf(this).day; };
  DateTime.prototype.hour = function () { return partsOf(this).hour; };
  DateTime.prototype.minute = function () { return partsOf(this).minute; };
  DateTime.prototype.second = function () { return partsOf(this).second; };
  DateTime.prototype.millisecond = function () { return partsOf(this).millisecond; };
  DateTime.prototype.weekday = function () { return partsOf(this).weekday; }; // ISO 1=Mon…7=Sun
  DateTime.prototype.quarter = function () { return partsOf(this).quarter; };
  DateTime.prototype.day_of_year = function () { return partsOf(this).day_of_year; };
  DateTime.prototype.iso_week = function () { return partsOf(this).iso_week; }; // {week, week_year}
  DateTime.prototype.days_in_month = function () { return partsOf(this).days_in_month; };

  // ---- period boundaries (computed in the view zone) -------------------
  DateTime.prototype.start_of = function (unit) {
    return new DateTime(call("start_of", payloadFor(this, { unit: unit })), this.zone);
  };
  DateTime.prototype.end_of = function (unit) {
    return new DateTime(call("end_of", payloadFor(this, { unit: unit })), this.zone);
  };

  // ---- weekend-aware business days (no holiday calendar) ---------------
  DateTime.prototype.is_weekend = function () {
    var wd = partsOf(this).weekday;
    return wd === 6 || wd === 7; // Saturday or Sunday
  };
  DateTime.prototype.is_business_day = function () { return !this.is_weekend(); };
  DateTime.prototype.add_business_days = function (n) {
    var step = n < 0 ? -1 : 1;
    var remaining = Math.abs(n);
    var cur = this;
    while (remaining > 0) {
      cur = cur.add({ days: step });
      if (cur.is_business_day()) remaining--;
    }
    return cur;
  };

  // ---- comparison (never reads the ambient clock) ----------------------
  DateTime.prototype.cmp = function (o) {
    var a = this.ms, b = msOf(o);
    return a < b ? -1 : a > b ? 1 : 0;
  };
  DateTime.prototype.eq = function (o) { return this.cmp(o) === 0; };
  DateTime.prototype.lt = function (o) { return this.cmp(o) < 0; };
  DateTime.prototype.lte = function (o) { return this.cmp(o) <= 0; };
  DateTime.prototype.gt = function (o) { return this.cmp(o) > 0; };
  DateTime.prototype.gte = function (o) { return this.cmp(o) >= 0; };
  DateTime.prototype.is_between = function (a, b) { return this.gte(a) && this.lte(b); };

  // ---- timezone view ----------------------------------------------------
  DateTime.prototype.in_zone = function (zone) {
    var z = String(zone);
    call("iso", { ms: this.ms, zone: z }); // validate the zone now (throws on an unknown name)
    return new DateTime(this.ms, z);
  };

  // ---- formatting / read-out -------------------------------------------
  // Resolve the effective render zone: an explicit arg, else this view's zone, else UTC.
  function renderZone(self, zone) {
    return zone !== undefined ? String(zone) : self.zone;
  }
  DateTime.prototype.iso = function (zone) {
    var p = { ms: this.ms };
    var z = renderZone(this, zone);
    if (z !== undefined) p.zone = z;
    return call("iso", p);
  };
  DateTime.prototype.format = function (pattern, zone) {
    var p = { ms: this.ms, pattern: String(pattern) };
    var z = renderZone(this, zone);
    if (z !== undefined) p.zone = z;
    return call("format", p);
  };
  DateTime.prototype.unix = function () { return Math.floor(this.ms / 1000); };
  DateTime.prototype.epoch_ms = function () { return this.ms; };
  // json()/JSON.stringify + coercion serialize the CANONICAL instant as RFC 3339 UTC (Z), never
  // the view zone — the value is always a UTC instant on the wire.
  DateTime.prototype.toJSON = function () { return call("iso", { ms: this.ms }); };
  DateTime.prototype.toString = function () { return call("iso", { ms: this.ms }); };

  // ---- factory ----------------------------------------------------------
  function parse(input) {
    if (isDateTime(input)) return new DateTime(input.ms); // canonicalize: drop any view zone
    return new DateTime(call("parse", { input: input }));
  }
  function from(parts, zone) {
    var p = { parts: parts };
    if (zone !== undefined) p.zone = String(zone);
    return new DateTime(call("from", p));
  }
  // `now` is the sole ambient-clock reader; `js/determinism.js` deletes it under the deterministic
  // profile (removed, not stubbed), leaving parse/from/components/arithmetic intact.
  function now() { return new DateTime(call("now", {})); }

  // datetime(input) ≡ datetime.parse(input); named constructors hang off the same function.
  function make(input) { return parse(input); }
  make.now = now;
  make.parse = parse;
  make.from = from;
  globalThis.datetime = make;
})();
