(function () {
  // Freeze/lock epilogue (D3 step 7 / MODIFIED freeze requirement), lazy variant. Runs strictly
  // AFTER the determinism prune and BEFORE `handler(ctx)`.
  //
  // Under lazy materialization the freeze is split: each value-util member is deep-frozen inside its
  // own getter at the moment it is built (see `js/std_lazy.js`), and THIS epilogue (a) locks the
  // `$std` container and (b) deep-freezes the eager DATA members. Crucially it must NOT read the
  // lazy accessor slots — doing so would fire their getters and force-build members the handler
  // never touches. So it inspects descriptors and only recurses into data properties.
  var _freeze = Object.freeze;
  var _names = Object.getOwnPropertyNames;
  var _getDesc = Object.getOwnPropertyDescriptor;
  var _def = Object.defineProperty;
  var deepFreeze = globalThis.__stdFreeze;

  // Container lock: non-extensible + every own slot non-configurable (the lazy accessor slots are
  // already non-configurable getter-only from setup). `Object.freeze` inspects property descriptors
  // only — it does NOT invoke accessor getters — so no lazy member is built here. A handler can
  // therefore neither add, delete, nor redefine a `$std` member.
  _freeze($std);

  // Deep-freeze the eager DATA members (json/log/emit, plus any eagerly-injected capability member
  // such as io/http/s3), skipping accessor slots so untouched lazy members stay unbuilt. Each lazy
  // member is deep-frozen in its own getter, so the whole surface is tamper-proof either way.
  var names = _names($std);
  for (var i = 0; i < names.length; i++) {
    var d = _getDesc($std, names[i]);
    if (d && "value" in d) deepFreeze(d.value);
  }

  // Lock the eager exposed globals non-writable + non-configurable, re-pinning to the canonical
  // `$std` member so any top-level reassignment during the user-script eval is undone. `$` was
  // installed as a locked getter funnel at projection (re-pinning it here would force-build money),
  // so it is intentionally omitted.
  var eager = ["json", "log", "emit"];
  for (var j = 0; j < eager.length; j++) {
    _def(globalThis, eager[j], {
      value: $std[eager[j]],
      writable: false,
      enumerable: true,
      configurable: false,
    });
  }
})();
