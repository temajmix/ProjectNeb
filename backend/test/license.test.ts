import { describe, expect, it } from "vitest";
import { loadSigningKey } from "../src/crypto.js";
import { verifyJwt } from "../src/tokens.js";
import {
  addSubscription,
  apiCall,
  call,
  countRows,
  createSession,
  createUser,
  decodeJwtPart,
  env,
} from "./helpers.js";

async function refresh(token: string, deviceId: string, deviceName = "Test PC") {
  return apiCall("/api/license/refresh", token, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ deviceId, deviceName }),
  });
}

describe("license refresh and device limit", () => {
  it("issues short-lived tokens for three devices", async () => {
    const user = await createUser();
    const subscription = await addSubscription(user);
    const session = await createSession(user);
    const signing = await loadSigningKey(env.LICENSE_PRIVATE_KEY);

    const seenDevices: string[][] = [];
    for (const id of ["device-1", "device-2", "device-3"]) {
      const response = await refresh(session, id);
      expect(response.status).toBe(200);
      const body = (await response.json()) as {
        token: string;
        jwt: string;
        plan: string;
        until: string;
      };
      expect(Object.keys(body).sort()).toEqual(["jwt", "plan", "token", "until"]);
      expect(body.jwt).toBe(body.token);
      expect(body.plan).toBe("pro");
      expect(body.until).toBe(new Date(subscription.until).toISOString());

      const claims = await verifyJwt(body.token, signing.publicKey);
      expect(claims.sub).toBe(user.discord_id);
      expect(claims.plan).toBe("pro");
      expect(claims.kind).toBe("license");
      expect((claims.exp as number) - (claims.iat as number)).toBe(900);
      seenDevices.push(claims.devices as string[]);
    }

    expect(seenDevices).toEqual([["device-1"], ["device-1", "device-2"], [
      "device-1",
      "device-2",
      "device-3",
    ]]);
    expect(await countRows("devices")).toBe(3);
  });

  it("puts the JWKS kid in the JWT header", async () => {
    const user = await createUser();
    await addSubscription(user);
    const session = await createSession(user);

    const response = await refresh(session, "device-1");
    expect(response.status).toBe(200);
    const { token, jwt } = (await response.json()) as { token: string; jwt: string };

    const header = decodeJwtPart(jwt, 0) as { alg?: string; kid?: string; typ?: string };
    expect(header.alg).toBe("EdDSA");
    expect(header.typ).toBe("JWT");
    expect(typeof header.kid).toBe("string");

    const jwks = (await (await call("/api/keys/jwks")).json()) as {
      keys: Array<{ kid?: string; x?: string; d?: string }>;
    };
    const published = jwks.keys.find((key) => key.kid === header.kid);
    expect(published).toBeTruthy();
    expect(published?.d).toBeUndefined();
    expect(token).toBe(jwt);
  });

  it("rejects a fourth device with a clear error", async () => {
    const user = await createUser();
    await addSubscription(user);
    const session = await createSession(user);

    for (const id of ["device-1", "device-2", "device-3"]) {
      expect((await refresh(session, id)).status).toBe(200);
    }

    const response = await refresh(session, "device-4");
    expect(response.status).toBe(403);
    const body = (await response.json()) as {
      error: { code: string; message: string; details: { limit: number } };
    };
    expect(body.error.code).toBe("device_limit_reached");
    expect(body.error.message).toContain("Device limit");
    expect(body.error.details.limit).toBe(3);
    expect(await countRows("devices")).toBe(3);
  });

  it("refreshes a known device without consuming a slot", async () => {
    const user = await createUser();
    await addSubscription(user);
    const session = await createSession(user);

    expect((await refresh(session, "device-1", "First name")).status).toBe(200);
    expect((await refresh(session, "device-2")).status).toBe(200);
    expect((await refresh(session, "device-3")).status).toBe(200);

    const response = await refresh(session, "device-1", "Renamed PC");
    expect(response.status).toBe(200);
    const body = (await response.json()) as {
      token: string;
      jwt: string;
    };
    expect(await countRows("devices")).toBe(3);

    const me = await apiCall("/api/me", session);
    const meBody = (await me.json()) as {
      devices: Array<{ deviceId: string; deviceName: string }>;
    };
    expect(meBody.devices).toHaveLength(3);
    expect(meBody.devices.find((device) => device.deviceId === "device-1")?.deviceName).toBe(
      "Renamed PC",
    );

    const claims = decodeJwtPart(body.jwt, 1);
    expect(claims.devices).toEqual(["device-1", "device-2", "device-3"]);
  });

  it("denies refresh without an active plan", async () => {
    const user = await createUser();
    const session = await createSession(user);

    const response = await refresh(session, "device-1");
    expect(response.status).toBe(403);
    const body = (await response.json()) as { error: { code: string } };
    expect(body.error.code).toBe("upgrade_required");
    expect(await countRows("devices")).toBe(0);
  });

  it("denies refresh for an expired subscription", async () => {
    const user = await createUser();
    await addSubscription(user, { until: Date.now() - 60_000 });
    const session = await createSession(user);

    const response = await refresh(session, "device-1");
    expect(response.status).toBe(403);
    expect(await countRows("devices")).toBe(0);
  });

  it("rejects unauthenticated and malformed refresh requests", async () => {
    const anonymous = await call("/api/license/refresh", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ deviceId: "device-1" }),
    });
    expect(anonymous.status).toBe(401);

    const user = await createUser();
    await addSubscription(user);
    const session = await createSession(user);

    const missing = await apiCall("/api/license/refresh", session, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ deviceName: "No id" }),
    });
    expect(missing.status).toBe(400);
    expect(((await missing.json()) as { error: { code: string } }).error.code).toBe(
      "invalid_request",
    );
  });

  it("rate-limits refreshes per user per minute", async () => {
    const user = await createUser();
    await addSubscription(user);
    const session = await createSession(user);

    const bucket = Math.floor(Date.now() / 60_000);
    await env.DB.prepare(
      `INSERT INTO rate_limits (subject, bucket, count) VALUES (?, ?, ?)`,
    )
      .bind(`user:${user.id}`, bucket, 20)
      .run();

    const response = await refresh(session, "device-1");
    expect(response.status).toBe(429);
    expect(response.headers.get("retry-after")).toBeTruthy();
    const body = (await response.json()) as { error: { code: string } };
    expect(body.error.code).toBe("rate_limited");
  });
});

