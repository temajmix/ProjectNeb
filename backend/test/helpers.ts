import { createExecutionContext, env } from "cloudflare:test";
import worker from "../src/index.js";
import type { Env } from "../src/env.js";
import { issueSessionToken } from "../src/tokens.js";
import type { SubscriptionRow, UserRow } from "../src/types.js";

export { env };
export const testEnv = env as unknown as Env;

export async function call(
  path: string,
  init: RequestInit = {},
): Promise<Response> {
  const request = new Request(`https://api.test${path}`, init);
  return worker.fetch(request, env, createExecutionContext());
}

export async function apiCall(
  path: string,
  token: string,
  init: RequestInit = {},
): Promise<Response> {
  const headers = new Headers(init.headers);
  headers.set("authorization", `Bearer ${token}`);
  return call(path, { ...init, headers });
}

export async function createUser(
  discordId = "123456789012345678",
  username = "neb-user",
): Promise<UserRow> {
  const now = Date.now();
  await env.DB.prepare(
    `INSERT INTO users (discord_id, username, avatar, email, created_at, updated_at)
     VALUES (?, ?, NULL, NULL, ?, ?)`,
  )
    .bind(discordId, username, now, now)
    .run();

  const user = await env.DB.prepare(`SELECT * FROM users WHERE discord_id = ?`)
    .bind(discordId)
    .first<UserRow>();
  if (!user) throw new Error("failed to create test user");
  return user;
}

export async function createSession(user: UserRow): Promise<string> {
  const session = await issueSessionToken(testEnv, user, 30 * 24 * 60 * 60);
  return session.token;
}

export async function addSubscription(
  user: UserRow,
  options: {
    plan?: string;
    until?: number;
    status?: string;
    email?: string | null;
    source?: string;
  } = {},
): Promise<SubscriptionRow> {
  const now = Date.now();
  const until = options.until ?? now + 30 * 24 * 60 * 60 * 1000;
  const plan = options.plan ?? "pro";
  const status = options.status ?? "active";
  const source = options.source ?? "boosty";

  const result = await env.DB.prepare(
    `INSERT INTO subscriptions
       (discord_id, email, plan, status, until, source, external_ref, created_at, updated_at)
     VALUES (?, ?, ?, ?, ?, ?, NULL, ?, ?)`,
  )
    .bind(user.discord_id, options.email ?? null, plan, status, until, source, now, now)
    .run();

  const row = await env.DB.prepare(`SELECT * FROM subscriptions WHERE id = ?`)
    .bind(Number(result.meta.last_row_id))
    .first<SubscriptionRow>();
  if (!row) throw new Error("failed to create test subscription");
  return row;
}

export async function countRows(table: string): Promise<number> {
  const row = await env.DB.prepare(`SELECT COUNT(*) AS count FROM ${table}`).first<{
    count: number;
  }>();
  return row?.count ?? 0;
}

export function decodeJwtPart(token: string, index: number): Record<string, unknown> {
  const part = token.split(".")[index];
  if (!part) throw new Error("token part missing");
  const base64 = part.replace(/-/g, "+").replace(/_/g, "/");
  const padded = base64 + "=".repeat((4 - (base64.length % 4)) % 4);
  return JSON.parse(atob(padded)) as Record<string, unknown>;
}

export function bearer(token: string): Record<string, string> {
  return { authorization: `Bearer ${token}` };
}
