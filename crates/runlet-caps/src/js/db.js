(function() {
  // Bound to the `db` capability via io.channel (see js/io.js); packs {sql, params} into the one
  // payload and throws a tagged capability error on failure. No direct native call here.
  var send = io.channel('db');
  function call(action, sql, params) {
    return send(action, { sql: sql, params: params || [] });
  }
  globalThis.db = {
    query: function(sql, params) { return call('query', sql, params); },
    execute: function(sql, params) { return call('execute', sql, params); },
    begin: function() { call('begin', '', []); },
    commit: function() { call('commit', '', []); },
    rollback: function() { call('rollback', '', []); }
  };
})();
