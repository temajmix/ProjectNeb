# ProjectNeb Pro backend

Cloudflare Worker + D1 service that issues short-lived, server-signed Pro
entitlements for the ProjectNeb desktop client.

**Status: deployable, not deployed.** `wrangler.toml` contains a placeholder D1
id and a placeholder Discord client id.

- Runtime: Cloudflare Workers, TypeScript, plain `fetch` routing (no framework)
- Storage: D1 (SQLite). OAuth state, rate limits and idempotency live here too,
  so no KV namespace is required.
- Signing: Ed25519 via WebCrypto. The private key is a Worker secret; the public
  key is derived at runtime and served as JWKS.
- Tests: `vitest` running inside `workerd` through `@cloudflare/vitest-plugin`
  with real D1 migrations.

The client never decides whether a user is Pro. The server checks the
subscription, enforces the device limit and signs a 15-minute license. The
client only verifies that signature offline and reads the claims.

---

## Endpoints

| Method | Path | Auth | Purpose |
| --- | --- | --- | --- |
| `GET` | `/health` | none | `{ "ok": true, "version": "0.1.0" }` |
| `GET` | `/auth/discord/start?return=<loopback>` | none | Validates the loopback return URL, creates a one-time OAuth state (10 min) and redirects to Discord (`identify` scope). |
| `GET` | `/auth/discord/callback?code&state` | OAuth state | Exchanges the code server-side, upserts the user, creates a 30-day session and redirects to the stored loopback URL with `?session=<jwt>#session=<jwt>`. |
| `GET` | `/api/me` | `Bearer` session | `{ user, plan, until, devices }` for the signed-in user. |
| `POST` | `/api/license/refresh` | `Bearer` session | Body `{ "deviceId": "...", "deviceName": "..." }`; returns `{ token, jwt, plan, until }` with a 15-minute EdDSA license. Enforces the device cap. |
| `GET` | `/api/keys/jwks` | none | Ed25519 public key(s) as JWKS. Current key is first; a rotation key may follow. |
| `POST` | `/webhooks/boosty` | HMAC header | Verifies `X-Boosty-Signature`, maps subscriber events to subscriptions, idempotent by event id. |
| `GET` | `/api/store/themes` | `Bearer` session + active plan | Placeholder Pro theme catalogue. `401` without a session, `403` on the free plan. |

Errors are JSON:

```json
{ "error": { "code": "device_limit_reached", "message": "Device limit reached (3 devices). Remove a device before adding another.", "details": { "limit": 3 } } }
```

Common codes: `unauthorized`, `invalid_token`, `token_expired`,
`session_expired`, `upgrade_required`, `device_limit_reached`, `rate_limited`,
`invalid_state`, `expired_state`, `invalid_signature`, `missing_signature`,
`origin_not_allowed`.

### CORS

Browser contexts may call the API only from:

- `https://neb.ui` (the desktop WebView2 virtual host), and
- loopback origins: `http://127.0.0.1:<port>` / `http://localhost:<port>`.

Allowed origins receive `Access-Control-Allow-Origin` (echoed, with
`Vary: Origin`) and preflights are answered without authentication. Any other
`Origin` gets `403 origin_not_allowed`. Requests without an `Origin` header
(CLI, worker-to-worker, native desktop HTTP) are not CORS requests and are
unaffected. Credentials mode is not used: the API authenticates with
`Authorization: Bearer`, not cookies.

### `return` URL policy (desktop loopback)

The desktop client opens a local listener and calls
`/auth/discord/start?return=http://127.0.0.1:<port>/callback` (or
`http://localhost:<port>/callback`). Only those two shapes are accepted. The
check is a raw-string match, so URL-parser normalisation cannot widen it.

| Input | Result |
| --- | --- |
| `http://127.0.0.1:53682/callback` | accepted |
| `http://localhost:3000/callback` | accepted |
| `https://evil.example/callback` | rejected (remote host) |
| `http://127.0.0.1.evil/callback` | rejected (host suffix trick) |
| `http://localhost.evil:8080/callback` | rejected (host suffix trick) |
| `http://127.0.0.1/callback` | rejected (explicit port required) |
| `https://127.0.0.1:8080/callback` | rejected (HTTP loopback only) |
| `file:///tmp/callback` | rejected (scheme) |
| `projectneb://auth/callback` | rejected (custom scheme) |
| `http://127.0.0.1:8080/callback?x=1` | rejected (query/fragment/path must be exact) |
| `http://user:pass@127.0.0.1:8080/callback` | rejected (credentials) |
| `http://[::1]:8080/callback` | rejected (not allowlisted) |

