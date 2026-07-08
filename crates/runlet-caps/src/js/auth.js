(function() {
  // Request-scoped cache: this wrapper is eval'd into a fresh Context per request,
  // so `cache` resets automatically — a repeated lookup makes no network round-trip
  // (and so correctly consumes no max_ops slot). No cross-request/global state.
  var cache = {};

  // Bound to the `auth` capability via io.channel. An infra failure carries `error` and is thrown
  // (tagged); an in-band result ({ ok: ... }) has no `error` → returned as data.
  var send = io.channel('auth');
  function call(action, token) {
    return send(action, { token: token || '' });
  }

  function memo(action, token) {
    var key = action + ':' + (token || '');
    if (Object.prototype.hasOwnProperty.call(cache, key)) return cache[key];
    var res = call(action, token);
    cache[key] = res;
    return res;
  }

  globalThis.auth = {
    // Validate a bearer token via the IAM userinfo endpoint.
    // → { ok: true, claims: {...} } | { ok: false, status, code: "AUTH_INVALID_TOKEN" }
    user_info: function(token) { return memo('user_info', token); },
    // RFC 7662 token introspection (needs config.auth.client_id/secret).
    // → { ok: true, claims: { active, scope, exp, ... } }
    introspect: function(token) { return memo('introspect', token); }
  };
})();
