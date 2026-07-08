# execution Specification (delta)

## MODIFIED Requirements

### Requirement: Single execution endpoint

The system SHALL expose `POST /execute` as the primary execution endpoint, accepting a JSON
body and returning a JSON `{data, error, meta}` envelope. The system additionally exposes
`POST /batch` (see the `batch-execution` capability), whose per-item results carry
the same `{data, error, meta}` envelope; no other execution endpoint exists.

#### Scenario: Successful execution

- **WHEN** a request supplies a handler that returns `json(value, null)`
- **THEN** the response is HTTP 200 with `data` set to `value`, `error` null, and a `meta` object

#### Scenario: Response always carries the envelope shape

- **WHEN** any request to `/execute` completes (success or failure)
- **THEN** the response body has exactly the keys `data`, `error`, and `meta`

#### Scenario: Batch endpoint reuses the envelope per item

- **WHEN** a request to `/batch` completes
- **THEN** every `results[i]` entry carries the same `{data, error, meta}` envelope defined here
