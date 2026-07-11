(function () {
  // The freeze/lock epilogue (D3 step 7): make the whole `$std` surface tamper-proof for the
  // handler. Runs strictly AFTER the determinism prune (so the pruned state is what gets locked in)
  // and BEFORE `handler(ctx)` — a prune-before-freeze ordering constraint (freezing first would make
  // the determinism `delete`s fail).

  // Deep-freeze the reachable own-property graph. Freeze BEFORE recursing so a cycle (a constructor
  // ↔ its `.prototype.constructor`) terminates on the `Object.isFrozen` guard. `getOwnPropertyNames`
  // never follows `[[Prototype]]`, so this stays within our own definitions and never freezes a
  // shared intrinsic (`Object.prototype`, `Array.prototype`, …).
  function deepFreeze(o) {
    if (o === null || (typeof o !== "object" && typeof o !== "function")) return;
    if (Object.isFrozen(o)) return;
    Object.freeze(o);
    var names = Object.getOwnPropertyNames(o);
    for (var i = 0; i < names.length; i++) {
      var v;
      try { v = o[names[i]]; } catch (e) { continue; } // a throwing getter is not a value to freeze
      deepFreeze(v);
    }
  }
  deepFreeze($std);

  // Lock each exposed global binding non-writable + non-configurable, re-pinning it to the canonical
  // `$std` member so any top-level reassignment done during the user-script eval is undone.
  var expose = globalThis.__stdExpose;
  var names = Object.keys(expose);
  for (var j = 0; j < names.length; j++) {
    Object.defineProperty(globalThis, names[j], {
      value: $std[expose[names[j]]],
      writable: false,
      enumerable: true,
      configurable: false,
    });
  }
  delete globalThis.__stdExpose;
})();
