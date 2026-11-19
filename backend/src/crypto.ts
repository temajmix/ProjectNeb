/**
 * Small WebCrypto toolkit: base64 helpers, hashing, HMAC, constant-time
 * comparison and Ed25519 key loading.
 *
 * Only public material ever leaves this module. The private key is read from
 * the `LICENSE_PRIVATE_KEY` Worker secret and never serialised into a response.
 */

export interface SigningKey {
  privateKey: CryptoKey;
  publicKey: CryptoKey;
  publicJwk: JsonWebKey;
  kid: string;
}

export interface PublicKeyInfo {
  key: CryptoKey;
  jwk: JsonWebKey;
  kid: string;
}

const ED25519_PKCS8_PREFIX = hexToBytes("302e020100300506032b657004220420");
const ED25519_SPKI_PREFIX = hexToBytes("302a300506032b6570032100");

export const textEncoder = new TextEncoder();
export const textDecoder = new TextDecoder();

export function utf8(value: string): Uint8Array {
  return textEncoder.encode(value);
}

export function fromUtf8(value: Uint8Array): string {
  return textDecoder.decode(value);
}

export function bytesToHex(bytes: Uint8Array): string {
  let out = "";
  for (const byte of bytes) out += byte.toString(16).padStart(2, "0");
  return out;
}

export function hexToBytes(hex: string): Uint8Array {
  const clean = hex.trim();
  if (clean.length % 2 !== 0 || !/^[0-9a-f]*$/i.test(clean)) {
    throw new Error("invalid hex");
  }
  const out = new Uint8Array(clean.length / 2);
  for (let i = 0; i < out.length; i += 1) {
    out[i] = Number.parseInt(clean.slice(i * 2, i * 2 + 2), 16);
  }
  return out;
}

function normaliseBase64(value: string): string {
  const clean = value.replace(/\s+/g, "").replace(/-/g, "+").replace(/_/g, "/");
  const remainder = clean.length % 4;
  if (remainder === 1) throw new Error("invalid base64 length");
  return clean + "=".repeat((4 - remainder) % 4);
}

export function base64ToBytes(value: string): Uint8Array {
  const binary = atob(normaliseBase64(value));
  const out = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i += 1) out[i] = binary.charCodeAt(i);
  return out;
}

export function bytesToBase64(bytes: Uint8Array): string {
  let binary = "";
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary);
}

export function bytesToBase64Url(bytes: Uint8Array): string {
  return bytesToBase64(bytes).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/g, "");
}

export function base64UrlToBytes(value: string): Uint8Array {
  return base64ToBytes(value);
}

export function concatBytes(...parts: Uint8Array[]): Uint8Array {
  const total = parts.reduce((sum, part) => sum + part.length, 0);
  const out = new Uint8Array(total);
  let offset = 0;
  for (const part of parts) {
    out.set(part, offset);
    offset += part.length;
  }
  return out;
}

export function randomBytes(length: number): Uint8Array {
  const out = new Uint8Array(length);
  crypto.getRandomValues(out);
  return out;
}

export function randomToken(length = 32): string {
  return bytesToBase64Url(randomBytes(length));
}

/**
 * Compares two byte strings without an early exit on the first mismatch.
 * Length is public in every protocol we use it in, so the length check is fine.
 */
export function timingSafeEqual(a: Uint8Array, b: Uint8Array): boolean {
  if (a.length !== b.length) return false;
  let diff = 0;
  for (let i = 0; i < a.length; i += 1) diff |= (a[i] ?? 0) ^ (b[i] ?? 0);
  return diff === 0;
}

export async function sha256Bytes(data: string | Uint8Array): Promise<Uint8Array> {
  const bytes = typeof data === "string" ? utf8(data) : data;
  return new Uint8Array(await crypto.subtle.digest("SHA-256", bytes));
}

export async function sha256Hex(data: string | Uint8Array): Promise<string> {
  return bytesToHex(await sha256Bytes(data));
}

export async function hmacSha256(secret: string, data: string | Uint8Array): Promise<Uint8Array> {
  const key = await crypto.subtle.importKey(
    "raw",
    utf8(secret),
    { name: "HMAC", hash: "SHA-256" },
    false,
    ["sign"],
  );
  const bytes = typeof data === "string" ? utf8(data) : data;
  return new Uint8Array(await crypto.subtle.sign("HMAC", key, bytes));
}

function wrapPkcs8(seed: Uint8Array): Uint8Array {
  return concatBytes(ED25519_PKCS8_PREFIX, seed);
}

