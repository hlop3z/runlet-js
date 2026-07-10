## MODIFIED Requirements

### Requirement: Bounded and enumerated mux-bypass surface

The host SHALL enumerate every authority reachable from a script that is not mediated by the
capability mux — the in-engine `http` and `s3` capabilities, and ambient primitives such as the
wall clock, entropy/RNG, and process exit — as a reviewed bypass of the central enforcement. Under
the deterministic profile these ambient authorities SHALL be removed from the context, not merely
gated; a registered-but-disabled import is not acceptable.

#### Scenario: Ambient authority is removed, not gated, under deterministic profile

- **WHEN** an invocation runs with the deterministic profile
- **THEN** the neutralized ambient authorities (time, randomness — the `datetime` clock and `$sys` crypto entropy) are absent from the context such that a script cannot re-reach them, rather than present-but-stubbed in a way that could be un-gated by a later change

#### Scenario: In-engine capabilities are declared as mux bypasses

- **WHEN** the `http` or `s3` capability is injected (they carry their own in-engine code and do not route through the egress mux)
- **THEN** each still enforces its own trust model — `http` applies the SSRF guard, `s3` performs only signing — and both are documented in the enumerated bypass surface so the omission from central mediation is a reviewed decision, not an oversight
