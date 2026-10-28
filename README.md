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
