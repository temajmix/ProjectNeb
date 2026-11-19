import type { Env } from "./env.js";
import {
  base64UrlToBytes,
  bytesToBase64Url,
  loadPublicKey,
  loadSigningKey,
  publicJwkForResponse,
  randomToken,
  sha256Hex,
  textDecoder,
  timingSafeEqual,
  utf8,
  type PublicKeyInfo,
} from "./crypto.js";
import { ApiError } from "./errors.js";
import { nowMs, nowSeconds, type SubscriptionRow, type UserRow } from "./types.js";

const ISSUER = "projectneb";
const AUDIENCE = "projectneb-client";

export interface JwtClaims {
  iss?: string;
  aud?: string;
  sub?: string;
  iat?: number;
  exp?: number;
  kind?: string;
  [key: string]: unknown;
}

export interface SessionClaims extends JwtClaims {
  kind: "session";
  sub: string;
  sid: string;
}

export interface LicenseClaims extends JwtClaims {
  kind: "license";
  sub: string;
  plan: string;
  until: number;
  devices: string[];
}

export async function signJwt(
  payload: JwtClaims,
  privateKey: CryptoKey,
  kid: string,
): Promise<string> {
  const header = { alg: "EdDSA", typ: "JWT", kid };
  const encodedHeader = bytesToBase64Url(utf8(JSON.stringify(header)));
  const encodedPayload = bytesToBase64Url(utf8(JSON.stringify(payload)));
  const signingInput = `${encodedHeader}.${encodedPayload}`;
  const signature = await crypto.subtle.sign("Ed25519", privateKey, utf8(signingInput));
  return `${signingInput}.${bytesToBase64Url(new Uint8Array(signature))}`;
}

export async function verifyJwt(token: string, publicKey: CryptoKey): Promise<JwtClaims> {
  const parts = token.split(".");
  if (parts.length !== 3) {
    throw new ApiError(401, "invalid_token", "Malformed token");
  }
  const [encodedHeader, encodedPayload, encodedSignature] = parts as [string, string, string];

  let header: { alg?: string; typ?: string; kid?: string };
  let claims: JwtClaims;
  try {
    header = JSON.parse(textDecoder.decode(base64UrlToBytes(encodedHeader)));
    claims = JSON.parse(textDecoder.decode(base64UrlToBytes(encodedPayload)));
  } catch {
    throw new ApiError(401, "invalid_token", "Malformed token");
  }

  if (header.alg !== "EdDSA") {
    throw new ApiError(401, "invalid_token", "Unsupported token algorithm");
  }

  let valid = false;
  try {
    valid = await crypto.subtle.verify(
      "Ed25519",
      publicKey,
      base64UrlToBytes(encodedSignature),
      utf8(`${encodedHeader}.${encodedPayload}`),
    );
  } catch {
    throw new ApiError(401, "invalid_token", "Malformed token signature");
  }
  if (!valid) {
    throw new ApiError(401, "invalid_signature", "Token signature is not valid");
  }

  if (typeof claims.exp !== "number" || !Number.isFinite(claims.exp)) {
    throw new ApiError(401, "invalid_token", "Token has no expiry");
  }
  if (claims.exp * 1000 <= nowMs()) {
    throw new ApiError(401, "token_expired", "Token has expired");
  }
  if (typeof claims.iat === "number" && claims.iat * 1000 > nowMs() + 5 * 60 * 1000) {
    throw new ApiError(401, "invalid_token", "Token is issued in the future");
  }
  if (typeof claims.sub !== "string" || !claims.sub) {
    throw new ApiError(401, "invalid_token", "Token has no subject");
  }

  return claims;
}

/** Verifies with the current key, then the optional previous key. */
export async function verifyTokenWithEnv(env: Env, token: string): Promise<JwtClaims> {
  const keys = await getPublicKeys(env);
  let lastError: unknown = new ApiError(401, "invalid_token", "Token could not be verified");

  for (const info of keys) {
    try {
      return await verifyJwt(token, info.key);
    } catch (error) {
      // A signature-valid token with a bad expiry must not be retried as if
      // the signature was wrong; report it directly.
      if (error instanceof ApiError && error.code === "token_expired") throw error;
      lastError = error;
    }
  }

  if (lastError instanceof ApiError) throw lastError;
  throw new ApiError(401, "invalid_token", "Token could not be verified");
}

export async function getPublicKeys(env: Env): Promise<PublicKeyInfo[]> {
  const signing = await loadSigningKey(env.LICENSE_PRIVATE_KEY);
  const keys: PublicKeyInfo[] = [
    { key: signing.publicKey, jwk: signing.publicJwk, kid: signing.kid },
  ];

  const previous = env.LICENSE_PREVIOUS_PUBLIC_KEY?.trim();
  if (previous) {
    try {
      keys.push(await loadPublicKey(previous));
    } catch (error) {
      // A broken rotation key must not take the current key offline.
      console.warn("LICENSE_PREVIOUS_PUBLIC_KEY is not a valid Ed25519 public key", error);
    }
  }
  return keys;
}

