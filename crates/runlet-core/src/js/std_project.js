(function () {
  // Projection epilogue (D3 step 3), lazy variant. Runs AFTER the lazy `$std` accessors are
  // installed and BEFORE the user script evals, so a handler sees `$`/`json`/`log`/`emit` as
  // ordinary globals.
  //
  // `$` funnels to the LAZY `$std.money` through a getter, NOT an eager `globalThis.$ = $std.money`
  // read (D3): an eager read would fire money's getter and build the single most expensive util on
  // every request, defeating the change. Routing through the accessor also guarantees identity
  // (`$ === $std.money`): both paths return the one memoized instance. It is installed locked
  // (getter-only, non-configurable) up front — a getter-only property is not writable, so a
  // top-level `$ = …` cannot rebind it, and there is no separate freeze step for it.
  //
  // `json`/`log`/`emit` carry per-request state (buffers) and are cheap, so they stay eager as
  // before — mirrored here and locked non-writable later by the freeze epilogue.
  var _def = Object.defineProperty;
  _def(globalThis, "$", {
    get: function () {
      return $std.money;
    },
    enumerable: true,
    configurable: false,
  });
  globalThis.json = $std.json;
  globalThis.log = $std.log;
  globalThis.emit = $std.emit;
})();
