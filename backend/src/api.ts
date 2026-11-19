import { config, type Env } from "./env.js";
import { ApiError, json } from "./errors.js";
import {
  authenticateRequest,
  issueLicenseToken,
} from "./tokens.js";
import {
  consumeRateLimit,
  getActiveSubscription,
  listDevices,
  registerDevice,
} from "./plans.js";
import { isoOrNull, readJson, type DeviceRow, type UserRow } from "./types.js";

function userPayload(user: UserRow): Record<string, unknown> {
  return {
    id: user.id,
    discordId: user.discord_id,
    username: user.username,
    avatar: user.avatar,
    createdAt: isoOrNull(user.created_at),
  };
}

function devicePayload(device: DeviceRow): Record<string, unknown> {
  return {
    deviceId: device.device_id,
    deviceName: device.device_name,
    createdAt: isoOrNull(device.created_at),
    lastSeenAt: isoOrNull(device.last_seen_at),
  };
}

export async function handleMe(env: Env, request: Request): Promise<Response> {
  const { user } = await authenticateRequest(env, request);
  const subscription = await getActiveSubscription(env.DB, user);
  const devices = await listDevices(env.DB, user.id);

  return json({
    user: userPayload(user),
    plan: subscription?.plan ?? "free",
    until: isoOrNull(subscription?.until ?? null),
    devices: devices.map(devicePayload),
  });
}

export async function handleLicenseRefresh(env: Env, request: Request): Promise<Response> {
  const { user } = await authenticateRequest(env, request);

  await consumeRateLimit(env, `user:${user.id}`, config.rateLimitPerMinute(env));

  const body = await readJson<{ deviceId?: unknown; deviceName?: unknown }>(request);
  const subscription = await getActiveSubscription(env.DB, user);
  if (!subscription) {
    throw new ApiError(
      403,
      "upgrade_required",
      "An active Pro subscription is required to refresh a license",
    );
  }

  const devices = await registerDevice(env, user.id, body.deviceId, body.deviceName);
  const ttl = config.licenseTtlSeconds(env);
  const license = await issueLicenseToken(
    env,
    user,
    subscription,
    devices.map((device) => device.device_id),
    ttl,
  );

  return json({
    token: license.token,
    jwt: license.token,
    plan: subscription.plan,
    until: isoOrNull(subscription.until),
  });
}

const THEME_CATALOGUE = [
  {
    id: "aurora-pro",
    name: "Aurora Pro",
    description: "Animated aurora gradients with a glassy dock.",
    version: "1.0.0",
    pro: true,
  },
  {
    id: "neon-grid",
    name: "Neon Grid",
    description: "Synthwave grid wallpaper, magenta accents and scanlines.",
    version: "1.0.0",
    pro: true,
  },
  {
    id: "obsidian",
    name: "Obsidian",
    description: "Near-black matte surfaces with minimal chrome.",
    version: "1.0.0",
    pro: true,
  },
  {
    id: "seasonal-snow",
    name: "Seasonal Snow",
    description: "Soft snowfall particles and an icy blue accent.",
    version: "1.0.0",
    pro: true,
  },
];

export async function handleStoreThemes(env: Env, request: Request): Promise<Response> {
  const { user } = await authenticateRequest(env, request);
  const subscription = await getActiveSubscription(env.DB, user);
  if (!subscription) {
    throw new ApiError(
      403,
      "upgrade_required",
      "The Pro theme store requires an active subscription",
    );
  }

  return json({
    themes: THEME_CATALOGUE,
    plan: subscription.plan,
    until: isoOrNull(subscription.until),
  });
}
