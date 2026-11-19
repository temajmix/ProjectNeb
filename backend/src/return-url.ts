import { ApiError } from "./errors.js";

/**
 * Desktop sign-in uses a loopback listener. The only URL the API will redirect
 * a session to is `http://127.0.0.1:<port>/callback` or
 * `http://localhost:<port>/callback`.
 *
 * The pattern is matched against the raw string so that URL-parser
 * normalisation (default-port removal, userinfo handling, trailing dots,
 * unicode hosts) cannot widen the policy. It also rejects custom schemes
 * (`projectneb:`, `file:`), HTTPS, remote hosts and any path or query.
 */
const LOOPBACK_RETURN_PATTERN =
  /^http:\/\/(127\.0\.0\.1|localhost):([0-9]{1,5})\/callback$/i;

export const RETURN_URL_POLICY_HINT =
  "return must be http://127.0.0.1:<port>/callback or http://localhost:<port>/callback";

export function isLoopbackReturnUrl(value: string): boolean {
  return LOOPBACK_RETURN_PATTERN.test(value);
}

/**
 * Validates `return` and returns the canonical form. Throws a 400
 * `invalid_return_url` for anything outside the loopback policy.
 */
export function parseLoopbackReturn(raw: string): string {
  const trimmed = raw.trim();
  const match = LOOPBACK_RETURN_PATTERN.exec(trimmed);
  if (!match) {
    throw new ApiError(400, "invalid_return_url", RETURN_URL_POLICY_HINT);
  }

  const host = (match[1] as string).toLowerCase();
  const port = Number.parseInt(match[2] as string, 10);
  if (!Number.isInteger(port) || port < 1 || port > 65_535) {
    throw new ApiError(400, "invalid_return_url", "return port must be between 1 and 65535");
  }

  // Keep the explicit port even for :80, where URL serialisation would drop it.
  return `http://${host}:${port}/callback`;
}

/**
 * Builds the final client redirect.
 *
 * The session is always placed in the fragment (never sent to a server).
 * For loopback returns only, it is also mirrored into the query string so the
 * desktop listener can read either form. The API refuses non-loopback returns,
 * so the query form can never be attached to a remote origin.
 */
export function buildSessionRedirect(returnUrl: string, token: string): string {
  const encoded = encodeURIComponent(token);
  const fragment = `#session=${encoded}`;
  return isLoopbackReturnUrl(returnUrl)
    ? `${returnUrl}?session=${encoded}${fragment}`
    : `${returnUrl}${fragment}`;
}

export const __test = { LOOPBACK_RETURN_PATTERN };
