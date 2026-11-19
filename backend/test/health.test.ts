import { describe, expect, it } from "vitest";
import { call } from "./helpers.js";

describe("health", () => {
  it("reports ok and the configured version", async () => {
    const response = await call("/health");
    expect(response.status).toBe(200);
    expect(await response.json()).toEqual({ ok: true, version: "test" });
  });

  it("returns 405 for the wrong method", async () => {
    const response = await call("/health", { method: "POST" });
    expect(response.status).toBe(405);
    expect(response.headers.get("allow")).toBe("GET");
  });

  it("returns 404 for unknown routes", async () => {
    const response = await call("/nope");
    expect(response.status).toBe(404);
    const body = (await response.json()) as { error: { code: string } };
    expect(body.error.code).toBe("not_found");
  });
});