Rejections are `400 { "error": { "code": "invalid_return_url" } }`. There is no
configurable scheme allowlist and no open redirect: the stored URL is
re-validated at callback time, so a tampered D1 row cannot redirect off-policy.

### Session delivery

The callback redirects to the stored loopback URL with the session in **both**
places:

```
http://127.0.0.1:53682/callback?session=<jwt>#session=<jwt>
```

- The **fragment** is never sent to a server, so it does not appear in access
  logs or `Referer` headers.
- The **query** copy exists only because the return host is loopback: a local
  listener can read it without parsing the fragment. Since the API refuses
  every non-loopback return, this copy can never be attached to a remote
  origin. The redirect also sets `Referrer-Policy: no-referrer` and
  `Cache-Control: no-store`.
- The desktop callback page should read the token, immediately replace the URL
  (for example `history.replaceState({}, "", "/callback")`) and avoid loading
  remote resources before doing so.

### State handling

- 32 random bytes, stored only as a SHA-256 hash.
- Single-use: the callback atomically claims the row with
  `UPDATE ... WHERE used_at IS NULL`; replays get `400 state_already_used`.
- Bound to the return URL: the URL is stored at `/start` and the callback
  ignores any `return` parameter supplied later.
- Expires 10 minutes after creation (`OAUTH_STATE_TTL_SECONDS`, capped at 600);
  expired rows are deleted on use and opportunistically during `/start`.

---

## Entitlement token

`POST /api/license/refresh` returns:

```json
{
  "token": "<ed25519 jwt>",
  "jwt": "<the same jwt>",
  "plan": "pro",
  "until": "2027-01-01T00:00:00.000Z"
}
```

`token` and `jwt` are aliases kept for the desktop client; both hold the same
signed string. `until` is ISO-8601 (Unix milliseconds inside the JWT). Extra
device data is available from `/api/me`.

The JWT header contains the `kid` of the signing key:

```json
{ "alg": "EdDSA", "typ": "JWT", "kid": "GJ1m2Q0r4n0m4Xbq" }
```

`GET /api/keys/jwks` publishes the matching key (`kid`, `kty: "OKP"`,
`crv: "Ed25519"`, `use: "sig"`, `alg: "EdDSA"`, `x`). Select the JWKS entry whose
`kid` equals the token header `kid`; never trust a key shipped next to the
token.

License JWT (EdDSA / Ed25519):

```json
{
  "iss": "projectneb",
  "aud": "projectneb-client",
  "sub": "123456789012345678",
  "plan": "pro",
  "until": 1798761600,
  "devices": ["a1b2c3", "d4e5f6"],
  "kind": "license",
  "iat": 1796169600,
  "exp": 1796170500
}
```

- `sub` is the Discord id.
- `plan` is `pro` while the subscription is active.
- `until` is the paid-through date in Unix seconds.
- `devices` are the currently registered device ids (up to `MAX_DEVICES`).
- `exp` is 15 minutes after `iat` (`LICENSE_TTL_SECONDS`, default 900).
- Header: `{ "alg": "EdDSA", "typ": "JWT", "kid": "<key id>" }`.

Session JWT (returned by OAuth, used only against this API):

```json
{ "iss": "projectneb", "aud": "projectneb-client", "sub": "123456789012345678", "sid": "...", "kind": "session", "iat": 0, "exp": 0 }
```

Sessions are 30 days. The token hash and `sid` are stored in D1, so a session
can be revoked server-side before its JWT expiry. The session token is not an
entitlement and carries no `plan`; clients must not treat it as one.

### How the client verifies a license

1. Obtain the Ed25519 public key. Either embed the raw `x` from `/api/keys/jwks`
   at build time (recommended) or fetch and cache JWKS. Pin the key; do not
   accept a key supplied next to the token.
2. Split the token into three base64url segments. Decode the header and require
   `alg == "EdDSA"`. Verify the Ed25519 signature over
   `base64url(header) + "." + base64url(payload)`.
