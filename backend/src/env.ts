export interface Env {
  DB: D1Database;

  ENVIRONMENT?: string;
  VERSION?: string;

  /** Ed25519 private key: PKCS#8 base64 or JWK JSON. Worker secret. */
  LICENSE_PRIVATE_KEY: string;
  /** Optional old Ed25519 public key kept during rotation. */
  LICENSE_PREVIOUS_PUBLIC_KEY?: string;

  DISCORD_CLIENT_ID: string;
  DISCORD_CLIENT_SECRET: string;
  DISCORD_REDIRECT_URI: string;

  /** Shared secret for HMAC-SHA256 webhook signatures. Worker secret. */
  BOOSTY_WEBHOOK_SECRET: string;

  MAX_DEVICES?: string;
  RATE_LIMIT_PER_MINUTE?: string;
  LICENSE_TTL_SECONDS?: string;
  SESSION_TTL_SECONDS?: string;
  /** OAuth states live for 10 minutes; never allow a longer window. */
  OAUTH_STATE_TTL_SECONDS?: string;
}

export function intEnv(
  value: string | undefined,
  fallback: number,
  min: number,
  max: number,
): number {
  const parsed = Number.parseInt(value ?? "", 10);
  if (!Number.isFinite(parsed)) return fallback;
  return Math.min(max, Math.max(min, parsed));
}

export const config = {
  maxDevices(env: Env): number {
    return intEnv(env.MAX_DEVICES, 3, 1, 50);
  },
  rateLimitPerMinute(env: Env): number {
    return intEnv(env.RATE_LIMIT_PER_MINUTE, 30, 1, 10_000);
  },
  licenseTtlSeconds(env: Env): number {
    return intEnv(env.LICENSE_TTL_SECONDS, 900, 60, 86_400);
  },
  sessionTtlSeconds(env: Env): number {
    return intEnv(env.SESSION_TTL_SECONDS, 30 * 24 * 60 * 60, 300, 365 * 24 * 60 * 60);
  },
  oauthStateTtlSeconds(env: Env): number {
    // Capped at 10 minutes: states are short-lived CSRF nonces, not sessions.
    return intEnv(env.OAUTH_STATE_TTL_SECONDS, 600, 60, 600);
  },
  version(env: Env): string {
    return env.VERSION?.trim() || "0.1.0";
  },
};
