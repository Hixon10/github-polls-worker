-- Installations we've seen (populated by /app/installed and webhooks).
CREATE TABLE IF NOT EXISTS installations (
  installation_id  INTEGER PRIMARY KEY,
  account_login    TEXT    NOT NULL,
  account_id       INTEGER NOT NULL,
  created_at       INTEGER NOT NULL,
  suspended_at     INTEGER
);
CREATE INDEX IF NOT EXISTS idx_installations_login
  ON installations(lower(account_login));

-- Created polls. Votes themselves live in the repo's CSV file.
CREATE TABLE IF NOT EXISTS polls (
  id               TEXT    PRIMARY KEY,          -- random UUID (public URL)
  installation_id  INTEGER NOT NULL,
  repo_owner       TEXT    NOT NULL,
  repo_name        TEXT    NOT NULL,
  csv_path         TEXT    NOT NULL,             -- polls/<id>.csv
  question         TEXT    NOT NULL,
  created_at       INTEGER NOT NULL,
  FOREIGN KEY (installation_id) REFERENCES installations(installation_id)
);
CREATE INDEX IF NOT EXISTS idx_polls_installation ON polls(installation_id);

-- User submitted /app/new but we don't have an installation yet – they're
-- being bounced through GitHub's App install. Short-lived.
CREATE TABLE IF NOT EXISTS pending_polls (
  state        TEXT    PRIMARY KEY,              -- random token, also passed to GH
  repo_owner   TEXT    NOT NULL,
  repo_name    TEXT    NOT NULL,
  question     TEXT    NOT NULL,
  created_at   INTEGER NOT NULL
);

-- CSRF for voter OAuth (single use, short-lived).
CREATE TABLE IF NOT EXISTS oauth_states (
  state       TEXT PRIMARY KEY,
  poll_id     TEXT NOT NULL,
  created_at  INTEGER NOT NULL
);