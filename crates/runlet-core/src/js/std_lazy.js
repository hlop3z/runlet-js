(function () {
  // Lazy `$std` materialization (change `lazy-std-injection`, D1/D2/D4). Each value-util member is
  // installed on `$std` as a NON-CONFIGURABLE, GETTER-ONLY accessor. On first access its build-unit
  // is built exactly once: the native `__stdBuild(key)` parses+executes the wrapper IIFE into a
  // fresh scratch realm and stashes the produced members on `globalThis.__stdBuilt`; each member is
  // then deep-frozen, memoized in a closure, and returned. Untouched members are never built — the
  // per-request bootstrap becomes usage-weighted.
  //
  // Genuine intrinsics are captured up front so a handler that reassigns `Object.*` at top level
  // (before its getters fire) cannot subvert a later build or freeze — the eager build these replace
  // had that property for free (it ran before the user script).
  var _create = Object.create;
  var _def = Object.defineProperty;
  var _freeze = Object.freeze;
  var _names = Object.getOwnPropertyNames;
  var _isFrozen = Object.isFrozen;

  // Build-scratch factory: a fresh object whose prototype is the real `$std`, so a wrapper's
  // dependency reads (e.g. money reading `$std.decimal`) delegate through and fire *those* lazy
  // getters — resolving inter-member deps on demand. Each produced member gets an own writable slot
  // so the wrapper's `$std.<name> = X` self-write lands locally instead of hitting the getter-only
  // accessor inherited from the prototype (which, having no setter, would silently swallow it).
  _def(globalThis, "__stdMake", {
    value: function (real, names) {
      var s = _create(real);
      for (var i = 0; i < names.length; i++) {
        _def(s, names[i], {
          value: undefined,
          writable: true,
          configurable: true,
          enumerable: true,
        });
      }
      return s;
    },
  });

  // Deep-freeze a freshly-built member's own-property graph using the captured intrinsics. Freeze
  // BEFORE recursing so a cycle (a constructor <-> its `.prototype.constructor`) terminates on the
  // `isFrozen` guard; `getOwnPropertyNames` never follows `[[Prototype]]`, so this stays within our
  // own definitions and never freezes a shared intrinsic.
  function deepFreeze(o) {
    if (o === null || (typeof o !== "object" && typeof o !== "function")) return o;
    if (_isFrozen(o)) return o;
    _freeze(o);
    var ns = _names(o);
    for (var i = 0; i < ns.length; i++) {
      var v;
      try {
        v = o[ns[i]];
      } catch (e) {
        continue; // a throwing getter is not a value to freeze
      }
      deepFreeze(v);
    }
    return o;
  }
  _def(globalThis, "__stdFreeze", { value: deepFreeze });

  // The lazy member set. Each unit maps 1+ `$std` member names to a build key the native
  // `__stdBuild` dispatches on; members of one unit share a single build + memo (e.g. `sys` builds
  // crypto/env/secrets together from one wrapper). Kept in lockstep with the engine's unit table
  // (`engine::build_unit_sources`).
  var units = [
    { key: "decimal", members: ["decimal"] },
    { key: "money", members: ["money"] },
    { key: "sys", members: ["crypto", "env", "secrets"] },
    { key: "datetime", members: ["datetime"] },
    { key: "text", members: ["text"] },
    { key: "list", members: ["list"] },
    { key: "dict", members: ["dict"] },
    { key: "template", members: ["template"] },
    { key: "check", members: ["check"] },
  ];

  function installUnit(unit) {
    var cache = {};
    var built = false;
    function ensure() {
      if (built) return;
      // Native: parses+builds the unit's wrapper(s) and stashes `{ member: value, … }`.
      __stdBuild(unit.key);
      var all = globalThis.__stdBuilt;
      globalThis.__stdBuilt = undefined;
      for (var i = 0; i < unit.members.length; i++) {
        cache[unit.members[i]] = deepFreeze(all[unit.members[i]]);
      }
      built = true;
    }
    for (var j = 0; j < unit.members.length; j++) {
      (function (m) {
        _def($std, m, {
          get: function () {
            ensure();
            return cache[m];
          },
          enumerable: true,
          configurable: false,
        });
      })(unit.members[j]);
    }
  }

  for (var u = 0; u < units.length; u++) {
    installUnit(units[u]);
  }
})();
