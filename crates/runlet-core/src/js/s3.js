(function() {
  // Shares the `__runlet` tagged-error unwrap with the mux via `__ffi.unwrap` (js/ffi.js). `s3` is
  // the in-engine bypass (its own `__s3` FFI), so it does not route through `io` — only the unwrap
  // contract is shared, not the transport.
  function call(action, payload) {
    return __ffi.unwrap(__s3(action, JSON.stringify(payload || {})));
  }
  function sign(opts) {
    opts = opts || {};
    return call('presign', {
      method: opts.method || 'PUT',
      key: opts.key || '',
      expires: opts.expires || 0
    });
  }
  globalThis.s3 = {
    // Sign a URL for any method (PUT/GET/HEAD/DELETE). Use the helpers below for the
    // common cases.
    sign_url: sign,
    // Sign an upload (PUT) link.
    upload_url: function(opts) {
      opts = opts || {};
      return sign({ method: 'PUT', key: opts.key, expires: opts.expires });
    },
    // Sign a download (GET) link.
    download_url: function(opts) {
      opts = opts || {};
      return sign({ method: 'GET', key: opts.key, expires: opts.expires });
    },
    // Sign a size-limited browser POST upload form. No size field: the cap comes only
    // from config.s3.max_upload_size.
    upload_form: function(opts) {
      opts = opts || {};
      return call('presign_post', { key: opts.key || '', expires: opts.expires || 0 });
    },
    // Total { prefix, bytes, objects } for a key prefix (e.g. "user-a/").
    usage: function(opts) {
      opts = opts || {};
      return call('usage', { prefix: opts.prefix || '' });
    },
    // Delete one object -> { key, deleted: true }. Throws unless the operator set
    // config.s3.allow_delete = true (destructive, so it is opt-in).
    delete: function(opts) {
      opts = opts || {};
      return call('delete', { key: opts.key || '' });
    }
  };
})();