3. Decode the payload and require:
   - `kind == "license"`, `iss == "projectneb"`, `aud == "projectneb-client"`
   - a known `sub` for the logged-in account
   - `plan == "pro"`
   - `devices` contains this installation's device id
   - `exp > now` (allow at most a few minutes of clock skew)
   - optionally `until > now`
4. Derive Pro features only from these verified claims. A locally editable
   `plan: true` flag must never unlock anything.
5. Refresh in the background before `exp` (for example every 5–7 minutes) using
   the session token. Network failure is not a licence failure: keep using the
   last verified token until `exp`.
6. Store the session token in the OS credential store (DPAPI/Credential
   Manager), not in a plain settings file.

The device id should be stable per installation and sent on every refresh. The
server rejects a new device once the account already has `MAX_DEVICES`.

---

## Data model

`migrations/0001_init.sql` creates:

| Table | Purpose | Key columns |
| --- | --- | --- |
| `users` | Discord identity | `discord_id` unique, `username`, `avatar`, optional `email` |
| `sessions` | API sessions | `id` (sid), `user_id`, `token_hash`, `expires_at`, `revoked_at` |
| `subscriptions` | Paid access | `discord_id` or `email`, `plan`, `status`, `until`, `source`, `external_ref` |
| `devices` | Device slots | `(user_id, device_id)` unique, `device_name`, `last_seen_at` |
| `webhook_events` | Webhook idempotency | `event_id` primary key, raw payload, `processed_at` |
| `oauth_states` | OAuth CSRF state | hashed state primary key, `return_url`, `expires_at`, `used_at` |
| `rate_limits` | Refresh throttle | `(subject, bucket_minute)` primary key, `count` |

All `*_at`/`until` columns are Unix milliseconds unless noted; JWT `exp`/`iat`
are Unix seconds. Indexes cover `users.discord_id`, `sessions.expires_at`,
`subscriptions.discord_id`, `subscriptions.email`, `subscriptions.until`,
`devices.user_id`, `webhook_events.received_at` and `oauth_states.expires_at`.

Subscription resolution takes the row with the furthest `until` where
`until > now` and `status IN ('active', 'cancelled')`. A cancelled subscription
therefore keeps access until the paid period ends.

---

## Local development

```sh
cd backend
npm install
cp .dev.vars.example .dev.vars      # PowerShell: Copy-Item .dev.vars.example .dev.vars
npm run db:migrate:local
npm run dev                          # http://127.0.0.1:8787
```

Fill `.dev.vars` with:

- `LICENSE_PRIVATE_KEY` — PKCS#8 DER base64, a raw 32-byte seed in base64, or a
  complete Ed25519 private JWK JSON.
- `DISCORD_CLIENT_ID`, `DISCORD_CLIENT_SECRET`, `DISCORD_REDIRECT_URI`.
- `BOOSTY_WEBHOOK_SECRET`.

Add `http://127.0.0.1:8787/auth/discord/callback` to the Discord application's
redirect URIs for local login. The desktop `return` value is always a loopback
listener such as `http://127.0.0.1:53682/callback`; no scheme allowlist or
production config change is needed.

`npm run dev` prints the local Worker URL. Point `DISCORD_REDIRECT_URI` at the
same host and path (`http://127.0.0.1:8787/auth/discord/callback` by default).

### Tests

```sh
npm test             # vitest inside workerd against local D1
npx tsc --noEmit     # strict type check
```

The suite covers token sign/verify, expiry, wrong key, JWKS/`kid` matching,
device limits, plan gating, refresh rate limiting, webhook signatures,
idempotency, expiry extension, loopback return-URL policy, state binding and
replay, session delivery in query + fragment, CORS allow/deny, and OAuth state
validation with a mocked Discord exchange. Tests run in `workerd` via
`@cloudflare/vitest-plugin` (the renamed successor to
`@cloudflare/vitest-pool-workers`) with isolated cleanup between cases.

---

## Deploy steps (not executed)

Prerequisites: Node 20+, a Cloudflare account, a Discord application, and a
Boosty webhook integration.

### Repeatable deploy (`scripts/deploy.ps1`)

`backend/scripts/deploy.ps1` runs migrations, sets Worker secrets from process
environment variables, deploys, and prints the Worker URL. It never writes
secret values to disk or to its output.

