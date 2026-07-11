(function () {
  // The projection epilogue (D3 step 3): mirror the curated `$std` members onto `globalThis`,
  // each as the identical object reference. Runs AFTER every `$std` member is built and BEFORE the
  // user script evals, so a handler sees `$`/`json`/`log`/`emit` as ordinary globals. The bindings
  // are locked non-writable only later, by the freeze epilogue (after the determinism prune), so a
  // top-level tampering attempt is simply overwritten back to the canonical reference at freeze.
  var expose = globalThis.__stdExpose;
  var names = Object.keys(expose);
  for (var i = 0; i < names.length; i++) {
    globalThis[names[i]] = $std[expose[names[i]]];
  }
})();
