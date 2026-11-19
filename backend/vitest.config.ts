import { generateKeyPairSync } from "node:crypto";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { cloudflareTest, readD1Migrations } from "@cloudflare/vitest-plugin";
import { defineConfig } from "vitest/config";

const migrationsDirectory = path.join(
  path.dirname(fileURLToPath(import.meta.url)),
  "migrations",
);

// A fresh test-only Ed25519 key for every run. No signing key is committed.
const testKeyPair = generateKeyPairSync("ed25519");
const TEST_PRIVATE_KEY = testKeyPair.privateKey
  .export({ format: "der", type: "pkcs8" })
  .toString("base64");

export default defineConfig(async () => {
  const migrations = await readD1Migrations(migrationsDirectory);

  return {
    plugins: [
      cloudflareTest({
        wrangler: { configPath: "./wrangler.toml" },
        miniflare: {
          bindings: {
            TEST_MIGRATIONS: migrations,
            LICENSE_PRIVATE_KEY: TEST_PRIVATE_KEY,
            LICENSE_PREVIOUS_PUBLIC_KEY: "",
            DISCORD_CLIENT_ID: "123456789012345678",
            DISCORD_CLIENT_SECRET: "test-discord-client-secret",
            DISCORD_REDIRECT_URI: "http://127.0.0.1:8787/auth/discord/callback",
            BOOSTY_WEBHOOK_SECRET: "test-webhook-secret",
            MAX_DEVICES: "3",
            RATE_LIMIT_PER_MINUTE: "20",
            LICENSE_TTL_SECONDS: "900",
            SESSION_TTL_SECONDS: "2592000",
            OAUTH_STATE_TTL_SECONDS: "600",
            VERSION: "test",
          },
        },
      }),
    ],
    test: {
      setupFiles: ["./test/setup.ts"],
    },
  };
});
