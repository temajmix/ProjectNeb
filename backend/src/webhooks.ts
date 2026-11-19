import type { Env } from "./env.js";
import { base64ToBytes, hexToBytes, hmacSha256, textDecoder, timingSafeEqual } from "./crypto.js";
import { ApiError, json } from "./errors.js";
import { nowMs, type SubscriptionRow } from "./types.js";

const MAX_BODY_BYTES = 64 * 1024;
const DAY_MS = 24 * 60 * 60 * 1000;

function headerValue(request: Request, names: string[]): string | null {
  for (const name of names) {
    const value = request.headers.get(name);
    if (value) return value;
  }
  return null;
}

function parseSignature(header: string): Uint8Array | null {
  let value = header.trim();
  if (value.toLowerCase().startsWith("sha256=")) value = value.slice("sha256=".length).trim();

  if (/^[0-9a-f]{64}$/i.test(value)) {
    return hexToBytes(value);
  }
  try {
    const bytes = base64ToBytes(value);
    return bytes.length === 32 ? bytes : null;
  } catch {
    return null;
  }
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function stringField(record: Record<string, unknown>, names: string[], maxLength = 256): string | null {
  for (const name of names) {
    const value = record[name];
    if (typeof value === "string" && value.trim()) return value.trim().slice(0, maxLength);
  }
  return null;
}

function parseUntil(value: unknown): number | null {
  if (typeof value === "number" && Number.isFinite(value)) {
    if (value > 100_000_000_000) return Math.floor(value); // milliseconds
    if (value > 1_000_000_000) return Math.floor(value * 1000); // seconds
    return null;
  }
  if (typeof value === "string" && value.trim()) {
    const asNumber = Number(value);
    if (Number.isFinite(asNumber)) return parseUntil(asNumber);
    const parsed = Date.parse(value);
    if (Number.isFinite(parsed)) return parsed;
  }
  return null;
}

function parseDurationDays(value: unknown): number | null {
  const parsed = typeof value === "number" ? value : typeof value === "string" ? Number(value) : NaN;
  if (!Number.isFinite(parsed) || parsed <= 0 || parsed > 3650) return null;
  return parsed;
}

async function findExisting(
  db: D1Database,
  discordId: string | null,
  email: string | null,
): Promise<SubscriptionRow | null> {
  return db
    .prepare(
      `SELECT * FROM subscriptions
       WHERE source = 'boosty'
         AND ((? IS NOT NULL AND discord_id = ?) OR (? IS NOT NULL AND email = ?))
       ORDER BY until DESC
       LIMIT 1`,
    )
    .bind(discordId, discordId, email, email)
    .first<SubscriptionRow>();
}

export async function handleBoostyWebhook(env: Env, request: Request): Promise<Response> {
  const secret = env.BOOSTY_WEBHOOK_SECRET?.trim();
  if (!secret) {
    throw new ApiError(500, "server_misconfigured", "BOOSTY_WEBHOOK_SECRET is not configured");
  }

  const rawBody = await request.arrayBuffer();
  if (rawBody.byteLength > MAX_BODY_BYTES) {
    throw new ApiError(413, "body_too_large", "Webhook body is too large");
  }
  const bodyBytes = new Uint8Array(rawBody);

  const signatureHeader = headerValue(request, ["x-boosty-signature", "x-signature"]);
  if (!signatureHeader) {
    throw new ApiError(401, "missing_signature", "Webhook signature header is missing");
  }

  const expected = await hmacSha256(secret, bodyBytes);
  const provided = parseSignature(signatureHeader);
  if (!provided || !timingSafeEqual(expected, provided)) {
    throw new ApiError(401, "invalid_signature", "Webhook signature is not valid");
  }

  const rawBodyText = textDecoder.decode(bodyBytes);
  let body: unknown;
  try {
    body = JSON.parse(rawBodyText);
  } catch {
    throw new ApiError(400, "invalid_json", "Webhook body must be valid JSON");
  }
  if (!isRecord(body)) {
    throw new ApiError(400, "invalid_request", "Webhook body must be a JSON object");
  }

  const eventId =
    stringField(body, ["id", "event_id"], 128) ??
    headerValue(request, ["x-boosty-event-id", "x-event-id"]);
  if (!eventId) {
    throw new ApiError(400, "missing_event_id", "Webhook event id is required");
  }

  const eventType = stringField(body, ["event", "type", "event_type"], 64) ?? "subscriber.updated";

  const nestedSubscriber = isRecord(body.subscriber)
    ? body.subscriber
    : isRecord(body.data) && isRecord(body.data.subscriber)
      ? body.data.subscriber
      : isRecord(body.data)
        ? body.data
        : null;
  const subscriber = nestedSubscriber ?? body;

  const rawDiscordId =
    stringField(subscriber, ["discord_id", "discordId"], 64) ??
    (typeof subscriber.discord_id === "number" &&
    Number.isSafeInteger(subscriber.discord_id) &&
    subscriber.discord_id > 0
      ? String(subscriber.discord_id)
      : null);
  const discordId = rawDiscordId;
  const emailRaw = stringField(subscriber, ["email"], 254);
  const email = emailRaw ? emailRaw.toLowerCase() : null;
  if (!discordId && !email) {
    throw new ApiError(
      400,
      "missing_subscriber",
      "Webhook must include subscriber.discord_id or subscriber.email",
    );
  }

  const plan = stringField(subscriber, ["plan"], 32) ?? stringField(body, ["plan"], 32) ?? "pro";
  const absoluteUntil = parseUntil(
    subscriber.until ??
      subscriber.expires_at ??
      subscriber.expiresAt ??
      body.until ??
      body.expires_at,
  );
  const durationDays = parseDurationDays(
    subscriber.durationDays ??
      subscriber.duration_days ??
      body.durationDays ??
      body.duration_days,
  );

  const externalRef = stringField(subscriber, ["subscriber_id", "subscriberId"], 128);
  const explicitStatus =
    stringField(subscriber, ["status"], 32) ?? stringField(body, ["status"], 32);
  const cancelled = explicitStatus
    ? /cancel|inactive|expire|refund|delete/i.test(explicitStatus)
    : /cancel|unsubscribe|delete|expire|refund/i.test(eventType);
  const status = cancelled ? "cancelled" : "active";

  const inserted = await env.DB.prepare(
    `INSERT INTO webhook_events (event_id, event_type, payload, received_at)
     VALUES (?, ?, ?, ?)
     ON CONFLICT(event_id) DO NOTHING`,
  )
    .bind(eventId, eventType, rawBodyText, nowMs())
    .run();

  if ((inserted.meta.changes ?? 0) === 0) {
    return json({ ok: true, duplicate: true, eventId });
  }

  try {
    const now = nowMs();
    const existing = await findExisting(env.DB, discordId, email);

    let until: number;
    if (cancelled) {
      // Cancellation never extends access; keep the already-paid period unless
      // the provider sends an explicit end date.
      until = absoluteUntil ?? existing?.until ?? now;
    } else if (durationDays !== null) {
      until = Math.max(now, existing?.until ?? 0) + Math.round(durationDays * DAY_MS);
    } else if (absoluteUntil !== null) {
      until = absoluteUntil;
    } else if (existing) {
      until = existing.until;
    } else {
      throw new ApiError(
        400,
        "missing_until",
        "Subscriber event must include until or durationDays",
      );
    }

    let subscriptionId: number;
    if (existing) {
      await env.DB.prepare(
        `UPDATE subscriptions SET
           discord_id = COALESCE(?, discord_id),
           email = COALESCE(?, email),
           plan = ?,
           status = ?,
           until = ?,
           external_ref = COALESCE(?, external_ref),
           updated_at = ?
         WHERE id = ?`,
      )
        .bind(discordId, email, plan, status, until, externalRef, now, existing.id)
        .run();
      subscriptionId = existing.id;
    } else {
      const result = await env.DB.prepare(
        `INSERT INTO subscriptions
           (discord_id, email, plan, status, until, source, external_ref, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, 'boosty', ?, ?, ?)`,
      )
        .bind(discordId, email, plan, status, until, externalRef, now, now)
        .run();
      subscriptionId = Number(result.meta.last_row_id);
    }

    await env.DB.prepare(
      `UPDATE webhook_events SET processed_at = ? WHERE event_id = ?`,
    )
      .bind(nowMs(), eventId)
      .run();

    return json({
      ok: true,
      duplicate: false,
      eventId,
      subscription: {
        id: subscriptionId,
        plan,
        status,
        until: new Date(until).toISOString(),
      },
    });
  } catch (error) {
    // Allow a retry when processing failed transiently. Permanent input errors
    // stay recorded so a broken event cannot be replayed forever.
    if (!(error instanceof ApiError) || error.status >= 500) {
      await env.DB.prepare(`DELETE FROM webhook_events WHERE event_id = ?`).bind(eventId).run();
    }
    throw error;
  }
}

export const __test = { parseSignature, parseUntil, parseDurationDays };
