# GitHub Polls — Cloudflare Worker (Rust)

A minimal, production-leaning backend for a GitHub App where **polls are CSV files** in your GitHub repo and **GitHub is
the auth provider for voters**.

**Example poll CSV** (real output from this worker): [`Hixon10/my-polls` → `polls/c52069e4-91f9-42d4-822b-08a6704eb134.csv`](https://github.com/Hixon10/my-polls/blob/main/polls/c52069e4-91f9-42d4-822b-08a6704eb134.csv)

## Flow

1. User visits `/app/new`, types a question + `owner/repo`.
2. If we already know an installation for that `owner`, we:
    - mint an installation token,
    - create `polls/<uuid>.csv` on `main`,
    - save a `polls` row,
    - show the poll link.
3. Otherwise, we save a short-lived "pending poll" and redirect to
   `https://github.com/apps/<slug>/installations/new?state=<token>`.
   After install, GitHub calls our `setup_url` (`/app/installed`) with
   `installation_id` and `state`. We verify with the App JWT, upsert the
   installation, finalize the pending poll, show the link.
4. Voters open `/p/:id`, sign in with GitHub (user-to-server OAuth via a
   **single fixed callback** `/p/callback`; the poll id is carried in the
   `state` param), submit a response. We append a line
   `timestamp,HMAC(pollId||userId),response` to the CSV with optimistic
   concurrency (If-Match via `sha`). Duplicate tags are rejected — one
   vote per GitHub user.
5. Webhooks (`installation`, `installation_repositories`) keep the
   `installations` table in sync. HMAC-verified.

## Prerequisites

```bash
rustup target add wasm32-unknown-unknown
cargo install worker-build
# Always use the latest wrangler — older versions bundle an old workerd
# that doesn't know the `cloudflare:workers` shim module or recent
# compatibility dates.
npm i -D wrangler@latest
```

## Configure the GitHub App

Settings → Developer settings → GitHub Apps → New GitHub App.

| Field                                                  | Value                                                            | 
|--------------------------------------------------------|------------------------------------------------------------------|
| Homepage URL                                           | `https://<worker>/`                                              |
| Setup URL (post-installation)                          | `https://<worker>/app/installed`  — check **Redirect on update** |
| Redirect on update                                     | ✅                                                                |
| User authorization callback URL                        | `https://<worker>/p/callback`  (single, stable path)             |
| Webhook → Active                                       | ✅                                                                |
| Webhook URL                                            | `https://<worker>/webhooks/github`                               |
| Webhook secret                                         | same value as `GITHUB_WEBHOOK_SECRET`                            |
| Permissions → Repository → Contents                    | **Read & write**                                                 |
| Subscribe to events                                    | none required                                                    |
| Where can this app be installed?                       | Any account                                                      |
| Request user authorization (OAuth) during installation | optional                                                         |

Copy these into your config (below):

- **App ID** (numeric, e.g. `438291`) → `GITHUB_APP_ID`
- **Client ID** (starts with `Iv1.` or `Iv23...`) → `GITHUB_APP_CLIENT_ID`
- **Public slug** from `https://github.com/settings/apps/<slug>` → `GITHUB_APP_SLUG`
- **Client secret** (generate once, shown once) → `GITHUB_APP_CLIENT_SECRET`
- **Private key** `.pem` (download) → `GITHUB_APP_PRIVATE_KEY`

### Private key: convert PKCS#1 → PKCS#8

GitHub's downloaded `.pem` is PKCS#1 (`-----BEGIN RSA PRIVATE KEY-----`).
The worker expects PKCS#8 (`-----BEGIN PRIVATE KEY-----`):

```bash
openssl pkcs8 -topk8 -nocrypt \
  -in  ~/Downloads/<your-app>.<date>.private-key.pem \
  -out ~/Downloads/<your-app>.pkcs8.pem
```

## `wrangler.toml` vars

Non-secret values go in `wrangler.toml`:

```toml name=wrangler.toml
[vars]
PUBLIC_BASE_URL = "https://<worker-or-tunnel>"
GITHUB_APP_ID = "438291"
GITHUB_APP_CLIENT_ID = "Iv23liXXXXXXXXXXXXXX"
GITHUB_APP_SLUG = "your-app-slug"
```

## Database

```bash
npx wrangler d1 create github-polls                 # copy id into wrangler.toml
npx wrangler d1 execute github-polls --file migrations/0001_init.sql           # remote
npx wrangler d1 execute github-polls --local --file migrations/0001_init.sql   # local
```

## Secrets (production)

```bash
# PEM: multi-line on the remote side is fine
npx wrangler secret put GITHUB_APP_PRIVATE_KEY < ~/Downloads/<your-app>.pkcs8.pem
npx wrangler secret put GITHUB_APP_CLIENT_SECRET
npx wrangler secret put GITHUB_WEBHOOK_SECRET

# 32 random bytes, base64 (standard or URL-safe — both accepted)
openssl rand -base64 32 | npx wrangler secret put SESSION_HMAC_KEY
openssl rand -base64 32 | npx wrangler secret put VOTER_ID_HMAC_KEY
```

⚠️ **Never rotate `VOTER_ID_HMAC_KEY`** after polls exist — it would let
every voter cast a second ballot under a new hash.

## Deploy

```bash
npx wrangler deploy
```

---

## Local development

### 1. `.dev.vars` (gitignored — dotenv, single-line values)

`.dev.vars` is dotenv-style, so the PEM must be a single line with `\n`
escapes. One-liner that copies it ready to paste:

```bash
awk 'BEGIN{ORS="\\n"} {print}' ~/Downloads/<your-app>.pkcs8.pem | pbcopy
```

```ini name=.dev.vars
GITHUB_APP_PRIVATE_KEY="-----BEGIN PRIVATE KEY-----\nMIIE...\n-----END PRIVATE KEY-----\n"
GITHUB_APP_CLIENT_SECRET=your_oauth_client_secret
GITHUB_WEBHOOK_SECRET=some-long-random-string
SESSION_HMAC_KEY=base64_any_variant_padding_ok
VOTER_ID_HMAC_KEY=base64_any_variant_padding_ok
```

### 2. Tunnel (GitHub needs to reach your laptop)

```bash
brew install cloudflared        # or: brew install ngrok
cloudflared tunnel --url http://localhost:8787
# => https://<something>.trycloudflare.com
```

Quick-tunnel URLs change on every restart. Each time, update **both**:

- `PUBLIC_BASE_URL` in `wrangler.toml`
- Homepage / Setup / Callback / Webhook URLs in the GitHub App settings

For a stable hostname, create a named tunnel
(`cloudflared tunnel create ...`).

### 3. Run

```bash
npx wrangler dev --local --persist-to .wrangler/state
```

### 4. Try it

1. `https://<tunnel>/app/new` → submit question + `yourname/some-repo`.
2. Install the App (first time only) → redirected to `/app/installed` →
   poll URL appears and a commit creating `polls/<uuid>.csv` lands.
3. Open the poll URL in another browser profile, sign in with a
   *different* GitHub user, vote → another commit.
4. Vote again with the same user → redirected to the CSV (already voted).

### Inspect local DB

```bash
npx wrangler d1 execute github-polls --local --command "SELECT * FROM installations;"
npx wrangler d1 execute github-polls --local --command "SELECT id, repo_owner, repo_name, question FROM polls;"
```

---

## Troubleshooting

| Symptom                                                                     | Cause / fix                                                                                                                                              |
|-----------------------------------------------------------------------------|----------------------------------------------------------------------------------------------------------------------------------------------------------|
| `No such module "cloudflare:workers"` at startup                            | Old wrangler. Run `npm i -D wrangler@latest` and invoke via `npx wrangler@latest`.                                                                       |
| `D1_TYPE_ERROR: Type 'bigint' not supported`                                | D1's JS driver rejects `BigInt`. All integer bindings in `src/db.rs` go through `f64` helpers (`n`, `n_opt`).                                            |
| `SESSION_HMAC_KEY: Invalid symbol …` / `Invalid padding`                    | The decoder accepts standard, URL-safe, padded, and unpadded base64 (`decode_b64_any`). Regenerate with `openssl rand -base64 32` and paste as-is.       |
| `incorrect_client_credentials` on OAuth exchange                            | `GITHUB_APP_CLIENT_SECRET` is wrong (often the webhook secret by mistake). Regenerate the **client** secret on the App page and re-set the var.          |
| `A JSON web token could not be decoded` (401 from `/app/installations/...`) | Wrong `GITHUB_APP_ID` (not the Client ID, not the installation ID) **or** the PEM is still PKCS#1. Convert to PKCS#8 as above.                           |
| GitHub: "redirect_uri is not associated with this application"              | Callback URL on the App must be exactly `https://<worker>/p/callback`.                                                                                   |
| GitHub 404 at `/apps/<slug>/installations/new`                              | `GITHUB_APP_SLUG` is wrong. Copy it from `https://github.com/settings/apps/<slug>`.                                                                      |
| Cookie set but voter page loops back to login                               | Make sure the callback builds its 302 manually (`Response::empty().with_status(302).with_headers(headers)`); `Response::redirect` can drop `Set-Cookie`. |

## Security notes

- Webhooks are rejected unless `X-Hub-Signature-256` verifies against `GITHUB_WEBHOOK_SECRET`.
- Voter IDs in CSVs are `HMAC(VOTER_ID_HMAC_KEY, pollId || userId)` — the repo never sees raw GitHub IDs.
- Session cookies are signed with `SESSION_HMAC_KEY` and set `HttpOnly; Secure; SameSite=Lax`.
- OAuth `state` values are single-use, 10-minute TTL, stored in D1.
- Don't commit `*.pem` or `.dev.vars`. Private key leak? Generate a new one in the App settings and delete the old key.
