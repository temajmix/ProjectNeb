import { describe, expect, it } from "vitest";
import { env } from "cloudflare:test";
import { loadSigningKey } from "../src/crypto.js";
import { signJwt, verifyJwt } from "../src/tokens.js";
import { nowSeconds } from "../src/types.js";
import { call, decodeJwtPart } from "./helpers.js";

async function generatedPair(): Promise<CryptoKeyPair> {
  return (await crypto.subtle.generateKey({ name: "Ed25519" }, true, [
    "sign",
    "verify",
  ])) as CryptoKeyPair;
}

describe("Ed25519 JWT", () => {
  it("signs and verifies a license-shaped token", async () => {
    const signing = await loadSigningKey(env.LICENSE_PRIVATE_KEY);
    const issuedAt = nowSeconds();
    const token = await signJwt(
      {
        sub: "123456789012345678",
        plan: "pro",
        devices: ["device-a", "device-b"],
        iat: issuedAt,
        exp: issuedAt + 900,
      },
      signing.privateKey,
      signing.kid,
    );

    const claims = await verifyJwt(token, signing.publicKey);
    expect(claims.sub).toBe("123456789012345678");
    expect(claims.plan).toBe("pro");
    expect(claims.devices).toEqual(["device-a", "device-b"]);
    expect(claims.exp).toBe(issuedAt + 900);

    const decodedHeader = decodeJwtPart(token, 0);
    expect(decodedHeader).toMatchObject({ alg: "EdDSA", typ: "JWT", kid: signing.kid });
  });

  it("rejects an expired token", async () => {
    const signing = await loadSigningKey(env.LICENSE_PRIVATE_KEY);
    const issuedAt = nowSeconds();
    const token = await signJwt(
      { sub: "123456789012345678", plan: "pro", iat: issuedAt - 1000, exp: issuedAt - 10 },
      signing.privateKey,
      signing.kid,
    );

    await expect(verifyJwt(token, signing.publicKey)).rejects.toMatchObject({
      status: 401,
      code: "token_expired",
    });
  });

  it("rejects a token signed by a different key", async () => {
    const first = await generatedPair();
    const second = await generatedPair();
    const issuedAt = nowSeconds();
    const token = await signJwt(
      { sub: "123456789012345678", plan: "pro", iat: issuedAt, exp: issuedAt + 900 },
      first.privateKey,
      "other-kid",
    );

    await expect(verifyJwt(token, second.publicKey)).rejects.toMatchObject({
      code: "invalid_signature",
    });
  });

  it("rejects a tampered payload", async () => {
    const signing = await loadSigningKey(env.LICENSE_PRIVATE_KEY);
    const issuedAt = nowSeconds();
    const token = await signJwt(
      { sub: "123456789012345678", plan: "free", iat: issuedAt, exp: issuedAt + 900 },
      signing.privateKey,
      signing.kid,
    );
    const [header, , signature] = token.split(".");
    const tamperedPayload = btoa(JSON.stringify({ sub: "123456789012345678", plan: "pro" }))
      .replace(/\+/g, "-")
      .replace(/\//g, "_")
      .replace(/=+$/, "");

    await expect(
      verifyJwt(`${header}.${tamperedPayload}.${signature}`, signing.publicKey),
    ).rejects.toMatchObject({ code: "invalid_signature" });
  });

  it("publishes a JWKS document without private material", async () => {
    const response = await call("/api/keys/jwks");
    expect(response.status).toBe(200);
    expect(response.headers.get("cache-control")).toContain("public");

    const body = (await response.json()) as {
      keys: Array<Record<string, unknown>>;
    };
    expect(body.keys).toHaveLength(1);
    expect(body.keys[0]).toMatchObject({
      kty: "OKP",
      crv: "Ed25519",
      use: "sig",
      alg: "EdDSA",
    });
    expect(typeof body.keys[0]?.x).toBe("string");
    expect(body.keys[0]?.d).toBeUndefined();
    expect(typeof body.keys[0]?.kid).toBe("string");
  });
});
