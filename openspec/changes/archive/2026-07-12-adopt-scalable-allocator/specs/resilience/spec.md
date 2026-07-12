## ADDED Requirements

### Requirement: Parallel execution scaling

The system SHALL execute admitted concurrent requests in parallel across the available CPU cores, so
that aggregate execution throughput increases with the number of concurrent workers up to the core
count. Concurrent executions SHALL NOT serialize on a shared process-global resource (such as the
memory allocator), so the Tier 1 concurrency bulkhead's admitted parallelism translates into
cross-core throughput rather than being negated by a serialized execution substrate. This is a
non-functional guarantee: it changes no `/execute` output, sandbox isolation, or determinism
property, only the throughput achieved under concurrency.

Rationale, the sweep methodology, and the measured before/after figures (root cause: musl-`malloc`
lock contention; fix: a scalable per-thread-arena global allocator): `docs/design/allocator-scaling.md`.

#### Scenario: Throughput scales with concurrent workers

- **WHEN** N worker threads (with N no greater than the host core count) each run compute-only
  executions concurrently on a warm host
- **THEN** aggregate throughput increases materially with N (approximately proportional to N),
  rather than remaining flat at the single-worker rate

#### Scenario: Executions do not serialize on the allocator

- **WHEN** many executions run concurrently across cores, each allocating and freeing heavily inside
  the QuickJS engine
- **THEN** per-core throughput efficiency stays high (it does not collapse toward the
  one-execution-at-a-time rate), because allocation is served by a scalable, per-thread-arena
  allocator rather than a globally serialized one

#### Scenario: Single-threaded throughput not regressed

- **WHEN** a single worker runs executions with no added concurrency
- **THEN** single-worker throughput is no lower than before the change (the scalable allocator does
  not regress the uncontended path)
