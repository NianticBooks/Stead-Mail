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

## Phase 7 (mail completeness) — what exists

- **Sent copy.** After a message is delivered, a copy is filed in the account's Sent folder with IMAP APPEND
  (iCloud does not do it for you). Gmail is skipped because it files its own. The Sent folder is found by its
  special-use flag, then by common names, so an account must have been synced once. If filing fails the message
  still counts as sent; the copy is retried on the next `outbox flush`/`run`. Bcc is never in the filed copy.
- **Reply all.** `message.reply_all` (GUI: Reply all button, `Shift+R`) returns the sender plus every other To/Cc
  address minus your own, without repeats. Replying to a message you sent goes to its original recipients. The Cc
  header is now stored at sync time; older cached messages read it from the server once.
- **Drafts.** Closing a changed compose window offers Save draft / Discard / Keep editing. Drafts live in the local
  database only (`drafts.save|list|get|delete`, `mailcore drafts list|show|delete`, sidebar Drafts). They are not
  mirrored to the server's Drafts folder, so they do not appear in other mail apps. Sending a draft deletes it.
- Schema v11 (outbox sent_copy/copy_error, messages.cc_addrs, drafts).

## Phase 6 (two-way calendar sync) — what exists

- **Linked calendars.** `mailcore cal remote` lists what your Google and iCloud accounts offer; `cal link "Name"`
  mirrors one as a local calendar (`kind` google or caldav). Linking never changes the server. `cal unlink` drops the
  local copy only. In the GUI: the link button in the Calendars header opens "Connect calendars".
- **Pull first, then push.** Google uses `syncToken` deltas (first sync reaches back 90 days; an expired token
  triggers a clean resync). CalDAV compares the collection's change tag, then ETags, and fetches only what changed.
  Local creates, edits and deletes are sent with `If-Match`, so the server refuses a write over a version we never saw.
- **Remote wins, visibly.** When the same event changed on both sides, the server's version is kept and the log records
  both versions ("Yours: … / Server: …"). The same goes for deleted-there/edited-here and deleted-here/changed-there
  (the event comes back, logged). Nothing is lost silently: `mailcore cal log` or the GUI's Sync log.
- **Fidelity.** Alarms and changed single occurrences of an iCloud series are kept verbatim and written back on edit,
  so an edit here does not delete a reminder there. Google single-occurrence removals become exceptions here.
  Pushing never sends invitations (`sendUpdates=none`). Retried creates cannot duplicate (Google ids derive from ours).
- **When it runs:** `s` in the calendar, the refresh button, `mailcore cal sync`, on first opening the calendar, and
  every five minutes while the app is open. One sync at a time; a failing calendar reports its error and the others
  still sync (a warning mark in the sidebar, details in the log).
- **Migration v10:** `sync_log`, `events.extra`, per-calendar last-sync status. **RPC:** `calendar.remote/link/unlink/sync`,
  `sync.log`, `sync.log_clear`. `MAILCORE_CALDAV_URL` points CalDAV at another server (tests, self-hosting).
- **Not done:** editing one occurrence of a series (Google "this event only" changes are reported as skipped), moving
  an event between calendars, reminders, read-only detection for shared iCloud calendars (a refused write shows in the
  log), a push that loses a race (reconciled on the next sync with the server winning), macOS.
- Verified: 162 Rust tests (stateful fake Google and CalDAV servers: first sync, token reuse, expired token, push with
  ETags, every conflict shape, alarms/overrides round trip, ctag skip, error isolation), the real binary against a
  local CalDAV fake (link, pull, push, server edit), and the app: sync, conflict log and Connect dialog.
  A read-only `cal remote` against the real accounts listed both; no real calendar was linked or written.

## Phase 5 (local calendar) — what exists

- **Calendars and events** live in SQLite (migration v9). A default local "Personal" calendar exists from the start;
  the old accepted-proposal table is folded into it. Events carry what two-way sync needs (remote id, etag, dirty
  flag, tombstone), which Phase 6 uses.
- **Time zones done properly.** An event keeps the IANA zone its wall clock belongs to and instants are stored in
  UTC. "Every Thursday at 9 AM" stays 9 AM across Daylight Saving changes (tested across both transitions).
  All-day events span whole local days with an exclusive end, so a three-day trip over a DST change is three days.
- **Recurrence** uses the `rrule` crate: daily, weekly, monthly (including "second Tuesday"), yearly, COUNT, UNTIL,
  INTERVAL. Expansion is bounded by the query window, so endless rules are cheap. One occurrence can be removed from
  a series (an EXDATE); editing a series keeps those exceptions unless the start or rule changes.
  Not yet: editing a single occurrence (RECURRENCE-ID overrides). Those are reported as skipped on import.
- **iCalendar import and export** (`.ics`): folded lines, escaping, TZID zones (including Windows names), all-day
  dates, EXDATE, SEQUENCE. Importing the same file again updates events by UID instead of duplicating them.
  Files over 5 MB or 5000 events are refused; broken events are skipped with a reason, never fatal.
- **Conflict warnings, never blocks.** Adding or accepting an event shows what it overlaps (all-day banners and
  cancelled events do not count). The AI proposal card shows the same warning before you click Add.
- **CLI:** `mailcore cal list | new | add | agenda | rm | conflicts | import | export`.
  **RPC:** `calendar.list/create/update/delete/range/conflicts/export/import`, `event.get/create/update/delete`.
- **GUI:** a Mail / Calendar switch in the sidebar. Month, week, day and agenda views; overlapping events sit side by
  side; a live "now" line; create and edit dialog with repeat presets, time zone, guests and a live overlap warning;
  per-calendar colour, visibility, rename, delete, import and export. Keys: `g c` / `g i` switch sections, `t` today,
  `n` new event, `[` `]` or arrows step, `m w d a` pick the view.
- Everything stays human-in-the-loop: nothing reaches the calendar except by an explicit click or command.
- Verified: 148 Rust tests (recurrence across DST, ICS round trips, migration from a real v8 schema, RPC), 60
  frontend tests run under four machine time zones, and the real app driven in a throwaway profile (all four
  views, new-event overlap warning, save, series dialog, Escape, switching sections).
- **Not verified / not done:** accepting a proposal through the live AI card, drag to move or resize, reminders, RECURRENCE-ID overrides,
  macOS.

## Phase 4 (AI layer) — what exists

Off until you turn it on (`mailcore ai enable`, or Settings → AI). Everything it produces is a proposal.

- **Triage**: category (conversation, scheduling, transaction, notification, newsletter, other), needs-reply,
  urgency and a one-line summary, shown in the list and the reading pane. Mail you wrote yourself, or that
  sits in Sent/Drafts, never needs a reply.
- **Events**: invitations (`.ics`) are parsed deterministically, including Windows zone names, floating times,
  all-day dates and cancellations. Free-text dates are read by a model; each proposal must quote the sentence
  it came from, the quote is checked against the message, the date must be plausible, past or vague sentences
  are rejected, and weekday names are corrected in code because models get weekday arithmetic wrong.
  Accepting a proposal adds it to the default local calendar (see Phase 5).
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
