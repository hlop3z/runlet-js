(function () {
  // The `$std` bootstrap — the FIRST injected script (before any util/capability IIFE). Every
  // built-in is defined as a member of this object: the cheap per-request channels (`json`/`log`/
  // `emit`) and any enabled capability (`io`/`http`/`s3`) as eager data members, and every value-util
  // (`decimal`/`money`/`crypto`/`env`/`secrets`/`datetime`/`text`/`list`/`dict`/`template`/`check`) as
  // a LAZY getter-only accessor installed by `js/std_lazy.js`. The bare globals a script sees
  // (`$`/`json`/`log`/`emit`) are a thin PROJECTION of a curated subset (see `js/std_project.js`),
  // never independent definitions.
  globalThis.$std = {};
})();