export async function jwksResponse(env: Env): Promise<Response> {
  const keys = await getPublicKeys(env);
  return new Response(
    JSON.stringify({ keys: keys.map((key) => publicJwkForResponse(key)) }),
    {
      headers: {
        "content-type": "application/json; charset=utf-8",
        "cache-control": "public, max-age=300",
        "x-content-type-options": "nosniff",
      },
    },
  );
}

export interface IssuedSession {
  token: string;
  sessionId: string;
  expiresAt: number;
}

export async function issueSessionToken(
  env: Env,
  user: UserRow,
  ttlSeconds: number,
): Promise<IssuedSession> {
  const signing = await loadSigningKey(env.LICENSE_PRIVATE_KEY);
  const issuedAt = nowSeconds();
  const sessionId = randomToken(18);
  const expiresAt = nowMs() + ttlSeconds * 1000;

  const token = await signJwt(
    {
      iss: ISSUER,
      aud: AUDIENCE,
      sub: user.discord_id,
      sid: sessionId,
      kind: "session",
      iat: issuedAt,
      exp: issuedAt + ttlSeconds,
    },
    signing.privateKey,
    signing.kid,
  );

  await env.DB.prepare(
    `INSERT INTO sessions (id, user_id, token_hash, created_at, expires_at)
     VALUES (?, ?, ?, ?, ?)`,
  )
    .bind(sessionId, user.id, await sha256Hex(token), nowMs(), expiresAt)
    .run();

  return { token, sessionId, expiresAt };
}

export interface IssuedLicense {
  token: string;
  expiresAt: number;
  claims: LicenseClaims;
}

export async function issueLicenseToken(
  env: Env,
  user: UserRow,
  subscription: SubscriptionRow | { plan: string; until: number },
  deviceIds: string[],
  ttlSeconds: number,
): Promise<IssuedLicense> {
  const signing = await loadSigningKey(env.LICENSE_PRIVATE_KEY);
  const issuedAt = nowSeconds();
  const expiresAtSeconds = issuedAt + ttlSeconds;
  const claims: LicenseClaims = {
    iss: ISSUER,
    aud: AUDIENCE,
    sub: user.discord_id,
    plan: subscription.plan,
    until: Math.floor(subscription.until / 1000),
    devices: deviceIds,
    kind: "license",
    iat: issuedAt,
    exp: expiresAtSeconds,
  };

  const token = await signJwt(claims, signing.privateKey, signing.kid);
  return { token, expiresAt: expiresAtSeconds * 1000, claims };
}

export interface AuthenticatedSession {
  user: UserRow;
  sessionId: string;
  claims: SessionClaims;
}

interface SessionUserRow extends UserRow {
  session_id: string;
  session_token_hash: string;
  session_expires_at: number;
  session_revoked_at: number | null;
}

export async function authenticateRequest(
  env: Env,
  request: Request,
): Promise<AuthenticatedSession> {
  const header = request.headers.get("authorization");
  if (!header) {
    throw new ApiError(401, "unauthorized", "Missing Authorization header");
  }
  const match = /^Bearer\s+(.+)$/i.exec(header.trim());
  if (!match?.[1]) {
    throw new ApiError(401, "unauthorized", "Authorization header must use Bearer");
  }

  const token = match[1].trim();
  const claims = await verifyTokenWithEnv(env, token);
  if (claims.kind !== "session" || typeof claims.sid !== "string" || !claims.sid) {
    throw new ApiError(401, "invalid_token", "Token is not a session token");
  }

  const row = await env.DB.prepare(
    `SELECT u.id, u.discord_id, u.username, u.avatar, u.email, u.created_at, u.updated_at,
            s.id AS session_id, s.token_hash AS session_token_hash,
            s.expires_at AS session_expires_at, s.revoked_at AS session_revoked_at
     FROM sessions s
     JOIN users u ON u.id = s.user_id
     WHERE s.id = ?`,
  )
    .bind(claims.sid)
    .first<SessionUserRow>();

  if (!row) {
    throw new ApiError(401, "invalid_token", "Session not found");
  }
  if (claims.sub !== row.discord_id) {
    throw new ApiError(401, "invalid_token", "Session subject does not match");
  }
  if (row.session_revoked_at !== null) {
    throw new ApiError(401, "session_revoked", "Session has been revoked");
  }
  if (row.session_expires_at <= nowMs()) {
    throw new ApiError(401, "session_expired", "Session has expired");
  }

  const expectedHash = utf8(await sha256Hex(token));
  if (!timingSafeEqual(expectedHash, utf8(row.session_token_hash))) {
    throw new ApiError(401, "invalid_token", "Session token does not match");
  }

  const user: UserRow = {
    id: row.id,
    discord_id: row.discord_id,
    username: row.username,
    avatar: row.avatar,
    email: row.email,
    created_at: row.created_at,
    updated_at: row.updated_at,
  };

  return {
    user,
    sessionId: row.session_id,
    claims: claims as SessionClaims,
  };
}

export { ISSUER, AUDIENCE };
