import { config, type Env } from "./env.js";
import { ApiError } from "./errors.js";
import { asString, nowMs, type DeviceRow, type SubscriptionRow, type UserRow } from "./types.js";

export interface ActiveSubscription {
  plan: string;
  until: number;
  status: string;
  subscriptionId: number;
}

/**
 * Returns the subscription with the furthest expiry for a user, or null.
 * Cancelled subscriptions keep access until their paid period ends.
 */
export async function getActiveSubscription(
  db: D1Database,
  user: UserRow,
  at = nowMs(),
): Promise<ActiveSubscription | null> {
  const row = await db
    .prepare(
      `SELECT id, plan, until, status
       FROM subscriptions
       WHERE until > ?
         AND status IN ('active', 'cancelled')
         AND (discord_id = ? OR (email IS NOT NULL AND email = ?))
       ORDER BY until DESC
       LIMIT 1`,
    )
    .bind(at, user.discord_id, user.email)
    .first<{ id: number; plan: string; until: number; status: string }>();

  if (!row) return null;
  return {
    plan: row.plan,
    until: row.until,
    status: row.status,
    subscriptionId: row.id,
  };
}

export async function getSubscriptionById(
  db: D1Database,
  id: number,
): Promise<SubscriptionRow | null> {
  return db
    .prepare(`SELECT * FROM subscriptions WHERE id = ?`)
    .bind(id)
    .first<SubscriptionRow>();
}

export async function listDevices(db: D1Database, userId: number): Promise<DeviceRow[]> {
  const result = await db
    .prepare(
      `SELECT id, user_id, device_id, device_name, created_at, last_seen_at
       FROM devices
       WHERE user_id = ?
       ORDER BY created_at ASC`,
    )
    .bind(userId)
    .all<DeviceRow>();
  return result.results ?? [];
}

export interface DeviceRegistration {
  deviceId: string;
  deviceName: string | null;
}

/**
 * Registers or refreshes a device and enforces the per-user device limit.
 *
 * Refreshing an already-known device never consumes another slot. The insert
 * is guarded by a COUNT in the same statement so concurrent refreshes cannot
 * push a user past the limit.
 */
export async function registerDevice(
  env: Env,
  userId: number,
  deviceIdInput: unknown,
  deviceNameInput: unknown,
): Promise<DeviceRow[]> {
  const deviceId = asString(deviceIdInput, "deviceId", 128);
  const deviceName = deviceNameInput === undefined || deviceNameInput === null
    ? null
    : asString(deviceNameInput, "deviceName", 100);
  const limit = config.maxDevices(env);
  const now = nowMs();

  const updated = await env.DB.prepare(
    `UPDATE devices
     SET device_name = COALESCE(?, device_name), last_seen_at = ?
     WHERE user_id = ? AND device_id = ?`,
  )
    .bind(deviceName, now, userId, deviceId)
    .run();

  if ((updated.meta.changes ?? 0) === 0) {
    const inserted = await env.DB.prepare(
      `INSERT INTO devices (user_id, device_id, device_name, created_at, last_seen_at)
       SELECT ?, ?, ?, ?, ?
       WHERE (SELECT COUNT(*) FROM devices WHERE user_id = ?) < ?`,
    )
      .bind(userId, deviceId, deviceName, now, now, userId, limit)
      .run();

    if ((inserted.meta.changes ?? 0) === 0) {
      throw new ApiError(
        403,
        "device_limit_reached",
        `Device limit reached (${limit} devices). Remove a device before adding another.`,
        { details: { limit } },
      );
    }
  }

  return listDevices(env.DB, userId);
}

/**
 * Fixed-window per-minute counter stored in D1. Throws 429 once the caller
 * exceeds `limit` requests in the current UTC minute.
 */
export async function consumeRateLimit(env: Env, subject: string, limit: number): Promise<void> {
  const bucket = Math.floor(nowMs() / 60_000);
  const results = await env.DB.batch([
    env.DB.prepare(`DELETE FROM rate_limits WHERE subject = ? AND bucket < ?`).bind(
      subject,
      bucket - 2,
    ),
    env.DB.prepare(
      `INSERT INTO rate_limits (subject, bucket, count)
       VALUES (?, ?, 1)
       ON CONFLICT(subject, bucket) DO UPDATE SET count = count + 1
       RETURNING count`,
    ).bind(subject, bucket),
  ]);

  const row = results[1]?.results?.[0] as { count?: number } | undefined;
  let count = row?.count;
  if (typeof count !== "number") {
    const fallback = await env.DB.prepare(
      `SELECT count FROM rate_limits WHERE subject = ? AND bucket = ?`,
    )
      .bind(subject, bucket)
      .first<{ count: number }>();
    count = fallback?.count ?? 1;
  }

  if (count > limit) {
    const secondsIntoMinute = Math.floor((nowMs() % 60_000) / 1000);
    throw new ApiError(
      429,
      "rate_limited",
      "Too many license refreshes. Try again in a minute.",
      {
        details: { limit, windowSeconds: 60 },
        headers: { "retry-after": String(Math.max(1, 60 - secondsIntoMinute)) },
      },
    );
  }
}
