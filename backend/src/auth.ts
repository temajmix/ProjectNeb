import { config, type Env } from "./env.js";
import { randomToken, sha256Hex } from "./crypto.js";
import { ApiError } from "./errors.js";
import { buildSessionRedirect, parseLoopbackReturn } from "./return-url.js";
import { issueSessionToken } from "./tokens.js";
import { nowMs, type UserRow } from "./types.js";

const DISCORD_AUTHORIZE_URL = "https://discord.com/oauth2/authorize";
const DISCORD_TOKEN_URL = "https://discord.com/api/oauth2/token";
const DISCORD_USER_URL = "https://discord.com/api/users/@me";

function requireString(value: string | undefined, name: string): string {
  if (!value?.trim()) {
    throw new ApiError(500, "server_misconfigured", `${name} is not configured`);
  }
  return value.trim();
}

export async function handleAuthStart(env: Env, url: URL): Promise<Response> {
  const rawReturn = url.searchParams.get("return");
  if (!rawReturn) {
    throw new ApiError(400, "missing_return", "return query parameter is required");
  }
  if (rawReturn.length > 2048) {
    throw new ApiError(400, "invalid_return_url", "return is too long");
  }
  const returnUrl = parseLoopbackReturn(rawReturn);
  const clientId = requireString(env.DISCORD_CLIENT_ID, "DISCORD_CLIENT_ID");
  const redirectUri = requireString(env.DISCORD_REDIRECT_URI, "DISCORD_REDIRECT_URI");

  const state = randomToken(32);
  const stateHash = await sha256Hex(state);
  const createdAt = nowMs();
  const expiresAt = createdAt + config.oauthStateTtlSeconds(env) * 1000;

  await env.DB.batch([
    env.DB.prepare(`DELETE FROM oauth_states WHERE expires_at < ?`).bind(
      createdAt - 24 * 60 * 60 * 1000,
    ),
    env.DB.prepare(
      `INSERT INTO oauth_states (state_hash, return_url, created_at, expires_at)
       VALUES (?, ?, ?, ?)`,
    ).bind(stateHash, returnUrl, createdAt, expiresAt),
  ]);

  const authorize = new URL(DISCORD_AUTHORIZE_URL);
  authorize.searchParams.set("client_id", clientId);
  authorize.searchParams.set("redirect_uri", redirectUri);
  authorize.searchParams.set("response_type", "code");
  authorize.searchParams.set("scope", "identify");
  authorize.searchParams.set("state", state);

  return new Response(null, {
    status: 302,
    headers: { location: authorize.toString(), "cache-control": "no-store" },
  });
}

interface DiscordTokenResponse {
  access_token?: string;
  token_type?: string;
  expires_in?: number;
  scope?: string;
}

interface DiscordUserResponse {
  id?: string;
  username?: string;
  global_name?: string | null;
  avatar?: string | null;
  email?: string | null;
}

export type FetchLike = (input: string, init?: RequestInit) => Promise<Response>;

export interface AuthDependencies {
  fetch: FetchLike;
}

const defaultDependencies: AuthDependencies = {
  fetch: (input, init) => fetch(input, init),
};

async function exchangeDiscordCode(
  env: Env,
  code: string,
  deps: AuthDependencies,
): Promise<string> {
  const clientId = requireString(env.DISCORD_CLIENT_ID, "DISCORD_CLIENT_ID");
  const clientSecret = requireString(env.DISCORD_CLIENT_SECRET, "DISCORD_CLIENT_SECRET");
  const redirectUri = requireString(env.DISCORD_REDIRECT_URI, "DISCORD_REDIRECT_URI");

  const response = await deps.fetch(DISCORD_TOKEN_URL, {
    method: "POST",
    headers: {
      "content-type": "application/x-www-form-urlencoded",
      accept: "application/json",
    },
    body: new URLSearchParams({
      client_id: clientId,
      client_secret: clientSecret,
      grant_type: "authorization_code",
      code,
      redirect_uri: redirectUri,
    }),
  });

  if (!response.ok) {
    console.warn("discord token exchange failed", response.status);
    throw new ApiError(502, "discord_exchange_failed", "Discord rejected the authorization code");
  }

  const data = (await response.json()) as DiscordTokenResponse;
  if (!data.access_token) {
    throw new ApiError(502, "discord_exchange_failed", "Discord returned no access token");
  }
  return data.access_token;
}

async function fetchDiscordUser(
  accessToken: string,
  deps: AuthDependencies,
): Promise<DiscordUserResponse> {
  const response = await deps.fetch(DISCORD_USER_URL, {
    headers: { authorization: `Bearer ${accessToken}`, accept: "application/json" },
  });
  if (!response.ok) {
    console.warn("discord user request failed", response.status);
    throw new ApiError(502, "discord_user_failed", "Could not load the Discord profile");
  }
  return (await response.json()) as DiscordUserResponse;
}

