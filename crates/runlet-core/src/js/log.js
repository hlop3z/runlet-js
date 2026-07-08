(function () {
  // The `log.*` diagnostic channel (Serilog-style message templates + Pino-style bound context).
  // A dedicated global, NOT `console.log`: a stateless box behind a gateway cannot honor "prints to
  // a stream you own", so a captured, policy-routed, sometimes-dropped sink gets an honest name.
  //
  // Levels map to the same ordinals the native floor (`__logFloor`) uses. The floor is checked here
  // FIRST — before merging context or serializing properties — so a below-floor call is near-free on
  // the always-on hot path (D6, Pino's cost trick). `log.with(fields)` derives a logger whose bound
  // context is merged into every subsequent entry, with per-call keys overriding it (OQ3).

  var LEVELS = { trace: 0, debug: 1, info: 2, warn: 3, error: 4 };

  function has(obj, key) {
    return obj != null && Object.prototype.hasOwnProperty.call(obj, key);
  }

  // Shallow-merges `bound` then `extra` into a new object; `extra` wins on a key collision (OQ3).
  function merge(bound, extra) {
    var out = {};
    var k;
    if (bound) for (k in bound) if (has(bound, k)) out[k] = bound[k];
    if (extra) for (k in extra) if (has(extra, k)) out[k] = extra[k];
    return out;
  }

  // Renders a Serilog `{name}` template against the merged properties. An unknown placeholder is
  // left verbatim; an object value is JSON-stringified so the message stays a flat string.
  function render(template, props) {
    return String(template).replace(/\{(\w+)\}/g, function (match, key) {
      if (!has(props, key)) return match;
      var value = props[key];
      if (value === null || value === undefined) return String(value);
      if (typeof value === "object") {
        try {
          return JSON.stringify(value);
        } catch (err) {
          return String(value);
        }
      }
      return String(value);
    });
  }

  function record(levelName, levelNum, bound, template, props) {
    if (levelNum < __logFloor) return; // D6: below the floor, do nothing (no merge, no stringify)
    var merged = merge(bound, props);
    var message = render(template, merged);
    var err = __log(levelName, String(template), JSON.stringify(merged), message);
    if (err) throw new Error(err);
  }

  // Builds a logger bound to `context` (null for the root). Each derived logger shares the native
  // buffer + `seq`; `with` layers additional bound context.
  function make(context) {
    return {
      trace: function (template, props) { record("trace", LEVELS.trace, context, template, props); },
      debug: function (template, props) { record("debug", LEVELS.debug, context, template, props); },
      info: function (template, props) { record("info", LEVELS.info, context, template, props); },
      warn: function (template, props) { record("warn", LEVELS.warn, context, template, props); },
      error: function (template, props) { record("error", LEVELS.error, context, template, props); },
      with: function (fields) { return make(merge(context, fields)); },
    };
  }

  globalThis.log = make(null);
})();
