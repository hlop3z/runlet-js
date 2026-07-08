(function() {
  // Shared FFI unwrap for the `__runlet` tagged-error contract, defined once here and used by
  // both egress surfaces: the mux (io.js, via `__io`) and the in-engine `s3` bypass (via `__s3`).
  // Each returns a JSON string that is either success data or a tagged error object
  // ({ error, code, retryable, owner, source, details? }); `unwrap` returns the former and throws
  // the latter as an Error carrying `__runlet` so the engine classifies it as a capability error.
  // Injected unconditionally with the bridge (both surfaces are gated independently — `s3` can be
  // active without the mux), so it is present whenever either wrapper runs. `http.js` deliberately
  // does not use it: its transport failures are in-band (`status: 0`), never thrown (§13).
  globalThis.__ffi = {
    unwrap: function(raw) {
      var res = JSON.parse(raw);
      if (res && res.error) {
        var err = new Error(res.error);
        err.__runlet = res;
        throw err;
      }
      return res;
    }
  };
})();
