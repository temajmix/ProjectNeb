import { describe, expect, it } from "vitest";
import { handleAuthCallback, type FetchLike } from "../src/auth.js";
import { sha256Hex } from "../src/crypto.js";
import { loadSigningKey } from "../src/crypto.js";
import { buildSessionRedirect } from "../src/return-url.js";
import { verifyJwt } from "../src/tokens.js";
import { call, countRows, env, testEnv, apiCall } from "./helpers.js";

const DISCORD_ID = "123456789012345678";
const LOOPBACK_RETURN = "http://127.0.0.1:53682/callback";

async function insertState(
  state: string,
  options: { returnUrl?: string; expiresInMs?: number; used?: boolean } = {},
): Promise<void> {
  const now = Date.now();
  await env.DB.prepare(
    `INSERT INTO oauth_states (state_hash, return_url, created_at, expires_at, used_at)
     VALUES (?, ?, ?, ?, ?)`,
  )
    .bind(
      await sha256Hex(state),
      options.returnUrl ?? LOOPBACK_RETURN,
      now,
      now + (options.expiresInMs ?? 600_000),
      options.used ? now : null,
    )
    .run();
}

function callbackPath(state: string, extra = ""): string {
  return `/auth/discord/callback?code=code-123&state=${encodeURIComponent(state)}${extra}`;
}

function callbackUrl(state: string, extra = ""): URL {
  return new URL(`https://api.test${callbackPath(state, extra)}`);
}

function discordMock(overrides: { failToken?: boolean } = {}): FetchLike {
  return async (input, init) => {
    const url = String(input);

    if (url.endsWith("/api/oauth2/token")) {
      if (init?.method !== "POST") throw new Error("token exchange must be POST");
      const body = new URLSearchParams(String(init?.body ?? ""));
      if (body.get("code") !== "code-123") throw new Error("wrong code supplied");
      if (body.get("grant_type") !== "authorization_code") {
        throw new Error("wrong grant type");
      }
      if (body.get("client_secret") !== "test-discord-client-secret") {
        throw new Error("wrong client secret");
      }
      if (overrides.failToken) {
        return new Response(JSON.stringify({ error: "invalid_grant" }), {
          status: 400,
          headers: { "content-type": "application/json" },
        });
      }
      return new Response(
        JSON.stringify({ access_token: "discord-access-token", token_type: "Bearer" }),
        { status: 200, headers: { "content-type": "application/json" } },
      );
    }

    if (url.endsWith("/api/users/@me")) {
      const headers = new Headers(init?.headers);
      if (headers.get("authorization") !== "Bearer discord-access-token") {
        throw new Error("missing Discord bearer token");
      }
      return new Response(
        JSON.stringify({
          id: DISCORD_ID,
          username: "neb-tester",
          avatar: "avatar-hash",
        }),
        { status: 200, headers: { "content-type": "application/json" } },
      );
    }

    throw new Error(`unexpected Discord request: ${url}`);
  };
}

function stateFrom(response: Response): string {
  const location = response.headers.get("location");
  if (!location) throw new Error("missing location");
  const state = new URL(location).searchParams.get("state");
  if (!state) throw new Error("missing state");
  return state;
}

