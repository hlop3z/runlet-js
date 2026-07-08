## ADDED Requirements

### Requirement: /execute response carries a gateway-gated diagnostic logs channel

The `/execute` response SHALL carry a top-level `logs` list of structured diagnostic entries when the
**trusted gateway** requests diagnostic capture for that request, and SHALL omit the field
otherwise. When present it SHALL appear on **both** the success and error paths (capture-on-failure).
The field is additive: the existing `data`, `error`, `meta`, and `effects` fields and the HTTP
status-class projection are unchanged, and a response for which no logs were requested SHALL be
identical to the prior `{data, error, meta, effects?}` contract. The capture request SHALL be
honored only from the trusted gateway; a debug/capture flag asserted by an untrusted caller SHALL
NOT cause logs to be surfaced.

#### Scenario: A trusted debug run surfaces logs inline

- **WHEN** the trusted gateway requests diagnostic capture and the handler calls `log.info(...)` and
  returns normally
- **THEN** the 2xx response carries a `logs` list including that entry, alongside the usual
  `data`/`error`/`meta`

#### Scenario: A failing trusted debug run surfaces the partial trail

- **WHEN** the trusted gateway requests diagnostic capture and the handler logs, then produces an
  error
- **THEN** the non-2xx response still carries the entries logged before the failure

#### Scenario: A normal request omits the logs field

- **WHEN** a request runs without the trusted gateway requesting diagnostic capture
- **THEN** the response has no `logs` field and is otherwise identical to the prior
  `{data, error, meta, effects?}` contract

#### Scenario: A caller-asserted debug flag does not surface logs

- **WHEN** an untrusted caller sets a debug/capture flag in the request but the trusted gateway did
  not request capture
- **THEN** the response carries no `logs` field

