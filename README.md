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

## Phase 4 (AI layer) — what exists

Off until you turn it on (`mailcore ai enable`, or Settings → AI). Everything it produces is a proposal.

- **Triage**: category (conversation, scheduling, transaction, notification, newsletter, other), needs-reply,
  urgency and a one-line summary, shown in the list and the reading pane. Mail you wrote yourself, or that
  sits in Sent/Drafts, never needs a reply.
- **Events**: invitations (`.ics`) are parsed deterministically, including Windows zone names, floating times,
  all-day dates and cancellations. Free-text dates are read by a model; each proposal must quote the sentence
  it came from, the quote is checked against the message, the date must be plausible, past or vague sentences
  are rejected, and weekday names are corrected in code because models get weekday arithmetic wrong.
  Accepting a proposal puts it in the local calendar table (views and sync come in Phase 5).
- **Drafting**: `Draft with AI` in a reply. Local by default. A cloud draft shows the exact system prompt and
  message and only runs if its SHA-256 matches what you were shown.
- **Local first**: Ollama (`qwen3:8b`, thinking off, JSON-schema output). Cloud (Claude Haiku for dates, Sonnet
  for drafts) is a second switch plus an API key in the keyring; event reading sends only date-bearing sentences.
- **Hostile mail**: messages that try to instruct an AI ("ignore previous instructions", role tags, "do not tell
  the user") are detected deterministically, never shown to a model, labelled by code, and the user is warned.
  Model output is validated against fixed vocabularies, and rules a model cannot be trusted with are enforced
  in code.
- **Audit**: every model call is logged by size only; Settings → AI and `mailcore ai audit` show what left.
- **Evaluation**: `mailcore ai eval` scores a model on 39 tuning cases; `--holdout` uses 14 cases never tuned
  against; `--no-guards` measures the model alone. qwen3:8b on the RTX 5060: holdout triage 87.5%, reply flag
  100%, event recall 100% / precision 80%, injection 3/3; about 1 s per call warm.
- Commands: `mailcore ai status|enable|disable|set|key|analyze|triage|proposals|accept|dismiss|calendar|draft|audit|eval`.
- Not verified live: cloud calls (no API key was available), only fake-server tests cover that path.

## Phase 3 (desktop GUI) — what exists

- Three-pane dark UI (Svelte 5): accounts and folders with server unread counts, message list with
  search, reading pane. Keyboard first: `j`/`k` move, `c` compose, `r` reply, `u` read/unread, `s` star,
  `/` search, `g i` inbox, `?` help, `Esc` closes dialogs
- HTML mail is sanitised in Rust (`mailcore::html`: scripts, forms, frames, dangerous URLs and risky CSS
  removed), then shown in a script-free sandboxed frame under its own CSP. Remote images are blocked
  until you load them for one message; the banner says how many were blocked
- Compose and reply (threading headers, quoted text, attachments, Cc/Bcc) through the outbox, with a
  discard guard; attachments save to Downloads without overwriting
- Settings: add iCloud, Gmail or generic IMAP accounts (the connection is checked and a bad account is
  not kept), Google Calendar sign-in, and a mark-read-on-open switch
- Live updates: one IDLE watcher per account inbox pushes changes into the window; the shell also
  retries queued mail every 30 s
- The Tauri shell is a pass-through to the JSON-RPC API (`rpc` command) plus browser sign-in, safe
  external links, reveal-in-folder and the watchers

Tests: 64 Rust (core, sanitiser attack vectors, flags, paging) and 30 UI logic tests
(`cd apps/desktop && npm test`). Not built yet: calendar views, reply-all, drafts, moving or deleting mail.

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
