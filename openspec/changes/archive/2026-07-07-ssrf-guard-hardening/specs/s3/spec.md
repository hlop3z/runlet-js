# s3 Specification (delta)

## MODIFIED Requirements

### Requirement: Operator-supplied endpoint under the http SSRF guard

The object-store endpoint and credentials SHALL be operator-supplied in `config.s3` (trusted,
like `db`/`mail`) and the endpoint host SHALL pass the same SSRF guard as `http`: only `http`/`https`
schemes are accepted, and localhost / private / internal addresses are blocked so a presigned
URL can never name a local or internal target (relaxed only in `debug` mode). For operations that
make an outbound connection (usage listing, delete, and any signed send), the address the client
connects to SHALL be the classifier-validated address (connect-time pinning), closing the
rebinding window between endpoint validation and connection even though the endpoint is
operator-supplied. Presign operations produce a URL only and perform no connection, so the host is
validated at sign time and there is nothing to pin.

#### Scenario: Non-http scheme rejected

- **WHEN** `config.s3.endpoint` uses a scheme other than `http://` or `https://`
- **THEN** the operation fails and no URL is produced

#### Scenario: Private or internal host blocked

- **WHEN** `config.s3.endpoint` resolves to localhost or a private/internal address and the server is not in `debug` mode
- **THEN** the operation is blocked by the SSRF guard

#### Scenario: Outbound operation pins the validated address

- **WHEN** a connecting operation (list, delete, signed send) targets an operator endpoint whose hostname resolves differently between validation and connection
- **THEN** the operation connects only to the classifier-validated address and refuses a rebinding to a private/internal address

#### Scenario: Public store supported

- **WHEN** `config.s3.endpoint` names a public S3-compatible store (AWS S3, Cloudflare R2, Backblaze B2, or a publicly reachable MinIO), with `path_style` selecting virtual-hosted or path addressing
- **THEN** signing and store operations target that endpoint
