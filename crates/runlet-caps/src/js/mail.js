(function() {
  function norm(value) {
    if (value === undefined || value === null) return [];
    return Array.isArray(value) ? value : [value];
  }
  // Bound to the `mail` capability via io.channel (throws a tagged capability error on failure).
  var call = io.channel('mail');
  globalThis.mail = {
    send: function(opts) {
      opts = opts || {};
      return call('send', {
        from: opts.from || '',
        to: norm(opts.to),
        cc: norm(opts.cc),
        bcc: norm(opts.bcc),
        reply_to: opts.reply_to || '',
        subject: opts.subject || '',
        text: opts.text || '',
        html: opts.html || ''
      });
    }
  };
})();
