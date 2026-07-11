(function () {
  // The `$std` bootstrap — the FIRST injected script (before any util/capability IIFE). Every
  // built-in is defined once as a member of this object; the bare globals a script sees are a thin
  // PROJECTION of a curated subset (see `js/std_project.js`), never independent definitions.
  globalThis.$std = {};

  // The single declarative exposure list (D1): maps a global name → the `$std` member it mirrors.
  // Each mirror is the SAME object reference as its `$std` member, so `$ === $std.money`. This is
  // the one source of truth for "what is a bare global"; the projection and freeze epilogues both
  // read it, and the `.d.ts` derives the mirror declares from the same set.
  //
  // ONLY pure, both-profile members are eligible (D2): an exposed global is a second reference to a
  // `$std` member, so a prunable ambient authority (`datetime.now`, `crypto.uuid`, `Math.random`)
  // must NEVER be mirrored — a surviving global alias would defeat the determinism prune. The list
  // therefore holds only `money`/`json`/`log`/`emit`.
  Object.defineProperty(globalThis, "__stdExpose", {
    value: { $: "money", json: "json", log: "log", emit: "emit" },
    configurable: true,
  });
})();
