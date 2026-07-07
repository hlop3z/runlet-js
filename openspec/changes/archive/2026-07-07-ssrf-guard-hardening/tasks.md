# Tasks: ssrf-guard-hardening

## 1. Classifier completeness (`ssrf.rs`, pure internal)

- [x] 1.1 Add the deprecated IPv4-compatible IPv6 form `::a.b.c.d` (`::/96`, seg0..5 == 0, not loopback/unspecified) to `is_private_v6`: unwrap the embedded v4 and re-check via `is_private_v4`
- [x] 1.2 Add multicast + reserved to the classifier: `is_private_v4` gains `224.0.0.0/4` and `240.0.0.0/4`; `is_private_v6` gains `ff00::/8`
- [x] 1.3 D4 regression tests: assert decimal/octal/hex/short-form literals (`2130706433`, `0x7f000001`, `127.1`, `0177.0.0.1`) canonicalize to a blocked address through the same parse path `validate_url` uses — pins the `url`-crate-inherited normalization
- [x] 1.4 Unit tests for 1.1/1.2 (IPv4-compatible loopback `::7f00:1`, multicast `224.0.0.1`/`ff02::1`, reserved `240.0.0.1`) and confirm a genuine public v6 still passes

## 2. Shared connect-time pinning + scheme allowlist

- [x] 2.1 Extract the `SsrfResolver` + `resolve_filtered` pinning helper from `http.rs` into `ssrf.rs` as a reusable constructor (fail-closed on empty, `allow_private` honored)
- [x] 2.2 D1: add an `http`/`https`-only scheme check to `validate_url` and `validate_redirect`; a non-http(s) URL or redirect target returns/stops with `HTTP_SSRF_BLOCKED` (do not rely on reqwest's supported-scheme set)
- [x] 2.3 D2: install the shared pinned resolver on the `s3` client for connecting operations (list/delete/signed send); leave presign as pure sign-time host validation
- [x] 2.4 Tests: cross-protocol redirect (`https` → `file://`/`gopher://`) is not followed; `s3` list/delete against a rebinding host connects only to the validated address

## 3. Production boot guard

- [x] 3.1 D5: in `runlet/src/config.rs`, reject startup when `allow_private` (debug) or a wildcard `*` allowlist is active on a non-loopback bind unless network isolation is asserted (reuse the trusted-mode isolation flag per the open question)
- [x] 3.2 Tests: relaxed guard + public bind + no isolation → boot error; loopback bind or asserted isolation → boots

## 4. Verification + docs

- [x] 4.1 Full gate: `task check` (fmt, clippy gauntlet, unit tests) — the `ssrf.rs` additions stay `#[expect]`-free and within the complexity thresholds. fmt clean; `cargo clippy -p runlet-core` 0 errors; changed code in `-p runlet` clippy-clean (the only `-p runlet` findings are pre-existing `absolute_paths`/`shadow_unrelated` in `handler.rs`/`identity.rs` test code, and `runlet-wire` `quic.rs`/`wire.rs`, all in files this change does not touch — surfaced only by the newer local clippy 1.96.0 vs CI's pinned toolchain); unit tests: runlet-core 45 passed, runlet 47 passed
- [x] 4.2 Integration coverage: an `api` call with a non-http scheme and a cross-protocol redirect both return `HTTP_SSRF_BLOCKED`. Added to `tests/test_simple.py::test_http_api`; verified live against the running server + httpbin — `file:///etc/passwd` → `{status:0, code:"HTTP_SSRF_BLOCKED"}` (host `unknown`, no connection), redirect → `gopher://localhost/` → `data:302` (not followed), control `GET` → `200`
- [x] 4.3 Docs: `docs/02-api.md` + README note that only `http`/`https` targets are reachable and that the guard is framework-enforced for every `ScriptControlled` capability; record the network-layer egress recommendation as the independent second line (deployment, not code) — added to `docs/security-hardening.md`
