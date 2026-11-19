-- ProjectNeb Pro licensing schema (D1 / SQLite).
-- All timestamp columns are Unix milliseconds unless noted otherwise.

PRAGMA foreign_keys = ON;

CREATE TABLE users (
  id          INTEGER PRIMARY KEY AUTOINCREMENT,
  discord_id  TEXT    NOT NULL UNIQUE,
  username    TEXT    NOT NULL,
  avatar      TEXT,
  email       TEXT,
  created_at  INTEGER NOT NULL,
  updated_at  INTEGER NOT NULL
);

CREATE INDEX idx_users_discord_id ON users (discord_id);

CREATE TABLE sessions (
  id          TEXT    PRIMARY KEY,
  user_id     INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
  token_hash  TEXT    NOT NULL,
  created_at  INTEGER NOT NULL,
  expires_at  INTEGER NOT NULL,
  revoked_at  INTEGER
);

CREATE INDEX idx_sessions_user_id ON sessions (user_id);
CREATE INDEX idx_sessions_expires_at ON sessions (expires_at);

CREATE TABLE subscriptions (
  id            INTEGER PRIMARY KEY AUTOINCREMENT,
  discord_id    TEXT,
  email         TEXT,
  plan          TEXT    NOT NULL DEFAULT 'pro',
  status        TEXT    NOT NULL DEFAULT 'active',
  until         INTEGER NOT NULL,
  source        TEXT    NOT NULL DEFAULT 'boosty',
  external_ref  TEXT,
  created_at    INTEGER NOT NULL,
  updated_at    INTEGER NOT NULL
);

CREATE INDEX idx_subscriptions_discord_id ON subscriptions (discord_id);
CREATE INDEX idx_subscriptions_email ON subscriptions (email);
CREATE INDEX idx_subscriptions_until ON subscriptions (until);
CREATE INDEX idx_subscriptions_status_until ON subscriptions (status, until);

CREATE TABLE devices (
  id          INTEGER PRIMARY KEY AUTOINCREMENT,
  user_id     INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
  device_id   TEXT    NOT NULL,
  device_name TEXT,
  created_at  INTEGER NOT NULL,
  last_seen_at INTEGER NOT NULL,
  UNIQUE (user_id, device_id)
);

CREATE INDEX idx_devices_user_id ON devices (user_id);
CREATE INDEX idx_devices_last_seen ON devices (last_seen_at);

CREATE TABLE webhook_events (
  event_id     TEXT    PRIMARY KEY,
  event_type   TEXT    NOT NULL,
  payload      TEXT    NOT NULL,
  received_at  INTEGER NOT NULL,
  processed_at INTEGER
);

CREATE INDEX idx_webhook_events_received_at ON webhook_events (received_at);

CREATE TABLE oauth_states (
  state_hash  TEXT    PRIMARY KEY,
  return_url  TEXT    NOT NULL,
  created_at  INTEGER NOT NULL,
  expires_at  INTEGER NOT NULL,
  used_at     INTEGER
);

CREATE INDEX idx_oauth_states_expires_at ON oauth_states (expires_at);

-- One row per subject per minute. subject is "user:<id>" or "ip:<address>".
CREATE TABLE rate_limits (
  subject TEXT    NOT NULL,
  bucket  INTEGER NOT NULL,
  count   INTEGER NOT NULL,
  PRIMARY KEY (subject, bucket)
);

CREATE INDEX idx_rate_limits_bucket ON rate_limits (bucket);
