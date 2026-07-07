// ─────────────────────────────────────────────────────────────────────────────
// `amq` — messaging producer: RabbitMQ or NATS (present when `config.io.amq` names a resource)
// ─────────────────────────────────────────────────────────────────────────────

/**
 * A single message: `[routingKey, payload]`. The payload is published as its JSON bytes.
 * `routingKey` is a RabbitMQ routing key (queue name on the default exchange) or, on the
 * `nats` backend, the subject.
 */
type AmqMessage = [routingKey: string, payload: unknown];

/**
 * Messaging **producer** over the **operator-defined** logical resource named in
 * `config.io.amq` (trusted, resolved operator-side — no SSRF guard). The backend is the
 * resource's configured `backend`: `"rabbitmq"` (default) or `"nats"`. **Producer-side
 * only** — publish on both backends, request-reply on `nats`; there is no subscribe/consume.
 * Synchronous; each call is one op against `max_ops`. A broker outage throws a retryable
 * `AMQ_CONNECTION`; a batch larger than the resource's `max_batch` throws `AMQ_BATCH_TOO_LARGE`.
 */
interface Amq {
  /**
   * Publishes a batch and returns the number published. `routingKey` is the RabbitMQ queue
   * name (default exchange; override via the resource's `exchange`) or the NATS subject.
   * @example amq.send([["user.created", { id: 1 }], ["user.created", { id: 2 }]]); // → 2
   */
  send(messages: AmqMessage[]): number;
  /**
   * **NATS backend only.** Publishes a request to `subject` and returns the reply's parsed
   * JSON body, bounded by the resource's `request_timeout_ms`. Throws `AMQ_UNSUPPORTED` on
   * the RabbitMQ backend and a retryable `AMQ_TIMEOUT` when no reply arrives in time.
   * @example const pong = amq.request("service.ping", { hi: true });
   */
  request<T = any>(subject: string, payload?: unknown): T;
}

/** Messaging producer. Present only when `config.io.amq` names a resource. */
declare const amq: Amq;
