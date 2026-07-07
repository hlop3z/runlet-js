// ─────────────────────────────────────────────────────────────────────────────
// `mail` — SMTP send (present when `config.io.mail` names a resource)
// ─────────────────────────────────────────────────────────────────────────────

/** Options for {@link Mail.send}. A single address or a list is accepted. */
interface MailOptions {
  /** Sender address. Defaults to the mail resource's configured `from` if omitted. */
  from?: string;
  /** Recipient(s). */
  to?: string | string[];
  /** Carbon-copy recipient(s). */
  cc?: string | string[];
  /** Blind-carbon-copy recipient(s). */
  bcc?: string | string[];
  /** `Reply-To` address. */
  reply_to?: string;
  /** Subject line. */
  subject?: string;
  /** Plain-text body. */
  text?: string;
  /** HTML body. */
  html?: string;
}

/** Result of {@link Mail.send}. */
interface MailResult {
  /** `true` if the SMTP server returned a positive (2xx) reply. */
  accepted: boolean;
  /** The SMTP server's response line. */
  response: string;
}

/**
 * SMTP mailer over an **operator-defined** logical resource named in
 * `config.io.mail` (relay + credentials resolved operator-side), so it is
 * trusted — no SSRF guard. Throws on a send failure.
 */
interface Mail {
  /**
   * Sends one email.
   * @example mail.send({ to: ctx.email, subject: "Hi", text: "Hello!" });
   */
  send(opts: MailOptions): MailResult;
}

/** SMTP mailer. Present only when `config.io.mail` names a resource. */
declare const mail: Mail;
