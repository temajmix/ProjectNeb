import { ApiError } from "./errors.js";

export interface UserRow {
  id: number;
  discord_id: string;
  username: string;
  avatar: string | null;
  email: string | null;
  created_at: number;
  updated_at: number;
}

export interface SessionRow {
  id: string;
  user_id: number;
  token_hash: string;
  created_at: number;
  expires_at: number;
  revoked_at: number | null;
}

export interface SubscriptionRow {
  id: number;
  discord_id: string | null;
  email: string | null;
  plan: string;
  status: string;
  until: number;
  source: string;
  external_ref: string | null;
  created_at: number;
  updated_at: number;
}

export interface DeviceRow {
  id: number;
  user_id: number;
  device_id: string;
  device_name: string | null;
  created_at: number;
  last_seen_at: number;
}

export async function readJson<T>(request: Request, maxBytes = 16 * 1024): Promise<T> {
  const text = await request.text();
  if (text.length > maxBytes) {
    throw new ApiError(413, "body_too_large", "Request body is too large");
  }
  if (!text) {
    throw new ApiError(400, "invalid_json", "Request body must be JSON");
  }
  try {
    return JSON.parse(text) as T;
  } catch {
    throw new ApiError(400, "invalid_json", "Request body must be valid JSON");
  }
}

export function asString(value: unknown, field: string, maxLength = 256): string {
  if (typeof value !== "string" || !value.trim()) {
    throw new ApiError(400, "invalid_request", `${field} must be a non-empty string`);
  }
  const trimmed = value.trim();
  if (trimmed.length > maxLength) {
    throw new ApiError(400, "invalid_request", `${field} is too long`);
  }
  return trimmed;
}

export function asOptionalString(value: unknown, field: string, maxLength = 256): string | null {
  if (value === undefined || value === null) return null;
  return asString(value, field, maxLength);
}

export function nowMs(): number {
  return Date.now();
}

export function nowSeconds(): number {
  return Math.floor(Date.now() / 1000);
}

export function isoOrNull(ms: number | null | undefined): string | null {
  return typeof ms === "number" ? new Date(ms).toISOString() : null;
}