describe("account and plan gating", () => {
  it("returns user, plan, until and devices from /api/me", async () => {
    const user = await createUser("223456789012345678", "paid-neb");
    const subscription = await addSubscription(user);
    const session = await createSession(user);

    expect((await refresh(session, "device-1")).status).toBe(200);

    const response = await apiCall("/api/me", session);
    expect(response.status).toBe(200);
    const body = (await response.json()) as {
      user: { discordId: string; username: string };
      plan: string;
      until: string;
      devices: Array<{ deviceId: string }>;
    };
    expect(body.user).toMatchObject({
      discordId: "223456789012345678",
      username: "paid-neb",
    });
    expect(body.plan).toBe("pro");
    expect(body.until).toBe(new Date(subscription.until).toISOString());
    expect(body.devices).toHaveLength(1);
  });

  it("reports free accounts without an until date", async () => {
    const user = await createUser();
    const session = await createSession(user);

    const body = (await (await apiCall("/api/me", session)).json()) as {
      plan: string;
      until: string | null;
      devices: unknown[];
    };
    expect(body.plan).toBe("free");
    expect(body.until).toBeNull();
    expect(body.devices).toEqual([]);
  });

  it("gates the theme store behind an active plan", async () => {
    const freeUser = await createUser("323456789012345678", "free-neb");
    const freeSession = await createSession(freeUser);

    const anonymous = await call("/api/store/themes");
    expect(anonymous.status).toBe(401);

    const forbidden = await apiCall("/api/store/themes", freeSession);
    expect(forbidden.status).toBe(403);
    expect(((await forbidden.json()) as { error: { code: string } }).error.code).toBe(
      "upgrade_required",
    );

    const paidUser = await createUser("423456789012345678", "pro-neb");
    await addSubscription(paidUser);
    const paidSession = await createSession(paidUser);

    const allowed = await apiCall("/api/store/themes", paidSession);
    expect(allowed.status).toBe(200);
    const body = (await allowed.json()) as {
      themes: Array<{ id: string; pro: boolean }>;
      plan: string;
    };
    expect(body.plan).toBe("pro");
    expect(body.themes.length).toBeGreaterThan(0);
    expect(body.themes.every((theme) => theme.pro)).toBe(true);
  });
});
