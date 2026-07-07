// ─────────────────────────────────────────────────────────────────────────────
// `auth` — OIDC/IAM identity (present when `config.io.auth` names a resource)
// ─────────────────────────────────────────────────────────────────────────────

/** A valid token's resolved identity claims (`sub`, `email`, `name`, …). */
type AuthClaims = Record<string, unknown>;

/** {@link Auth.user_info} result — a discriminated union on `ok`. */
type AuthUserInfo =
  | { ok: true; claims: AuthClaims }
  | { ok: false; status: number; code: "AUTH_INVALID_TOKEN" };

/** {@link Auth.introspect} result (RFC 7662). Read `claims.active` to gate. */
interface AuthIntrospection {
  /** Always `true` on a successful round-trip (the IAM answered). */
  ok: true;
  /** The introspection response; `claims.active` tells you if the token is live. */
  claims: AuthClaims & { active: boolean };
}

/**
 * OIDC/IAM identity over the **operator-defined** logical resource named in
 * `config.io.auth` (the issuer + credentials are resolved operator-side), so it is
 * trusted — no SSRF guard. Validation is delegated to the IAM (a `userinfo`
 * round-trip), so there is no local JWT/JWKS crypto. Endpoints are auto-discovered
 * from `{issuer}/.well-known/openid-configuration` unless overridden in the
 * resource's config.
 *
 * **Hybrid errors:** a token-validity outcome is the caller's business flow, so an
 * invalid/expired/insufficient-scope token comes back **in-band** (`{ ok: false }`)
 * and never throws. Infra failures the handler can't act on (issuer down, misconfig)
 * **throw** a tagged capability error (`AUTH_UNAVAILABLE` / `AUTH_REQUEST`).
 */
interface Auth {
  /**
   * Resolves a bearer token's claims via the IAM userinfo endpoint.
   * @example
   * const u = auth.user_info(ctx.token);
   * if (!u.ok) return json(null, { code: "unauthorized" });
   * return json({ id: u.claims.sub }, null);
   */
  user_info(token: string): AuthUserInfo;
  /**
   * RFC 7662 token introspection. Needs the auth resource's `client_id` / `client_secret`.
   * @example const r = auth.introspect(ctx.token); if (!r.claims.active) { ... }
   */
  introspect(token: string): AuthIntrospection;
}

/** OIDC/IAM identity helper. Present only when `config.io.auth` names a resource. */
declare const auth: Auth;