describe("loopback return URL policy", () => {
  it.each([
    ["https://evil.example/callback", "remote HTTPS host"],
    ["http://127.0.0.1.evil/callback", "host that merely starts with 127.0.0.1"],
    ["http://localhost.evil:8080/callback", "host that merely starts with localhost"],
    ["file:///tmp/callback", "file scheme"],
    ["projectneb://auth/callback", "custom scheme"],
    ["http://127.0.0.1/callback", "missing explicit port"],
    ["http://localhost/callback", "missing explicit port"],
    ["https://127.0.0.1:8080/callback", "loopback over HTTPS is not part of the policy"],
    ["http://127.0.0.1:0/callback", "port below range"],
    ["http://127.0.0.1:65536/callback", "port above range"],
    ["http://127.0.0.1:8080/not-callback", "wrong path"],
    ["http://127.0.0.1:8080/callback?next=https://evil.example", "query string"],
    ["http://127.0.0.1:8080/callback#frag", "fragment"],
    ["http://user:pass@127.0.0.1:8080/callback", "credentials"],
    ["http://[::1]:8080/callback", "IPv6 loopback is not allowlisted"],
    ["http://127.0.0.1:8080/callback/", "trailing slash"],
    ["//127.0.0.1:8080/callback", "scheme-relative URL"],
  ])("rejects %s (%s)", async (value) => {
    const response = await call(`/auth/discord/start?return=${encodeURIComponent(value)}`);
    expect(response.status).toBe(400);
    const body = (await response.json()) as { error: { code: string } };
    expect(body.error.code).toBe("invalid_return_url");
  });

  it("accepts loopback hosts and keeps an explicit port", async () => {
    for (const value of [
      "http://127.0.0.1:53682/callback",
      "http://localhost:3000/callback",
      "HTTP://LOCALHOST:4040/callback",
    ]) {
      const response = await call(`/auth/discord/start?return=${encodeURIComponent(value)}`);
      expect(response.status).toBe(302);
      const row = await env.DB.prepare(
        `SELECT return_url FROM oauth_states WHERE state_hash = ?`,
      )
        .bind(await sha256Hex(stateFrom(response)))
        .first<{ return_url: string }>();
      expect(row?.return_url).toMatch(/^http:\/\/(127\.0\.0\.1|localhost):\d+\/callback$/);
    }
  });

  it("requires the return parameter", async () => {
    const response = await call("/auth/discord/start");
    expect(response.status).toBe(400);
    expect(((await response.json()) as { error: { code: string } }).error.code).toBe(
      "missing_return",
    );
  });
});

describe("OAuth state", () => {
  it("stores a single-use state bound to the return URL for 10 minutes", async () => {
    const response = await call(
      `/auth/discord/start?return=${encodeURIComponent(LOOPBACK_RETURN)}`,
    );
    expect(response.status).toBe(302);

    const state = stateFrom(response);
    const row = await env.DB.prepare(
      `SELECT return_url, created_at, expires_at, used_at FROM oauth_states WHERE state_hash = ?`,
    )
      .bind(await sha256Hex(state))
      .first<{
        return_url: string;
        created_at: number;
        expires_at: number;
        used_at: number | null;
      }>();

    expect(row?.return_url).toBe(LOOPBACK_RETURN);
    expect(row?.used_at).toBeNull();
    expect((row?.expires_at ?? 0) - (row?.created_at ?? 0)).toBe(600_000);
  });

  it("ignores a return parameter supplied to the callback", async () => {
    const stored = "http://127.0.0.1:41000/callback";
    const attacker = "http://127.0.0.1:41001/callback";
    await insertState("bound-state", { returnUrl: stored });

    const response = await handleAuthCallback(
      testEnv,
      callbackUrl("bound-state", `&return=${encodeURIComponent(attacker)}`),
      { fetch: discordMock() },
    );
    expect(response.status).toBe(302);
    const redirect = new URL(response.headers.get("location") as string);
    expect(`${redirect.protocol}//${redirect.host}${redirect.pathname}`).toBe(stored);
  });

  it("binds each state to its own return URL", async () => {
    const first = await call(
      `/auth/discord/start?return=${encodeURIComponent("http://127.0.0.1:5001/callback")}`,
    );
    const second = await call(
      `/auth/discord/start?return=${encodeURIComponent("http://localhost:5002/callback")}`,
    );
    const firstState = stateFrom(first);
    const secondState = stateFrom(second);
    expect(firstState).not.toBe(secondState);

    const firstRedirect = await handleAuthCallback(
      testEnv,
      callbackUrl(firstState),
      { fetch: discordMock() },
    );
    expect(new URL(firstRedirect.headers.get("location") as string).origin).toBe(
      "http://127.0.0.1:5001",
    );
  });

  it("rejects an unknown state", async () => {
    const response = await call(callbackPath("never-issued"));
    expect(response.status).toBe(400);
    expect(((await response.json()) as { error: { code: string } }).error.code).toBe(
      "invalid_state",
    );
  });

  it("rejects and removes an expired state", async () => {
    await insertState("expired-state", { expiresInMs: -60_000 });

    const response = await call(callbackPath("expired-state"));
    expect(response.status).toBe(400);
    expect(((await response.json()) as { error: { code: string } }).error.code).toBe(
      "expired_state",
    );
    expect(await countRows("oauth_states")).toBe(0);
  });

  it("does not allow a state to be replayed", async () => {
    await insertState("replay-state");

    const first = await handleAuthCallback(testEnv, callbackUrl("replay-state"), {
      fetch: discordMock(),
    });
    expect(first.status).toBe(302);

    let fetchCalled = false;
    await expect(
      handleAuthCallback(testEnv, callbackUrl("replay-state"), {
        fetch: async () => {
          fetchCalled = true;
          throw new Error("must not call Discord");
        },
      }),
    ).rejects.toMatchObject({ status: 400, code: "state_already_used" });
    expect(fetchCalled).toBe(false);
  });
});

