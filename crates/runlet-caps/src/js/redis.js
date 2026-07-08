(function() {
  // Bound to the `redis` capability via io.channel (throws a tagged capability error on failure).
  var call = io.channel('redis');
  globalThis.redis = {
    // strings in/out; get of a missing key returns null. Synchronous (no await).
    get: function(key) { return call('get', { key: key }).value; },
    set: function(key, value, opts) {
      opts = opts || {};
      // ttl is optional, in seconds. Value is coerced to a string (the script owns JSON).
      return call('set', { key: key, value: String(value), ttl: opts.ttl }).ok;
    },
    delete: function(key) { return call('delete', { key: key }).count; },
    increment: function(key) { return call('increment', { key: key }).value; },
    expire: function(key, seconds) { return call('expire', { key: key, seconds: seconds }).set; }
  };
})();
