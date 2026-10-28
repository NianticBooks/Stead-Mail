# ai-mail-client (working name)

Dark-mode-first, AI-native desktop email + calendar client. Linux first, macOS next.

## Architecture

```
crates/mailcore        Rust core: accounts, SQLite store, secrets, IMAP sync, calendar (later), AI (later)
crates/mailcore-cli    `mailcore` binary: CLI + JSON-RPC 2.0 daemon (`mailcore serve`)
apps/desktop           Tauri 2 + Svelte 5 shell. Commands are thin adapters over mailcore::Api
```

One rule: **all logic lives in `mailcore::Api`.** The CLI, the daemon and the Tauri shell call the
same methods. A future native SwiftUI shell (BAMA) talks to `mailcore serve` over JSON-RPC.

Decisions (2026-10-27): Svelte frontend; Tauri 2, not a pure-Rust UI; separate daemon boundary;
iCloud + Gmail + generic IMAP first with app passwords; OAuth2 only for Google Calendar; Secret
Service / Keychain with an encrypted-file fallback; hybrid AI (local Ollama for extraction and
triage, cloud for drafting) with 100% human-in-the-loop; Google Calendar REST + iCloud CalDAV
two-way sync with remote-wins conflict policy; AUR packaging first; MIT.

## Phase 2 (sync engine) — what exists

- Incremental IMAP sync: first pass pulls the newest N, later passes fetch only new UIDs, one
  flags fetch over the cached range detects flag changes and expunges, UIDVALIDITY resets the cache
- `mailcore sync --backfill N` walks older mail in batches; `mailcore watch` keeps an IDLE
  connection open and re-syncs on every push, reconnecting with backoff
- `mailcore show ACCOUNT UID` downloads one body (PEEK, so it never marks mail as read), parses
  MIME text, HTML and attachment metadata, and caches it
- `mailcore search QUERY` is FTS5 full text over cached subjects, addresses and downloaded bodies
- Conversation threading from In-Reply-To/References (`mailcore thread`), stable under any
  arrival order; schema v4
- Outbox: `mailcore send` builds the message once (To/Cc/Bcc, reply headers, attachments), stores it,
  and delivers; temporary failures retry at 1 min, 5 min, 15 min, 1 h, 6 h, 12 h, 24 h and give up after
  8 attempts, while rejections and bad credentials fail at once. `mailcore outbox list|flush|run|retry|cancel`;
  the daemon (`serve --socket`) flushes every 30 s. Bcc never appears in the message. Attachments are
  read when queued. Claiming an item before sending stops two processes double-sending.
- `mailcore attachment ACCOUNT UID INDEX --dir D` saves an attachment with a sanitised name and
  never overwrites; `mailcore sync ACCOUNT --all` covers every folder except Trash, Junk, All Mail and
  virtual views
- Not yet: copying sent mail into the Sent folder (Gmail does it itself; iCloud does not), watching
  several folders at once, POP3 (deprioritised)

## Phase 1 (auth + daemon) — what exists

- `mailcore account test` now checks IMAP and SMTP logins; `mailcore send` sends plain text
- `mailcore caldav ACCOUNT` discovers CalDAV calendars (iCloud reuses the app-specific password)
- `mailcore google configure|login|accounts|calendars|logout`: OAuth2 PKCE with a loopback
  redirect, Calendar scopes only (no CASA audit), refresh token kept in the secret store
- `mailcore serve --socket` runs the JSON-RPC daemon on `$XDG_RUNTIME_DIR/mailcore.sock` (mode 0600)
- SQLite schema v2 adds `oauth_identities`; migrations are applied in order on open

Google setup: Google Cloud Console → create a *Desktop app* OAuth client, enable the Google
Calendar API, add yourself as a test user, then
`mailcore google configure --client-id ID --client-secret-stdin` and `mailcore google login`.
Note: apps in Testing mode have refresh tokens that expire after 7 days.

## Phase 0 (spike) — what exists

- `mailcore account add|list|remove|test`, `mailcore sync`, `mailcore messages`, `mailcore serve`
- IMAP over rustls: LOGIN, LIST, SELECT, FETCH of newest N headers; UIDVALIDITY reset handling
- SQLite schema v1 (accounts, mailboxes, messages) with WAL
- Secrets: OS keyring, fallback to ChaCha20-Poly1305 + Argon2id file (`MAILCORE_SECRETS=file`)
- Tauri window with a diagnostics panel proving WebKitGTK renders on Wayland + Nvidia

## Run

```sh
# core + CLI
cargo test -p mailcore
cargo run -p mailcore-cli -- ping
cargo run -p mailcore-cli -- account add --name icloud --email you@icloud.com --provider icloud
cargo run -p mailcore-cli -- account test icloud
cargo run -p mailcore-cli -- sync icloud --limit 50
cargo run -p mailcore-cli -- messages icloud

# JSON-RPC daemon
echo '{"jsonrpc":"2.0","id":1,"method":"ping"}' | cargo run -q -p mailcore-cli -- serve

# desktop shell
cd apps/desktop && npm install && npm run tauri dev
```

Environment knobs: `MAILCORE_DATA_DIR`, `MAILCORE_SECRETS=keyring|file`, `MAILCORE_PASSPHRASE`,
`RUST_LOG=mailcore=debug`.

## Linux graphics notes

Tauri renders through WebKitGTK. On Nvidia + Wayland a blank window is a known failure mode. Try,
in order: `nvidia_drm.modeset=1`, `__NV_DISABLE_EXPLICIT_SYNC=1`, `WEBKIT_DISABLE_DMABUF_RENDERER=1`,
`WEBKIT_DISABLE_COMPOSITING_MODE=1`. The diagnostics panel shows which are set.