describe("Discord OAuth callback", () => {
  it("exchanges the code, upserts the user and returns the session in query and fragment", async () => {
    await insertState("good-state");

    const response = await handleAuthCallback(
      testEnv,
      callbackUrl("good-state"),
      { fetch: discordMock() },
    );
    expect(response.status).toBe(302);
    expect(response.headers.get("cache-control")).toBe("no-store");
    expect(response.headers.get("referrer-policy")).toBe("no-referrer");

    const location = response.headers.get("location");
    expect(location).toBeTruthy();
    const redirect = new URL(location as string);
    expect(redirect.protocol).toBe("http:");
    expect(redirect.hostname).toBe("127.0.0.1");
    expect(redirect.port).toBe("53682");
    expect(redirect.pathname).toBe("/callback");

    // Loopback listeners can read either form; both must carry the same token.
    const queryToken = redirect.searchParams.get("session");
    const fragmentToken = new URLSearchParams(redirect.hash.replace(/^#/, "")).get("session");
    expect(queryToken).toBeTruthy();
    expect(fragmentToken).toBe(queryToken);

    const token = queryToken as string;
    const me = await apiCall("/api/me", token);
    expect(me.status).toBe(200);
    expect(await me.json()).toMatchObject({
      user: { discordId: DISCORD_ID, username: "neb-tester" },
      plan: "free",
      until: null,
      devices: [],
    });

    const signing = await loadSigningKey(env.LICENSE_PRIVATE_KEY);
    const claims = await verifyJwt(token, signing.publicKey);
    expect(claims).toMatchObject({
      sub: DISCORD_ID,
      kind: "session",
      iss: "projectneb",
    });
    expect((claims.exp as number) - (claims.iat as number)).toBe(2_592_000);

    const user = await env.DB.prepare(`SELECT username, avatar FROM users WHERE discord_id = ?`)
      .bind(DISCORD_ID)
      .first<{ username: string; avatar: string | null }>();
    expect(user).toEqual({ username: "neb-tester", avatar: "avatar-hash" });

    const session = await env.DB.prepare(`SELECT token_hash, revoked_at FROM sessions`).first<{
      token_hash: string;
      revoked_at: number | null;
    }>();
    expect(session?.token_hash).toBe(await sha256Hex(token));
    expect(session?.revoked_at).toBeNull();

    const state = await env.DB.prepare(
      `SELECT used_at FROM oauth_states WHERE state_hash = ?`,
    )
      .bind(await sha256Hex("good-state"))
      .first<{ used_at: number | null }>();
    expect(state?.used_at).not.toBeNull();
  });

  it("reports a Discord exchange failure without issuing a session", async () => {
    await insertState("bad-exchange");

    await expect(
      handleAuthCallback(testEnv, callbackUrl("bad-exchange"), {
        fetch: discordMock({ failToken: true }),
      }),
    ).rejects.toMatchObject({ status: 502, code: "discord_exchange_failed" });
    expect(await countRows("users")).toBe(0);
    expect(await countRows("sessions")).toBe(0);
  });

  it("adds the session to the query only for loopback returns", () => {
    const token = "header.payload.signature";
    expect(buildSessionRedirect("http://127.0.0.1:5000/callback", token)).toBe(
      "http://127.0.0.1:5000/callback?session=header.payload.signature#session=header.payload.signature",
    );
    expect(buildSessionRedirect("http://localhost:5000/callback", token)).toContain(
      "?session=",
    );
    // Defensive helper behaviour; the API itself never accepts a remote return.
    expect(buildSessionRedirect("https://neb.ui/callback", token)).toBe(
      "https://neb.ui/callback#session=header.payload.signature",
    );
  });
});
