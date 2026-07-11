(function () {
  // One native bridge: __template(op, source, arg2, arg3). For `render`, arg2 is the JSON context
  // and arg3 the `{"html","missing"}` options; `check`/`fields` use only `source`.
  function call(op, source, arg2, arg3) {
    var raw = __template(
      op,
      source,
      arg2 === undefined ? "" : arg2,
      arg3 === undefined ? "" : arg3
    );
    var res = JSON.parse(raw);
    if (res && res.error) throw new Error(res.error);
    return res;
  }

  // A compiled template: the source is validated at construction (see `make`), then re-compiled per
  // render on the Rust side (stateless across the FFI boundary). `_missing` is the placeholder for
  // undefined merge tags; `.missing()` returns a new object so the value is immutable.
  function Compiled(source, mode, miss) {
    this.source = source;
    this.mode = mode;
    this._missing = miss === undefined ? "" : String(miss);
  }

  Compiled.prototype.render = function (context) {
    var opts = JSON.stringify({ html: this.mode === "html", missing: this._missing });
    var ctx = JSON.stringify(context === undefined || context === null ? {} : context);
    return call("render", this.source, ctx, opts).v;
  };

  // Return a new template whose undefined merge tags render as `placeholder` (immutable).
  Compiled.prototype.missing = function (placeholder) {
    return new Compiled(this.source, this.mode, placeholder);
  };

  // The top-level merge tags this template references (sorted). Useful for "what data does this
  // template need?" before rendering.
  Compiled.prototype.fields = function () {
    return call("fields", this.source, "", "").list;
  };

  // Compile-check the source eagerly so a syntax error throws at `html(...)`/`text(...)`, not at
  // first render, then build the compiled object in the requested escaping mode.
  function make(source, mode) {
    var src = String(source);
    call("check", src, "", "");
    return new Compiled(src, mode, "");
  }

  $std.template = {
    // HTML auto-escaping ON — for invoices, HTML email, anything shown in a browser.
    html: function (source) { return make(source, "html"); },
    // No escaping — for plain email, SMS, receipts.
    text: function (source) { return make(source, "text"); },
  };
})();
