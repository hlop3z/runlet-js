(function () {
  // The exact-number engine (injected first). Money composes over it: every arithmetic op
  // runs through Decimal, and money ÷ money returns a bare Decimal ratio.
  var D = globalThis.Decimal;
  var DecCtor = D._Dec;

  // ---- ISO 4217 minor-unit exponents ------------------------------------
  // "Name the currency, get its decimal places." The exponents are stable/event-driven
  // (they change only on redenomination), so a static table is safe standard practice.
  // Default is 2; only the non-2 currencies are listed. KNOWN is the validation set —
  // a code absent from it throws (unknown currency), never silently defaults.
  var EXP = {
    // zero-decimal
    BIF: 0, CLP: 0, DJF: 0, GNF: 0, ISK: 0, JPY: 0, KMF: 0, KRW: 0, PYG: 0,
    RWF: 0, UGX: 0, UYI: 0, VND: 0, VUV: 0, XAF: 0, XOF: 0, XPF: 0,
    // three-decimal
    BHD: 3, IQD: 3, JOD: 3, KWD: 3, LYD: 3, OMR: 3, TND: 3,
    // four-decimal
    CLF: 4, UYW: 4,
  };
  var KNOWN = {};
  [
    "AED", "AFN", "ALL", "AMD", "ANG", "AOA", "ARS", "AUD", "AWG", "AZN",
    "BAM", "BBD", "BDT", "BGN", "BHD", "BIF", "BMD", "BND", "BOB", "BRL",
    "BSD", "BTN", "BWP", "BYN", "BZD", "CAD", "CDF", "CHF", "CLF", "CLP",
    "CNY", "COP", "CRC", "CUP", "CVE", "CZK", "DJF", "DKK", "DOP", "DZD",
    "EGP", "ERN", "ETB", "EUR", "FJD", "FKP", "GBP", "GEL", "GHS", "GIP",
    "GMD", "GNF", "GTQ", "GYD", "HKD", "HNL", "HTG", "HUF", "IDR", "ILS",
    "INR", "IQD", "IRR", "ISK", "JMD", "JOD", "JPY", "KES", "KGS", "KHR",
    "KMF", "KPW", "KRW", "KWD", "KYD", "KZT", "LAK", "LBP", "LKR", "LRD",
    "LSL", "LYD", "MAD", "MDL", "MGA", "MKD", "MMK", "MNT", "MOP", "MRU",
    "MUR", "MVR", "MWK", "MXN", "MYR", "MZN", "NAD", "NGN", "NIO", "NOK",
    "NPR", "NZD", "OMR", "PAB", "PEN", "PGK", "PHP", "PKR", "PLN", "PYG",
    "QAR", "RON", "RSD", "RUB", "RWF", "SAR", "SBD", "SCR", "SDG", "SEK",
    "SGD", "SHP", "SLE", "SOS", "SRD", "SSP", "STN", "SVC", "SYP", "SZL",
    "THB", "TJS", "TMT", "TND", "TOP", "TRY", "TTD", "TWD", "TZS", "UAH",
    "UGX", "USD", "UYI", "UYU", "UYW", "UZS", "VED", "VES", "VND", "VUV",
    "WST", "XAF", "XCD", "XOF", "XPF", "YER", "ZAR", "ZMW", "ZWG",
  ].forEach(function (code) { KNOWN[code] = true; });

  var SYMBOL = {
    USD: "$", EUR: "€", GBP: "£", JPY: "¥", CNY: "¥",
    INR: "₹", KRW: "₩", BRL: "R$", RUB: "₽", AUD: "$",
    CAD: "$", NZD: "$", HKD: "$", SGD: "$", MXN: "$",
  };

  function exponentOf(cur) {
    if (!KNOWN[cur]) throw new Error("unknown currency: " + cur);
    var e = EXP[cur];
    return e === undefined ? 2 : e;
  }

  // ---- construction + currency cascade ----------------------------------
  // Resolve currency: explicit arg -> config.currency / operator default (both fold into
  // __default_currency, injected by the host) -> else a plain-language error.
  function resolveCurrency(explicit, inherited) {
    var cur = explicit || inherited || globalThis.__default_currency || "";
    cur = String(cur).toUpperCase();
    if (!cur) {
      throw new Error(
        "no currency set — pass one, e.g. $(19.99, \"USD\"), or configure config.currency"
      );
    }
    return cur;
  }

  function Money(amount, currency) {
    this.a = amount; // exact decimal string (major units)
    this.c = currency; // ISO 4217 code
  }

  function isMoney(x) { return x instanceof Money; }
  function isDec(x) { return DecCtor && x instanceof DecCtor; }

  // Amount -> exact decimal string; if it is itself money, carry its currency as the inherited one.
  function amountString(amount) {
    if (isMoney(amount)) return amount.a;
    return D(amount).toString();
  }

  function make(amount, currency) {
    var inherited = isMoney(amount) ? amount.c : undefined;
    var cur = resolveCurrency(currency, inherited);
    exponentOf(cur); // validate the code (throws if unknown)
    return new Money(amountString(amount), cur);
  }

  // Build a money value from a Decimal in `cur`, rounded to the currency minor unit.
  function fromDec(dec, cur, mode) {
    return new Money(dec.round(exponentOf(cur), mode).toString(), cur);
  }

  function sameCurrency(self, other, verb) {
    if (!isMoney(other)) throw new Error("can only " + verb + " money with money");
    if (self.c !== other.c) {
      throw new Error("currency mismatch: " + self.c + " vs " + other.c + " (no implicit conversion)");
    }
  }

  // ---- arithmetic -------------------------------------------------------
  Money.prototype.add = function (o) {
    sameCurrency(this, o, "add");
    return new Money(D(this.a).add(o.a).toString(), this.c);
  };
  Money.prototype.sub = function (o) {
    sameCurrency(this, o, "subtract");
    return new Money(D(this.a).sub(o.a).toString(), this.c);
  };
  Money.prototype.mul = function (s) {
    if (isMoney(s)) throw new Error("cannot multiply money by money");
    return new Money(D(this.a).mul(s).toString(), this.c);
  };
  Money.prototype.div = function (o) {
    if (isMoney(o)) {
      sameCurrency(this, o, "divide");
      return D(this.a).div(o.a); // dimensionless ratio — a Decimal, not money
    }
    return new Money(D(this.a).div(o).toString(), this.c);
  };
  Money.prototype.neg = function () { return new Money(D(this.a).neg().toString(), this.c); };
  Money.prototype.abs = function () { return new Money(D(this.a).abs().toString(), this.c); };

  // ---- business percentages (rounded to the currency precision) ---------
  Money.prototype.pct = function (p) { return fromDec(D(this.a).pct(p), this.c); };
  Money.prototype.add_pct = function (p) {
    return fromDec(D(this.a).add(D(this.a).pct(p)), this.c);
  };
  Money.prototype.sub_pct = function (p) {
    return fromDec(D(this.a).sub(D(this.a).pct(p)), this.c);
  };

  // ---- rounding ---------------------------------------------------------
  Money.prototype.round = function (mode) { return fromDec(D(this.a), this.c, mode); };

  // ---- allocation (largest-remainder, penny-safe) -----------------------
  function allocate_weights(self, weights) {
    var raw = __decimal("allocate", self.a, String(exponentOf(self.c)), JSON.stringify(weights));
    var res = JSON.parse(raw);
    if (res && res.error) throw new Error(res.error);
    var cur = self.c;
    return res.list.map(function (part) { return new Money(part, cur); });
  }
  Money.prototype.allocate = function (weights) { return allocate_weights(this, weights); };
  Money.prototype.allocate_to = function (n) {
    var weights = [];
    for (var i = 0; i < n; i++) weights.push(1);
    return allocate_weights(this, weights);
  };
  Money.prototype.split = function (n) { return this.allocate_to(n); };

  // ---- comparison -------------------------------------------------------
  Money.prototype.cmp = function (o) { sameCurrency(this, o, "compare"); return D(this.a).cmp(o.a); };
  Money.prototype.eq = function (o) { return this.cmp(o) === 0; };
  Money.prototype.lt = function (o) { return this.cmp(o) < 0; };
  Money.prototype.lte = function (o) { return this.cmp(o) <= 0; };
  Money.prototype.gt = function (o) { return this.cmp(o) > 0; };
  Money.prototype.gte = function (o) { return this.cmp(o) >= 0; };
  Money.prototype.is_zero = function () { return D(this.a).is_zero(); };
  Money.prototype.is_negative = function () { return D(this.a).is_negative(); };
  Money.prototype.is_positive = function () { return D(this.a).is_positive(); };

  // ---- read-out + interop ----------------------------------------------
  // Integer minor units (zero-decimal-correct via the currency exponent) for payment payloads.
  Money.prototype.to_minor = function () {
    var raw = __decimal("to_minor", this.a, String(exponentOf(this.c)), "");
    var res = JSON.parse(raw);
    if (res && res.error) throw new Error(res.error);
    return Number(res.v);
  };
  Money.prototype.amount = function () { return D(this.a); }; // currency-less Decimal
  Money.prototype.currency = function () { return this.c; };
  Money.prototype.format = function () {
    var sym = SYMBOL[this.c];
    var shown = D(this.a).round(exponentOf(this.c)).toString();
    return sym ? sym + shown : this.c + " " + shown;
  };
  Money.prototype.to_string = function () { return this.a; };
  Money.prototype.toString = function () { return this.a; };
  Money.prototype.to_number = function () { return Number(this.a); };
  // Always self-describing: json()/JSON.stringify emit { amount, currency, minor_units }.
  Money.prototype.toJSON = function () {
    return { amount: this.a, currency: this.c, minor_units: this.to_minor() };
  };

  // Internal interop hooks the `list` verbs read (not author-facing, absent from base.d.ts):
  // __order_key → the amount as an exact Decimal (currency stripped, for numeric ordering only);
  // __id_key → an identity string that INCLUDES the currency (USD 19.99 ≠ EUR 19.99).
  Money.prototype.__order_key = function () { return D(this.a); };
  Money.prototype.__id_key = function () { return this.a + " " + this.c; };

  // Expose the constructor so `list` can brand-check money values (mirrors Decimal._Dec).
  make._Money = Money;
  globalThis.$ = make;
  globalThis.money = make;
})();