function wrapSpki(raw: Uint8Array): Uint8Array {
  return concatBytes(ED25519_SPKI_PREFIX, raw);
}

function parsePkcs8(value: string): Uint8Array {
  const bytes = base64ToBytes(value);
  // Convenience: accept a raw 32-byte seed and wrap it into PKCS#8.
  return bytes.length === 32 ? wrapPkcs8(bytes) : bytes;
}

const signingKeyCache = new Map<string, Promise<SigningKey>>();

/** Imports (and caches) the Ed25519 private key from a Worker secret. */
export function loadSigningKey(secret: string): Promise<SigningKey> {
  const cached = signingKeyCache.get(secret);
  if (cached) return cached;

  const pending = (async (): Promise<SigningKey> => {
    const trimmed = secret.trim();
    if (!trimmed) throw new Error("LICENSE_PRIVATE_KEY is empty");

    let privateKey: CryptoKey;
    if (trimmed.startsWith("{")) {
      const jwk = JSON.parse(trimmed) as JsonWebKey;
      if (jwk.kty !== "OKP" || jwk.crv !== "Ed25519" || !jwk.d) {
        throw new Error("LICENSE_PRIVATE_KEY JWK must be an Ed25519 private key");
      }
      privateKey = await crypto.subtle.importKey(
        "jwk",
        jwk,
        { name: "Ed25519" },
        true,
        ["sign"],
      );
    } else {
      privateKey = await crypto.subtle.importKey(
        "pkcs8",
        parsePkcs8(trimmed),
        { name: "Ed25519" },
        true,
        ["sign"],
      );
    }

    const privateJwk = (await crypto.subtle.exportKey("jwk", privateKey)) as JsonWebKey;
    if (!privateJwk.x) throw new Error("could not derive Ed25519 public key");
    const encodedX = privateJwk.x;

    const publicKey = await crypto.subtle.importKey(
      "raw",
      base64UrlToBytes(encodedX),
      { name: "Ed25519" },
      true,
      ["verify"],
    );
    const kid = await keyIdForRawPublicKey(base64UrlToBytes(encodedX));
    const publicJwk: JsonWebKey = {
      kty: "OKP",
      crv: "Ed25519",
      x: encodedX,
      ext: true,
      key_ops: ["verify"],
    };

    return { privateKey, publicKey, publicJwk, kid };
  })();

  signingKeyCache.set(secret, pending);
  pending.catch(() => signingKeyCache.delete(secret));
  return pending;
}

/** Accepts SPKI DER base64, a raw 32-byte public key, or a public JWK. */
export async function loadPublicKey(secret: string): Promise<PublicKeyInfo> {
  const trimmed = secret.trim();
  if (!trimmed) throw new Error("public key is empty");

  let publicKey: CryptoKey;
  let jwk: JsonWebKey;
  let raw: Uint8Array;

  if (trimmed.startsWith("{")) {
    const parsed = JSON.parse(trimmed) as JsonWebKey;
    if (parsed.kty !== "OKP" || parsed.crv !== "Ed25519" || !parsed.x) {
      throw new Error("public key JWK must be an Ed25519 public key");
    }
    publicKey = await crypto.subtle.importKey(
      "jwk",
      parsed,
      { name: "Ed25519" },
      true,
      ["verify"],
    );
    raw = base64UrlToBytes(parsed.x);
    jwk = { ...parsed, kty: "OKP", crv: "Ed25519" };
  } else {
    const bytes = base64ToBytes(trimmed);
    raw = bytes.length === 32 ? bytes : bytes.slice(ED25519_SPKI_PREFIX.length);
    publicKey = await crypto.subtle.importKey(
      "spki",
      bytes.length === 32 ? wrapSpki(bytes) : bytes,
      { name: "Ed25519" },
      true,
      ["verify"],
    );
    const exported = (await crypto.subtle.exportKey("jwk", publicKey)) as JsonWebKey;
    if (!exported.x) throw new Error("could not export Ed25519 public key");
    jwk = exported;
  }

  const kid = await keyIdForRawPublicKey(raw);
  return { key: publicKey, jwk, kid };
}

async function keyIdForRawPublicKey(raw: Uint8Array): Promise<string> {
  return bytesToBase64Url(await sha256Bytes(raw)).slice(0, 16);
}

export function publicJwkForResponse(info: PublicKeyInfo): JsonWebKey & { kid: string; use: string; alg: string } {
  return {
    kty: "OKP",
    crv: "Ed25519",
    x: info.jwk.x,
    kid: info.kid,
    use: "sig",
    alg: "EdDSA",
  };
}
