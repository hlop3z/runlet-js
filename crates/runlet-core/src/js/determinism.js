(function () {
  // Profile::Deterministic enforcement: neutralize nondeterministic surfaces so the same
  // (code, context) always produce the same result + `emit(kind, value)` effects. Runs AFTER
  // eval/Proxy removal.
  //
  // D9 (WASI lesson): the ambient authorities are *removed*, not stubbed. A present-but-gated
  // authority is one refactor away from being un-gated, so `Math.random`/`Date.now`/`datetime.now`
  // + `$sys` entropy are `delete`d outright — the property is simply gone (`typeof x === "undefined"`),
  // with no closure left holding the real function to re-reach. `new Date()` (no args) is the one
  // surface that cannot be a property deletion: it is blocked by replacing the constructor, so the
  // wall clock is still structurally unreachable (there is no residual property to reach).

  if (typeof Math !== "undefined") {
    delete Math.random;
  }

  if (typeof Date !== "undefined") {
    var RealDate = Date;
    // Block reading the wall clock via `new Date()` / `Date()` (no args) while keeping
    // explicit construction (`new Date(ms)`, `new Date(y, m, ...)`) and the pure statics.
    var SafeDate = function (a, b, c, d, e, f, g) {
      if (arguments.length === 0) {
        throw new Error(
          "new Date() (current time) is disabled in the deterministic profile"
        );
      }
      switch (arguments.length) {
        case 1: return new RealDate(a);
        case 2: return new RealDate(a, b);
        case 3: return new RealDate(a, b, c);
        case 4: return new RealDate(a, b, c, d);
        case 5: return new RealDate(a, b, c, d, e);
        case 6: return new RealDate(a, b, c, d, e, f);
        default: return new RealDate(a, b, c, d, e, f, g);
      }
    };
    SafeDate.prototype = RealDate.prototype;
    SafeDate.parse = RealDate.parse;
    SafeDate.UTC = RealDate.UTC;
    // `SafeDate.now` is intentionally never copied over — `Date.now` is thus absent (removed),
    // not a throwing stub.
    globalThis.Date = SafeDate;
  }

  // `datetime.now` is the sole ambient-clock reader on the `datetime` value-util — delete it
  // (removed, not stubbed). Everything else on `datetime` (parse/from/components/arithmetic/
  // comparison/formatting) is pure given an explicit instant and stays available.
  if (typeof datetime !== "undefined") {
    delete datetime.now;
  }

  if (typeof $sys !== "undefined") {
    if ($sys.crypto) delete $sys.crypto.uuid;
  }
})();
