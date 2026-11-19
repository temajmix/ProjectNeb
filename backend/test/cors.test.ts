import { describe, expect, it } from "vitest";
import { createUser, createSession, call } from "./helpers.js";

describe("CORS policy", () => {
  it.each(["https://neb.ui", "http://127.0.0.1:4173", "http://localhost:53682"])(
    "allows %s and echoes the origin",
    async (origin) => {
      const response = await call("/health", { headers: { origin } });
      expect(response.status).toBe(200);
      expect(response.headers.get("access-control-allow-origin")).toBe(origin);
      expect(response.headers.get("vary")).toContain("Origin");
    },
  );

  it("does not add CORS headers when no origin is present", async () => {
    const response = await call("/health");
    expect(response.status).toBe(200);
    expect(response.headers.get("access-control-allow-origin")).toBeNull();
  });

  it.each([
    "https://evil.example",
    "https://neb.ui.evil",
    "http://127.0.0.1.evil:4173",
    "https://127.0.0.1:4173",
    "null",
  ])("rejects browser calls from %s", async (origin) => {
    const response = await call("/health", { headers: { origin } });
    expect(response.status).toBe(403);
    expect(response.headers.get("access-control-allow-origin")).toBeNull();
    const body = (await response.json()) as { error: { code: string } };
    expect(body.error.code).toBe("origin_not_allowed");
  });

  it("answers allowed preflights without authentication", async () => {
    const response = await call("/api/license/refresh", {
      method: "OPTIONS",
      headers: {
        origin: "https://neb.ui",
        "access-control-request-method": "POST",
        "access-control-request-headers": "authorization, content-type",
      },
    });
    expect(response.status).toBe(204);
    expect(response.headers.get("access-control-allow-origin")).toBe("https://neb.ui");
    expect(response.headers.get("access-control-allow-methods")).toContain("POST");
    expect(response.headers.get("access-control-allow-headers")).toContain("authorization");
  });

  it("rejects preflights from disallowed origins", async () => {
    const response = await call("/api/me", {
      method: "OPTIONS",
      headers: {
        origin: "https://evil.example",
        "access-control-request-method": "GET",
      },
    });
    expect(response.status).toBe(403);
    expect(response.headers.get("access-control-allow-origin")).toBeNull();
  });

  it("leaves authenticated CLI-style calls unchanged", async () => {
    const user = await createUser();
    const token = await createSession(user);

    const response = await call("/api/me", {
      headers: { authorization: `Bearer ${token}` },
    });
    expect(response.status).toBe(200);
    expect(response.headers.get("access-control-allow-origin")).toBeNull();
  });
});
