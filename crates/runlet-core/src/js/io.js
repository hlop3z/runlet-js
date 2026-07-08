(function() {
  // Generic egress wrapper over the `__io` FFI. `io.call(name, action, payload)` is the explicit
  // low-level surface; `io.channel(name)` returns a caller bound to one capability name so a
  // per-capability wrapper (db.js, redis.js, …) names its capability exactly once. The `__runlet`
  // tagged-error contract is unwrapped by the shared `__ffi.unwrap` primitive (js/ffi.js) — the
  // same one the in-engine `s3` bypass uses.
  function call(name, action, payload) {
    return __ffi.unwrap(
      __io(name, action, JSON.stringify(payload === undefined ? null : payload))
    );
  }
  function channel(name) {
    return function(action, payload) { return call(name, action, payload); };
  }
  globalThis.io = { call: call, channel: channel };
})();