```powershell
cd backend
$env:DISCORD_CLIENT_ID     = "<discord application id>"
$env:DISCORD_CLIENT_SECRET = "<discord client secret>"
$env:LICENSE_PRIVATE_KEY   = "<pkcs8 base64 or jwk json>"
$env:BOOSTY_WEBHOOK_SECRET = "<boosty hmac secret>"
# Optional:
$env:DISCORD_REDIRECT_URI         = "https://<worker-host>/auth/discord/callback"
$env:LICENSE_PREVIOUS_PUBLIC_KEY  = "<old public key>"

# Preview everything first (no mutations):
./scripts/deploy.ps1 -WhatIf        # or: ./scripts/deploy.ps1 --dry-run

# Real deploy:
./scripts/deploy.ps1
```

Flags: `-SkipMigrations`, `-SkipSecrets`, `-SkipDeploy`, `-Database <name>`,
`-WorkerUrl <url>` (used for the final printout when `wrangler` output cannot
be parsed). `--dry-run` and `--whatif` are accepted as Unix-style aliases.
Use Windows PowerShell 5.1 or PowerShell 7+.

The first run still needs the D1 database id in `wrangler.toml` (step 1 below)
and the Discord redirect URI registered in the Discord portal. Secret values
are piped to `wrangler secret put` on stdin, so they do not appear in the
process command line.

### Manual steps (equivalent)

1. **Create the database**

   ```sh
   npx wrangler d1 create projectneb-pro
   ```

   Copy the printed `database_id` into `wrangler.toml`, replacing
   `00000000-0000-0000-0000-000000000000`.

2. **Apply migrations to production D1**

   ```sh
   npx wrangler d1 migrations apply projectneb-pro --remote
   ```

3. **Generate a signing key** (keep the private value out of the repo)

   ```sh
   node -e "const {generateKeyPairSync}=require('crypto');const {privateKey}=generateKeyPairSync('ed25519');console.log(privateKey.export({format:'der',type:'pkcs8'}).toString('base64'))"
   ```

   The Worker derives the public key automatically. The optional public-key
   command for rotation is in the next section.

4. **Set Worker secrets**

   ```sh
   npx wrangler secret put LICENSE_PRIVATE_KEY
   npx wrangler secret put DISCORD_CLIENT_SECRET
   npx wrangler secret put BOOSTY_WEBHOOK_SECRET
   ```

   Paste values at the prompts. Optionally set `LICENSE_PREVIOUS_PUBLIC_KEY`
   during a rotation.

5. **Set public configuration** in `wrangler.toml` `[vars]`:

   - `DISCORD_CLIENT_ID`
   - `DISCORD_REDIRECT_URI` — must be exactly
     `https://<worker-host>/auth/discord/callback` and match the Discord portal.
   - Tune `MAX_DEVICES` (default 3), `RATE_LIMIT_PER_MINUTE` (30),
     `LICENSE_TTL_SECONDS` (900), `SESSION_TTL_SECONDS` (2592000).

6. **Deploy**

   ```sh
   npx wrangler deploy
   ```

7. **Verify**

   ```sh
   curl https://<worker-host>/health
   curl https://<worker-host>/api/keys/jwks
   ```

8. **Configure Boosty** to POST to
   `https://<worker-host>/webhooks/boosty` with header
   `X-Boosty-Signature: sha256=<hex(HMAC-SHA256(raw_body, secret))>`.
   Base64 signatures and the `x-signature` header are also accepted.

### Free-tier limits (at the time of writing)

| Resource | Cloudflare Free | Effect here |
| --- | --- | --- |
| Worker requests | 100,000/day | Each health/OAuth/API/webhook call is one request. |
| Worker CPU | 10 ms/request | Ed25519 sign/verify and D1 round-trips fit comfortably. |
| D1 storage | 5 GB | Users, sessions and events are tiny. |
| D1 rows read | 5,000,000/day | A refresh performs roughly 5–8 reads. |
| D1 rows written | 100,000/day | A refresh writes the device, rate-limit row and subscription lookups; expect roughly 10k–20k refreshes/day before this becomes the first limit. |

Check the Cloudflare dashboard for current limits; they can change. For a large
user base, the per-request D1 writes (especially the rate-limit counter) are the
first thing to move to a Durable Object or KV-backed limiter.

