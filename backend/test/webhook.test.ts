import { describe, expect, it } from "vitest";
import { bytesToHex, hmacSha256 } from "../src/crypto.js";
import { call, countRows, env } from "./helpers.js";

const SECRET = "test-webhook-secret";
const DAY = 24 * 60 * 60 * 1000;

async function signatureFor(body: string): Promise<string> {
  return `sha256=${bytesToHex(await hmacSha256(SECRET, body))}`;
}

async function sendWebhook(
  body: Record<string, unknown>,
  options: { signature?: string | null; rawBody?: string } = {},
): Promise<Response> {
  const raw = options.rawBody ?? JSON.stringify(body);
  const signature = options.signature === undefined ? await signatureFor(raw) : options.signature;
  const headers: Record<string, string> = { "content-type": "application/json" };
  if (signature !== null) headers["x-boosty-signature"] = signature;

  return call("/webhooks/boosty", {
    method: "POST",
    headers,
    body: raw,
  });
}

function event(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    id: "evt_1",
    event: "subscriber.created",
    subscriber: {
      discord_id: "123456789012345678",
      plan: "pro",
      until: new Date(Date.now() + 30 * DAY).toISOString(),
    },
    ...overrides,
  };
}

describe("Boosty webhook", () => {
  it("creates a subscription for a valid signature", async () => {
    const response = await sendWebhook(event());
    expect(response.status).toBe(200);
    expect(await response.json()).toMatchObject({ ok: true, duplicate: false, eventId: "evt_1" });

    const row = await env.DB.prepare(`SELECT * FROM subscriptions`).first<{
      discord_id: string;
      plan: string;
      status: string;
      until: number;
      source: string;
    }>();
    expect(row).toMatchObject({
      discord_id: "123456789012345678",
      plan: "pro",
      status: "active",
      source: "boosty",
    });
    expect(row?.until).toBeGreaterThan(Date.now() + 29 * DAY);
    expect(await countRows("webhook_events")).toBe(1);
  });

  it("rejects an invalid signature without writing data", async () => {
    const body = event();
    const raw = JSON.stringify(body);
    const response = await sendWebhook(body, { rawBody: raw, signature: "sha256=" + "00".repeat(32) });
    expect(response.status).toBe(401);
    expect(((await response.json()) as { error: { code: string } }).error.code).toBe(
      "invalid_signature",
    );
    expect(await countRows("subscriptions")).toBe(0);
    expect(await countRows("webhook_events")).toBe(0);
  });

  it("rejects a missing signature", async () => {
    const response = await sendWebhook(event(), { signature: null });
    expect(response.status).toBe(401);
    expect(((await response.json()) as { error: { code: string } }).error.code).toBe(
      "missing_signature",
    );
  });

  it("is idempotent by event id", async () => {
    const first = await sendWebhook(event());
    expect(first.status).toBe(200);
    const second = await sendWebhook(event());
    expect(second.status).toBe(200);
    expect(await second.json()).toMatchObject({ ok: true, duplicate: true, eventId: "evt_1" });
    expect(await countRows("subscriptions")).toBe(1);
    expect(await countRows("webhook_events")).toBe(1);
  });

  it("extends an existing subscription expiry", async () => {
    const created = await sendWebhook({
      id: "evt_create",
      event: "subscriber.created",
      subscriber: {
        discord_id: "123456789012345678",
        plan: "pro",
        durationDays: 30,
      },
    });
    expect(created.status).toBe(200);

    const before = await env.DB.prepare(`SELECT until FROM subscriptions`).first<{ until: number }>();
    expect(before).not.toBeNull();

    const extended = await sendWebhook({
      id: "evt_renew",
      event: "subscriber.updated",
      subscriber: {
        discord_id: "123456789012345678",
        plan: "pro",
        duration_days: 15,
      },
    });
    expect(extended.status).toBe(200);

    const after = await env.DB.prepare(`SELECT until FROM subscriptions`).first<{ until: number }>();
    expect(after).not.toBeNull();
    const delta = (after?.until ?? 0) - (before?.until ?? 0);
    expect(delta).toBeGreaterThanOrEqual(15 * DAY - 5_000);
    expect(delta).toBeLessThanOrEqual(15 * DAY + 5_000);
    expect(await countRows("subscriptions")).toBe(1);
  });

  it("keeps access until the paid period ends when cancelled", async () => {
    const until = new Date(Date.now() + 10 * DAY).toISOString();
    await sendWebhook({
      id: "evt_create",
      event: "subscriber.created",
      subscriber: { discord_id: "123456789012345678", plan: "pro", until },
    });

    const cancelled = await sendWebhook({
      id: "evt_cancel",
      event: "subscriber.cancelled",
      subscriber: { discord_id: "123456789012345678" },
    });
    expect(cancelled.status).toBe(200);

    const row = await env.DB.prepare(`SELECT status, until FROM subscriptions`).first<{
      status: string;
      until: number;
    }>();
    expect(row?.status).toBe("cancelled");
    expect(row?.until).toBe(new Date(until).getTime());
  });

  it("maps an email-only purchase to a subscription", async () => {
    const response = await sendWebhook({
      id: "evt_email",
      event: "subscriber.created",
      subscriber: { email: "Buyer@Example.COM", plan: "pro", durationDays: 30 },
    });
    expect(response.status).toBe(200);

    const row = await env.DB.prepare(`SELECT discord_id, email FROM subscriptions`).first<{
      discord_id: string | null;
      email: string | null;
    }>();
    expect(row).toEqual({ discord_id: null, email: "buyer@example.com" });
  });

  it("accepts data-wrapped subscriber payloads", async () => {
    const response = await sendWebhook({
      id: "evt_wrapped",
      type: "subscriber.created",
      data: {
        subscriber: {
          discordId: "523456789012345678",
          plan: "pro",
          expiresAt: new Date(Date.now() + 10 * DAY).toISOString(),
        },
      },
    });
    expect(response.status).toBe(200);
    const row = await env.DB.prepare(`SELECT discord_id FROM subscriptions`).first<{
      discord_id: string;
    }>();
    expect(row?.discord_id).toBe("523456789012345678");
  });

  it("rejects a subscriber without an identity or expiry", async () => {
    const noIdentity = await sendWebhook({
      id: "evt_bad_1",
      event: "subscriber.created",
      subscriber: { plan: "pro", until: new Date().toISOString() },
    });
    expect(noIdentity.status).toBe(400);

    const noUntil = await sendWebhook({
      id: "evt_bad_2",
      event: "subscriber.created",
      subscriber: { discord_id: "123456789012345678", plan: "pro" },
    });
    expect(noUntil.status).toBe(400);
    expect(((await noUntil.json()) as { error: { code: string } }).error.code).toBe(
      "missing_until",
    );
  });
});
