#!/bin/sh
# Per-capability cfg sweep for runlet-core (the deterministic-core feature matrix).
# Runs the real gate (plain `cargo clippy`, NOT --all-targets) for each in-engine capability
# plus the no-capability build. Post composable-capability-core the core exposes exactly two
# capability features — `http` and `s3` (the driver-backed capabilities moved to the
# `runlet-caps` preset crate and are no longer cargo features). fabric-backends (fabricd repo)
# is featureless and not swept.
set -e
for f in NONE http s3; do
  if [ "$f" = "NONE" ]; then
    echo "=== runlet-core --no-default-features ==="
    cargo clippy -p runlet-core --no-default-features --quiet 2>&1 | tail -n 30
  else
    echo "=== runlet-core --no-default-features --features $f ==="
    cargo clippy -p runlet-core --no-default-features --features "$f" --quiet 2>&1 | tail -n 30
  fi
done

# The deterministic-only core links no network dependency: assert `reqwest` is absent from the
# `--no-default-features` build's dependency tree (it enters only via `http`/`s3`).
echo "=== cargo tree: no reqwest without http/s3 ==="
if cargo tree -p runlet-core --no-default-features -i reqwest >/dev/null 2>&1; then
  echo "FAIL: reqwest is linked in the deterministic-only core" >&2
  exit 1
fi
echo "ok: no reqwest in the deterministic-only core"

echo "=== sweep done ==="