async function upsertUser(env: Env, profile: DiscordUserResponse): Promise<UserRow> {
  const discordId = typeof profile.id === "string" ? profile.id.trim() : "";
  if (!discordId || discordId.length > 64) {
    throw new ApiError(502, "discord_user_failed", "Discord profile has no valid id");
  }

  const username =
    (typeof profile.username === "string" && profile.username.trim()) ||
    (typeof profile.global_name === "string" && profile.global_name.trim()) ||
    `discord-${discordId}`;
  const avatar = typeof profile.avatar === "string" ? profile.avatar : null;
  const email = typeof profile.email === "string" && profile.email.trim()
    ? profile.email.trim().toLowerCase()
    : null;
  const now = nowMs();

  await env.DB.prepare(
    `INSERT INTO users (discord_id, username, avatar, email, created_at, updated_at)
     VALUES (?, ?, ?, ?, ?, ?)
     ON CONFLICT(discord_id) DO UPDATE SET
       username = excluded.username,
       avatar = excluded.avatar,
       email = COALESCE(excluded.email, users.email),
       updated_at = excluded.updated_at`,
  )
    .bind(discordId, username.slice(0, 100), avatar, email, now, now)
    .run();

  const user = await env.DB.prepare(`SELECT * FROM users WHERE discord_id = ?`)
    .bind(discordId)
    .first<UserRow>();
  if (!user) {
    throw new ApiError(500, "internal_error", "Could not persist the user");
  }
  return user;
}

function redirectWithSession(returnUrl: string, token: string): Response {
  // The stored return URL was validated at /auth/discord/start. Revalidate it
  // here so a tampered D1 row cannot produce an off-policy redirect.
  const target = buildSessionRedirect(parseLoopbackReturn(returnUrl), token);
  return new Response(null, {
    status: 302,
    headers: {
      location: target,
      "cache-control": "no-store",
      // The loopback query carries the session so the desktop listener can read
      // it without parsing the fragment. Suppress Referer on any follow-up.
      "referrer-policy": "no-referrer",
    },
  });
}

export async function handleAuthCallback(
  env: Env,
  url: URL,
  deps: AuthDependencies = defaultDependencies,
): Promise<Response> {
  const state = url.searchParams.get("state");
  if (!state) {
    throw new ApiError(400, "invalid_state", "OAuth state is missing");
  }
  if (state.length > 512) {
    throw new ApiError(400, "invalid_state", "OAuth state is not valid");
  }

  const stateHash = await sha256Hex(state);
  const row = await env.DB.prepare(
    `SELECT state_hash, return_url, created_at, expires_at, used_at
     FROM oauth_states WHERE state_hash = ?`,
  )
    .bind(stateHash)
    .first<{
      state_hash: string;
      return_url: string;
      created_at: number;
      expires_at: number;
      used_at: number | null;
    }>();

  if (!row) {
    throw new ApiError(400, "invalid_state", "OAuth state is unknown");
  }
  if (row.used_at !== null) {
    throw new ApiError(400, "state_already_used", "OAuth state has already been used");
  }
  if (row.expires_at <= nowMs()) {
    await env.DB.prepare(`DELETE FROM oauth_states WHERE state_hash = ?`).bind(stateHash).run();
    throw new ApiError(400, "expired_state", "OAuth state has expired");
  }

  if (url.searchParams.get("error")) {
    throw new ApiError(
      400,
      "discord_denied",
      url.searchParams.get("error_description") ?? "Discord authorization was denied",
    );
  }

  const code = url.searchParams.get("code");
  if (!code) {
    throw new ApiError(400, "missing_code", "Discord did not return an authorization code");
  }

  const consumed = await env.DB.prepare(
    `UPDATE oauth_states SET used_at = ? WHERE state_hash = ? AND used_at IS NULL`,
  )
    .bind(nowMs(), stateHash)
    .run();
  if ((consumed.meta.changes ?? 0) === 0) {
    throw new ApiError(400, "state_already_used", "OAuth state has already been used");
  }

  const accessToken = await exchangeDiscordCode(env, code, deps);
  const profile = await fetchDiscordUser(accessToken, deps);
  const user = await upsertUser(env, profile);
  const session = await issueSessionToken(env, user, config.sessionTtlSeconds(env));

  return redirectWithSession(row.return_url, session.token);
}

/** Exported for tests. */
export const __test = { DISCORD_TOKEN_URL, DISCORD_USER_URL, redirectWithSession };