Product limits enforced by this Worker: **3 devices per account**, **15-minute
license**, **30-day session**, **30 refresh requests per user per minute**,
**10-minute OAuth state**.

---

## Key rotation

`/api/keys/jwks` publishes the current key and, if configured, one previous
public key. Tokens are verified against both.

1. Generate a new key (private above). Print its SPKI public key as base64:

   ```sh
   node -e "const {generateKeyPairSync}=require('crypto');const {publicKey}=generateKeyPairSync('ed25519');console.log(publicKey.export({format:'der',type:'spki'}).toString('base64'))"
   ```

2. Set the **old** public key as the `LICENSE_PREVIOUS_PUBLIC_KEY` secret, and
   the new private key as `LICENSE_PRIVATE_KEY`. Deploy both changes together.
   New licenses use the new `kid`; old tokens still verify.
3. Keep the previous key for at least the session TTL (30 days) if you do not
   want to log every client out. Removing it earlier invalidates old sessions
   and any unexpired license tokens immediately.
4. Remove `LICENSE_PREVIOUS_PUBLIC_KEY` after the window.

If the private key is compromised, rotate immediately and do **not** keep the
old public key. Also revoke sessions:

```sql
UPDATE sessions SET revoked_at = (unixepoch() * 1000) WHERE revoked_at IS NULL;
```

Clients will have to log in again. Existing offline tokens stay valid only
until their 15-minute `exp`.

---

## Security notes

- The private key exists only as the `LICENSE_PRIVATE_KEY` secret. It is never
  returned, logged or committed. Test runs generate an ephemeral key.
- Discord code exchange and `/users/@me` run server-side. The client never
  supplies a Discord identity the server trusts.
- OAuth state is random, stored only as a SHA-256 hash, single-use, bound to
  the loopback return URL and expires in 10 minutes.
- `return` is restricted to `http://127.0.0.1:<port>/callback` and
  `http://localhost:<port>/callback`; the policy is a raw-string match, so
  parser normalisation cannot bypass it. The URL is revalidated at callback
  time before redirecting.
- Sessions are delivered in the fragment and (loopback only) the query. Because
  non-loopback returns are impossible, the query token cannot reach a remote
  origin; the redirect sets `Referrer-Policy: no-referrer`.
- Browser CORS is restricted to `https://neb.ui` and loopback origins; requests
  without `Origin` (CLI, worker-to-worker) are unaffected.
- Sessions are signed JWTs backed by a D1 row containing a SHA-256 token hash;
  revoked or expired sessions are rejected on every API call.
- Boosty signatures are HMAC-SHA256 over the exact raw body and compared in
  constant time.
- The device cap uses a single guarded `INSERT ... SELECT ... WHERE count <
  limit`, so concurrent refreshes cannot overfill it.
- No secret values are present in `wrangler.toml` or `.dev.vars.example`.

## Known concerns / follow-ups

- **Email-only Boosty purchases** are stored on the subscription but cannot be
  auto-linked to a Discord user with the `identify` scope. `users.email` exists
  for a future linking flow or a manual `UPDATE subscriptions SET discord_id`.
- **No device removal endpoint.** Users at the cap need support to delete a
  `devices` row, or a future `DELETE /api/devices/:id` should be added.
- **Refunds/cancellations keep access until `until`.** A refund event is mapped
  to `cancelled`; if refunds must revoke immediately, handle `refund` with an
  explicit `until = now`.
- **Revocation is bounded by token lifetime** for offline clients: a revoked
  subscription remains usable until the current 15-minute license expires.
- **Rate limiting is a fixed one-minute window per user** and is approximate at
  minute boundaries. It is a cost guard, not a hard security boundary.
- **Webhook format is a contract.** The handler accepts
  `{ id, event, subscriber: { discord_id | email, plan, until | durationDays } }`;
  adjust it if the real Boosty payload differs.
- **Loopback redirect only.** A future non-desktop client (for example a web
  app) cannot use this callback directly; it would need a first-class redirect
  target policy with the same strict validation.
- **No retention job.** `sessions`, `webhook_events` and expired OAuth states can
  grow over time. Add a Cron Trigger to delete rows older than the desired
  retention (for example 90 days for webhook events, 30 days past session
  expiry).
- D1 serialises writes in one primary region. Very high refresh concurrency
  should move counters and device checks to a Durable Object.
