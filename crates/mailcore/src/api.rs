//! The single public surface of mailcore. Everything a UI, CLI or agent can do goes through
//! [`Api`]; the JSON-RPC layer in [`crate::rpc`] is a thin serialiser over these methods.

use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::account::{Account, Provider};
use crate::caldav::{self, CalDavCalendar, CalDavClient, CalDavHome};
use crate::error::{Error, Result};
use crate::google::{self, CalendarSummary, ClientConfig, GoogleEndpoints};
use crate::imap;
use crate::oauth::{self, TokenSet};
use crate::secrets::SecretStore;
use crate::smtp;
use crate::store::{MailboxRow, MailboxStateUpdate, MessageDetail, MessageRow, NewOutbox, OAuthIdentity, OutboxItem, SearchHit, Store};
use crate::sync::{self, LocalView, SyncOptions};

const GOOGLE: &str = "google";
const GOOGLE_CLIENT_SECRET_KEY: &str = "oauth/google/client";

pub struct Api {
    store: Mutex<Store>,
    secrets: Box<dyn SecretStore>,
    data_dir: PathBuf,
    http: reqwest::Client,
    google_endpoints: GoogleEndpoints,
    smtp_transport: smtp::Transport,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PingInfo {
    pub name: &'static str,
    pub version: &'static str,
    pub data_dir: String,
    pub secrets_backend: &'static str,
    pub accounts: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddAccountRequest {
    pub name: String,
    /// Replace an existing account with the same name or email instead of failing.
    #[serde(default)]
    pub replace: bool,
    pub email: String,
    pub provider: Provider,
    pub password: String,
    pub imap_host: Option<String>,
    pub imap_port: Option<u16>,
    pub smtp_host: Option<String>,
    pub smtp_port: Option<u16>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionReport {
    pub account: String,
    pub imap_host: String,
    pub mailboxes: Vec<imap::MailboxInfo>,
    pub smtp_host: String,
    /// `None` = SMTP login OK. IMAP failure is a hard error instead.
    pub smtp_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SendRequest {
    pub account: String,
    pub to: Vec<String>,
    #[serde(default)]
    pub cc: Vec<String>,
    /// Envelope only; never appears in the message.
    #[serde(default)]
    pub bcc: Vec<String>,
    pub subject: String,
    pub body: String,
    #[serde(default)]
    pub in_reply_to: Option<String>,
    #[serde(default)]
    pub references: Vec<String>,
    /// Local file paths, read when the message is queued so later changes to the files do not matter.
    #[serde(default)]
    pub attachments: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SendOutcome {
    pub item: OutboxItem,
    /// True when the server accepted the message on the first attempt.
    pub delivered: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FlushReport {
    pub attempted: usize,
    pub sent: usize,
    pub retrying: usize,
    pub failed: usize,
    pub items: Vec<OutboxItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedAttachment {
    pub path: String,
    pub filename: String,
    pub content_type: String,
    pub bytes: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SyncAllReport {
    pub reports: Vec<SyncReport>,
    /// Mailboxes deliberately left out (Trash, Junk, All Mail, unselectable containers).
    pub skipped: Vec<String>,
    /// Mailboxes that failed to sync, with the reason. One bad folder does not stop the rest.
    pub errors: Vec<(String, String)>,
}

/// Seconds to wait before retry number `attempt` (1-based) of a temporarily failing message.
pub fn outbox_backoff_secs(attempt: i64) -> i64 {
    const STEPS: [i64; 7] = [60, 300, 900, 3600, 6 * 3600, 12 * 3600, 24 * 3600];
    STEPS[(attempt.max(1) as usize - 1).min(STEPS.len() - 1)]
}

/// Give up after this many attempts; the item stays visible as failed until retried or cancelled.
pub const OUTBOX_MAX_ATTEMPTS: i64 = 8;

/// Locks older than this are assumed to belong to a dead process.
const OUTBOX_STALE_LOCK_SECS: i64 = 600;

/// Folders worth syncing by default: not Trash, Junk, All Mail, starred/important virtual views,
/// or unselectable containers. The name check catches servers (iCloud) that omit special-use flags.
pub fn syncable_mailbox(name: &str, attributes: &str) -> bool {
    let attrs = attributes.to_ascii_lowercase();
    let special = attrs.split_whitespace().any(|t| matches!(t, "noselect" | "all" | "trash" | "junk" | "flagged")) || attrs.contains("important");
    // Whole words only, so a folder called "Cabinet" is not mistaken for a bin.
    let last = name.rsplit('/').next().unwrap_or(name).to_ascii_lowercase();
    let bad_name = last.split(|c: char| !c.is_alphanumeric()).any(|w| matches!(w, "junk" | "spam" | "trash" | "deleted" | "bin"));
    !special && !bad_name
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalDavReport {
    pub account: String,
    pub home: CalDavHome,
    pub calendars: Vec<CalDavCalendar>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SyncReport {
    pub account: String,
    pub mailbox: String,
    /// Messages in the mailbox on the server.
    pub exists: u32,
    pub uidvalidity: Option<u32>,
    pub uidvalidity_changed: bool,
    /// New headers the server returned (includes backfilled ones).
    pub fetched: usize,
    /// Of those, how many were actually inserted.
    pub stored: usize,
    pub backfilled: usize,
    pub flags_updated: usize,
    pub expunged: usize,
    /// Messages cached locally for this mailbox after the pass.
    pub cached_total: i64,
    pub mailboxes_seen: usize,
}

impl Api {
    pub fn new(store: Store, secrets: Box<dyn SecretStore>, data_dir: PathBuf) -> Api {
        let http = crate::http_client();
        Api { store: Mutex::new(store), secrets, data_dir, http, google_endpoints: GoogleEndpoints::default(), smtp_transport: smtp::Transport::Tls }
    }

    /// Deliver over clear text. Loopback test servers only.
    pub fn with_plain_smtp(mut self) -> Api {
        self.smtp_transport = smtp::Transport::Plain;
        self
    }

    /// Point Google calls at a different host (tests).
    pub fn with_google_endpoints(mut self, ep: GoogleEndpoints) -> Api {
        self.google_endpoints = ep;
        self
    }

    /// Open the default database and secret store under [`crate::data_dir`].
    pub fn open_default() -> Result<Api> {
        let dir = crate::data_dir();
        let store = Store::open(&dir.join("mailcore.db"))?;
        let secrets = crate::secrets::open_default(&dir)?;
        Ok(Api::new(store, secrets, dir))
    }

    fn store(&self) -> std::sync::MutexGuard<'_, Store> {
        self.store.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn ping(&self) -> Result<PingInfo> {
        Ok(PingInfo {
            name: "mailcore",
            version: env!("CARGO_PKG_VERSION"),
            data_dir: self.data_dir.display().to_string(),
            secrets_backend: self.secrets.backend_name(),
            accounts: self.store().list_accounts()?.len(),
        })
    }

    pub fn list_accounts(&self) -> Result<Vec<Account>> {
        self.store().list_accounts()
    }

    pub fn add_account(&self, req: AddAccountRequest) -> Result<Account> {
        let mut account = Account::from_preset(&req.name, &req.email, req.provider)?;
        if let Some(h) = req.imap_host { account.imap.host = h; }
        if let Some(p) = req.imap_port { account.imap.port = p; account.imap.starttls = p != 993; }
        if let Some(h) = req.smtp_host { account.smtp.host = h; }
        if let Some(p) = req.smtp_port { account.smtp.port = p; account.smtp.starttls = p != 465; }
        if account.imap.host.is_empty() {
            return Err(Error::Invalid("imap_host is required for custom accounts".into()));
        }
        if req.password.is_empty() {
            return Err(Error::Invalid("password must not be empty".into()));
        }
        // Names and emails are lookup keys, so they must be unique. Without this, re-adding an
        // account after a typo silently leaves the old one first in line (seen 2026-10-27).
        let existing: Vec<Account> = self
            .store()
            .list_accounts()?
            .into_iter()
            .filter(|a| a.name.eq_ignore_ascii_case(&account.name) || a.email.eq_ignore_ascii_case(&account.email))
            .collect();
        if !existing.is_empty() {
            if !req.replace {
                return Err(Error::Invalid(format!(
                    "an account named '{}' or using '{}' already exists; remove it or pass replace",
                    existing[0].name, existing[0].email
                )));
            }
            for old in &existing {
                self.store().delete_account(&old.id)?;
                self.secrets.delete(&old.password_secret_key())?;
            }
        }
        self.secrets.set(&account.password_secret_key(), &req.password)?;
        if let Err(e) = self.store().insert_account(&account) {
            let _ = self.secrets.delete(&account.password_secret_key());
            return Err(e);
        }
        Ok(account)
    }

    pub fn remove_account(&self, key: &str) -> Result<Account> {
        let account = self.store().find_account(key)?;
        self.store().delete_account(&account.id)?;
        self.secrets.delete(&account.password_secret_key())?;
        Ok(account)
    }

    fn password_for(&self, account: &Account) -> Result<String> {
        self.secrets
            .get(&account.password_secret_key())?
            .ok_or_else(|| Error::Secrets(format!("no password stored for account '{}'", account.name)))
    }

    /// Log in over IMAP and SMTP and list mailboxes. Sends and stores nothing.
    pub async fn test_connection(&self, key: &str) -> Result<ConnectionReport> {
        let account = self.store().find_account(key)?;
        let password = self.password_for(&account)?;
        let client = imap::connect_tls(&account.imap.host, account.imap.port).await?;
        let mut session = imap::login(client, &account.imap.username, &password).await?;
        let mailboxes = imap::list_mailboxes(&mut session).await?;
        imap::logout(session).await?;
        let smtp_error = smtp::verify_login(&account.smtp, &password, self.smtp_transport).await.err().map(|e| e.to_string());
        Ok(ConnectionReport { account: account.name, imap_host: account.imap.host, mailboxes, smtp_host: account.smtp.host, smtp_error })
    }

    // ----- outbox -----

    fn now() -> i64 {
        crate::oauth::now_unix()
    }

    /// Build the message, store it in the outbox and return. Nothing is sent yet.
    pub fn outbox_enqueue(&self, req: SendRequest) -> Result<OutboxItem> {
        let account = self.store().find_account(&req.account)?;
        let mut attachments = Vec::new();
        for path in &req.attachments {
            let p = std::path::Path::new(path);
            let meta = std::fs::metadata(p).map_err(|e| Error::Invalid(format!("cannot read attachment {path}: {e}")))?;
            if !meta.is_file() {
                return Err(Error::Invalid(format!("attachment {path} is not a regular file")));
            }
            if meta.len() as usize > smtp::MAX_MESSAGE_BYTES {
                return Err(Error::Invalid(format!("attachment {path} is larger than the {} MB limit", smtp::MAX_MESSAGE_BYTES / (1024 * 1024))));
            }
            let filename = p.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_else(|| "attachment".into());
            attachments.push(smtp::AttachmentIn {
                content_type: smtp::guess_content_type(&filename).to_string(),
                data: std::fs::read(p)?,
                filename,
            });
        }
        let built = smtp::build_message(&smtp::Compose {
            from_name: Some(account.name.clone()).filter(|n| !n.is_empty()),
            from_email: account.email.clone(),
            to: req.to,
            cc: req.cc,
            bcc: req.bcc,
            subject: req.subject.clone(),
            body: req.body,
            in_reply_to: req.in_reply_to,
            references: req.references,
            attachments,
        })?;
        self.store().outbox_insert(&NewOutbox {
            account_id: &account.id,
            message_id: &built.message_id,
            from: &account.email,
            rcpts: &built.rcpts,
            subject: &req.subject,
            raw: &built.raw,
            now: Self::now(),
        })
    }

    /// Queue a message and try to deliver it immediately. A temporary failure leaves it queued.
    pub async fn send(&self, req: SendRequest) -> Result<SendOutcome> {
        let account_key = req.account.clone();
        let item = self.outbox_enqueue(req)?;
        let account = self.store().find_account(&account_key)?;
        let password = self.password_for(&account)?;
        let item = self.deliver_item(item, &account, &password, Self::now()).await?;
        Ok(SendOutcome { delivered: item.state == "sent", item })
    }

    /// Attempt every due item once. Safe to call from several processes: items are claimed first.
    pub async fn outbox_flush(&self) -> Result<FlushReport> {
        self.outbox_flush_at(Self::now()).await
    }

    pub async fn outbox_flush_at(&self, now: i64) -> Result<FlushReport> {
        let due = {
            let store = self.store();
            store.outbox_recover_stuck(now, OUTBOX_STALE_LOCK_SECS)?;
            store.outbox_due(now, None)?
        };
        let mut report = FlushReport::default();
        for item in due {
            let account = match self.store().find_account(&item.account_id) {
                Ok(a) => a,
                Err(_) => continue,
            };
            let password = match self.password_for(&account) {
                Ok(p) => p,
                Err(e) => {
                    // Not an SMTP failure: leave a visible reason and stop retrying until fixed.
                    self.store().outbox_lock(&item.id, now)?;
                    self.store().outbox_mark_failed(&item.id, &e.to_string())?;
                    report.attempted += 1;
                    report.failed += 1;
                    report.items.push(self.store().outbox_get(&item.id)?);
                    continue;
                }
            };
            let before = item.state.clone();
            let done = self.deliver_item(item, &account, &password, now).await?;
            if before == "queued" && done.attempts > 0 {
                report.attempted += 1;
                match done.state.as_str() {
                    "sent" => report.sent += 1,
                    "queued" => report.retrying += 1,
                    "failed" => report.failed += 1,
                    _ => {}
                }
            }
            report.items.push(done);
        }
        Ok(report)
    }

    /// Claim one item, deliver it, and record the outcome.
    async fn deliver_item(&self, item: OutboxItem, account: &Account, password: &str, now: i64) -> Result<OutboxItem> {
        if !self.store().outbox_lock(&item.id, now)? {
            return self.store().outbox_get(&item.id); // another worker has it
        }
        let raw = self.store().outbox_raw(&item.id)?;
        let result = smtp::deliver(&account.smtp, password, &item.from, &item.rcpts, &raw, self.smtp_transport).await;
        {
            let store = self.store();
            match result {
                Ok(()) => store.outbox_mark_sent(&item.id)?,
                Err(Error::SmtpTransient(msg)) => {
                    let attempt = item.attempts + 1;
                    if attempt >= OUTBOX_MAX_ATTEMPTS {
                        store.outbox_mark_failed(&item.id, &format!("gave up after {attempt} attempts: {msg}"))?;
                    } else {
                        store.outbox_mark_retry(&item.id, now + outbox_backoff_secs(attempt), &msg)?;
                    }
                }
                Err(e) => store.outbox_mark_failed(&item.id, &e.to_string())?,
            }
        }
        self.store().outbox_get(&item.id)
    }

    pub fn outbox_list(&self, account: Option<&str>, include_done: bool, limit: usize) -> Result<Vec<OutboxItem>> {
        let store = self.store();
        let account_id = match account {
            Some(k) => Some(store.find_account(k)?.id),
            None => None,
        };
        store.outbox_list(account_id.as_deref(), include_done, limit)
    }

    /// Put a failed item back in the queue with a fresh attempt budget.
    pub fn outbox_retry(&self, id: &str) -> Result<OutboxItem> {
        let store = self.store();
        let item = store.outbox_get(id)?;
        if !store.outbox_requeue(&item.id, Self::now())? {
            return Err(Error::Invalid(format!("outbox item {} is {}, only failed items can be retried", &item.id[..8], item.state)));
        }
        store.outbox_get(&item.id)
    }

    pub fn outbox_cancel(&self, id: &str) -> Result<OutboxItem> {
        let store = self.store();
        let item = store.outbox_get(id)?;
        if !store.outbox_cancel(&item.id)? {
            return Err(Error::Invalid(format!("outbox item {} is {}, only queued or failed items can be cancelled", &item.id[..8], item.state)));
        }
        store.outbox_get(&item.id)
    }

    // ----- iCloud / generic CalDAV -----

    /// Discover the CalDAV home and list calendars. For iCloud the app-specific password is reused.
    pub async fn caldav_calendars(&self, key: &str) -> Result<CalDavReport> {
        let account = self.store().find_account(key)?;
        let password = self.password_for(&account)?;
        let base = match account.provider {
            Provider::Icloud => caldav::ICLOUD_CALDAV.to_string(),
            _ => std::env::var("MAILCORE_CALDAV_URL")
                .map_err(|_| Error::Invalid("set MAILCORE_CALDAV_URL for non-iCloud CalDAV accounts".into()))?,
        };
        let client = CalDavClient::new(&base, &account.email, &password)?;
        let home = client.discover().await?;
        let calendars = client.list_calendars(&home.home).await?;
        Ok(CalDavReport { account: account.name, home, calendars })
    }

    // ----- Google (Calendar-only OAuth) -----

    pub fn google_configure(&self, cfg: ClientConfig) -> Result<()> {
        if cfg.client_id.trim().is_empty() {
            return Err(Error::Invalid("client_id must not be empty".into()));
        }
        let json = serde_json::to_string(&cfg).map_err(|e| Error::Other(e.to_string()))?;
        self.secrets.set(GOOGLE_CLIENT_SECRET_KEY, &json)
    }

    /// Environment variables win over the stored client.
    pub fn google_client_config(&self) -> Result<ClientConfig> {
        if let Ok(id) = std::env::var("MAILCORE_GOOGLE_CLIENT_ID") {
            if !id.is_empty() {
                return Ok(ClientConfig { client_id: id, client_secret: std::env::var("MAILCORE_GOOGLE_CLIENT_SECRET").ok().filter(|s| !s.is_empty()) });
            }
        }
        match self.secrets.get(GOOGLE_CLIENT_SECRET_KEY)? {
            Some(json) => serde_json::from_str(&json).map_err(|e| Error::Secrets(format!("stored google client is corrupt: {e}"))),
            None => Err(Error::Invalid(
                "no Google OAuth client configured. In Google Cloud Console create a 'Desktop app' OAuth client, enable the Google Calendar API and add yourself as a test user, then run: mailcore google configure --client-id ID --client-secret SECRET".into(),
            )),
        }
    }

    /// Browser login. `present` receives the URL to show or open.
    pub async fn google_login(&self, present: impl FnOnce(&str)) -> Result<OAuthIdentity> {
        let cfg = self.google_client_config()?;
        let client = google::oauth_client(&cfg, &self.google_endpoints);
        let tokens = oauth::authorize(&client, &self.http, std::time::Duration::from_secs(300), present).await?;
        let refresh = tokens
            .refresh_token
            .clone()
            .ok_or_else(|| Error::OAuth("Google returned no refresh token; revoke the app at myaccount.google.com/permissions and retry".into()))?;
        let email = google::userinfo_email(&self.http, &self.google_endpoints, &tokens.access_token).await?;
        let identity = self.store().upsert_oauth_identity(&OAuthIdentity {
            id: uuid::Uuid::new_v4().to_string(),
            provider: GOOGLE.into(),
            email,
            scopes: client.scopes.clone(),
            client_id: cfg.client_id.clone(),
            created_at: String::new(),
        })?;
        self.secrets.set(&identity.refresh_secret_key(), &refresh)?;
        self.store_access_token(&identity, &tokens)?;
        Ok(identity)
    }

    pub fn google_identities(&self) -> Result<Vec<OAuthIdentity>> {
        self.store().list_oauth_identities(GOOGLE)
    }

    pub fn google_logout(&self, key: &str) -> Result<OAuthIdentity> {
        let id = self.store().find_oauth_identity(GOOGLE, key)?;
        self.store().delete_oauth_identity(&id.id)?;
        self.secrets.delete(&id.refresh_secret_key())?;
        self.secrets.delete(&id.access_secret_key())?;
        Ok(id)
    }

    fn store_access_token(&self, identity: &OAuthIdentity, tokens: &TokenSet) -> Result<()> {
        let cached = TokenSet { refresh_token: None, ..tokens.clone() };
        let json = serde_json::to_string(&cached).map_err(|e| Error::Other(e.to_string()))?;
        self.secrets.set(&identity.access_secret_key(), &json)
    }

    /// A valid access token, refreshed through the stored refresh token when stale.
    pub async fn google_access_token(&self, key: &str) -> Result<(OAuthIdentity, String)> {
        let identity = self.store().find_oauth_identity(GOOGLE, key)?;
        if let Some(json) = self.secrets.get(&identity.access_secret_key())? {
            if let Ok(t) = serde_json::from_str::<TokenSet>(&json) {
                if t.is_fresh(60) {
                    return Ok((identity, t.access_token));
                }
            }
        }
        let refresh = self
            .secrets
            .get(&identity.refresh_secret_key())?
            .ok_or_else(|| Error::Secrets(format!("no refresh token stored for {}; log in again", identity.email)))?;
        let cfg = self.google_client_config()?;
        let client = google::oauth_client(&cfg, &self.google_endpoints);
        let tokens = oauth::refresh(&client, &self.http, &refresh).await?;
        self.store_access_token(&identity, &tokens)?;
        Ok((identity, tokens.access_token))
    }

    pub async fn google_calendars(&self, key: &str) -> Result<Vec<CalendarSummary>> {
        let (_identity, token) = self.google_access_token(key).await?;
        google::list_calendars(&self.http, &self.google_endpoints, &token).await
    }

    /// Pull the newest `limit` headers on first sync, then only changes on later passes.
    pub async fn sync_mailbox(&self, key: &str, mailbox: &str, limit: u32) -> Result<SyncReport> {
        self.sync_mailbox_opts(key, mailbox, SyncOptions { initial_limit: limit, backfill: 0 }).await
    }

    pub async fn sync_mailbox_opts(&self, key: &str, mailbox: &str, opts: SyncOptions) -> Result<SyncReport> {
        let account = self.store().find_account(key)?;
        let password = self.password_for(&account)?;
        let client = imap::connect_tls(&account.imap.host, account.imap.port).await?;
        let session = imap::login(client, &account.imap.username, &password).await?;
        self.sync_with_session(session, &account, mailbox, opts).await
    }

    /// One sync pass on an open session, then log out. Generic so tests can use a plain socket.
    pub async fn sync_with_session<T: imap::ImapStream>(
        &self,
        session: async_imap::Session<T>,
        account: &Account,
        mailbox: &str,
        opts: SyncOptions,
    ) -> Result<SyncReport> {
        let (report, session) = self.sync_step(session, account, mailbox, opts, true).await?;
        imap::logout(session).await?;
        Ok(report)
    }

    /// One sync pass that hands the session back, so `watch` can keep using the connection.
    pub async fn sync_step<T: imap::ImapStream>(
        &self,
        mut session: async_imap::Session<T>,
        account: &Account,
        mailbox: &str,
        opts: SyncOptions,
        refresh_mailbox_list: bool,
    ) -> Result<(SyncReport, async_imap::Session<T>)> {
        let need_list = refresh_mailbox_list || self.store().find_mailbox(&account.id, mailbox).is_err();
        let mut seen = 0;
        if need_list {
            let infos = imap::list_mailboxes(&mut session).await?;
            seen = infos.len();
            let store = self.store();
            for info in &infos {
                store.upsert_mailbox(&account.id, &info.name, info.delimiter.as_deref(), &info.attributes.join(" "))?;
            }
        }
        let row = self.store().find_mailbox(&account.id, mailbox)?;
        let local = {
            let store = self.store();
            LocalView { uidvalidity: row.uidvalidity, uid_flags: store.local_uid_flags(row.id)? }
        };
        let g = sync::gather(&mut session, &row.name, local, opts).await?;
        if g.reset {
            tracing::warn!(mailbox = %row.name, "UIDVALIDITY changed; dropping cached messages");
        }
        let (counts, cached_total) = {
            let mut store = self.store();
            let counts = store.apply_sync(
                &account.id,
                row.id,
                g.reset,
                &g.new,
                &g.flag_updates,
                &g.expunged,
                MailboxStateUpdate { uidvalidity: g.state.uidvalidity, uidnext: g.state.uidnext, exists: Some(g.state.exists) },
            )?;
            (counts, store.count_messages(row.id)?)
        };
        let report = SyncReport {
            account: account.name.clone(),
            mailbox: row.name,
            exists: g.state.exists,
            uidvalidity: g.state.uidvalidity,
            uidvalidity_changed: g.reset,
            fetched: g.new.len(),
            stored: counts.inserted,
            backfilled: g.backfilled,
            flags_updated: counts.flags_updated,
            expunged: g.expunged.len(),
            cached_total,
            mailboxes_seen: seen,
        };
        Ok((report, session))
    }

    /// Keep a mailbox in sync until `max_cycles` passes (None = forever): sync, IDLE, re-sync on
    /// every push. Reconnects with backoff on network errors; a rejected login is fatal.
    pub async fn watch(
        &self,
        key: &str,
        mailbox: &str,
        opts: SyncOptions,
        idle_timeout: std::time::Duration,
        mut on_report: impl FnMut(&SyncReport),
        max_cycles: Option<u32>,
    ) -> Result<()> {
        let account = self.store().find_account(key)?;
        let password = self.password_for(&account)?;
        let mut cycles = 0u32;
        let mut backoff = std::time::Duration::from_secs(1);
        loop {
            let client = match imap::connect_tls(&account.imap.host, account.imap.port).await {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!("connect failed ({e}); retrying in {backoff:?}");
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(std::time::Duration::from_secs(60));
                    continue;
                }
            };
            let session = imap::login(client, &account.imap.username, &password).await?;
            backoff = std::time::Duration::from_secs(1);
            match self.watch_session(session, &account, mailbox, opts, idle_timeout, &mut on_report, &mut cycles, max_cycles).await {
                Ok(()) => return Ok(()),
                Err(e @ (Error::Imap(_) | Error::Io(_) | Error::Tls(_))) => {
                    tracing::warn!("connection lost ({e}); reconnecting in {backoff:?}");
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(std::time::Duration::from_secs(60));
                }
                Err(e) => return Err(e),
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn watch_session<T: imap::ImapStream>(
        &self,
        mut session: async_imap::Session<T>,
        account: &Account,
        mailbox: &str,
        mut opts: SyncOptions,
        idle_timeout: std::time::Duration,
        on_report: &mut impl FnMut(&SyncReport),
        cycles: &mut u32,
        max_cycles: Option<u32>,
    ) -> Result<()> {
        let mut first = true;
        loop {
            let (report, s) = self.sync_step(session, account, mailbox, opts, first).await?;
            session = s;
            first = false;
            opts.backfill = 0; // backfill only on the first pass
            on_report(&report);
            *cycles += 1;
            if max_cycles.is_some_and(|m| *cycles >= m) {
                imap::logout(session).await?;
                return Ok(());
            }
            // Re-issue IDLE on timeout without re-syncing; only a push triggers a pass.
            loop {
                let (s, outcome) = imap::idle_wait(session, idle_timeout).await?;
                session = s;
                if outcome == imap::IdleOutcome::Changed {
                    break;
                }
            }
        }
    }

    pub fn list_mailboxes(&self, key: &str) -> Result<Vec<MailboxRow>> {
        let store = self.store();
        let account = store.find_account(key)?;
        store.list_mailboxes(&account.id)
    }

    pub fn list_messages(&self, key: &str, mailbox: &str, limit: usize) -> Result<Vec<MessageRow>> {
        let store = self.store();
        let account = store.find_account(key)?;
        let mb = store.find_mailbox(&account.id, mailbox)?;
        store.list_messages(mb.id, limit)
    }

    /// Headers plus parsed body. The body is downloaded once and cached; later calls are local.
    pub async fn fetch_message(&self, key: &str, mailbox: &str, uid: u32) -> Result<MessageDetail> {
        let account = self.store().find_account(key)?;
        if let Some(d) = self.cached_detail(&account, mailbox, uid)? {
            return Ok(d);
        }
        let password = self.password_for(&account)?;
        let client = imap::connect_tls(&account.imap.host, account.imap.port).await?;
        let session = imap::login(client, &account.imap.username, &password).await?;
        self.fetch_message_with_session(session, &account, mailbox, uid).await
    }

    fn cached_detail(&self, account: &Account, mailbox: &str, uid: u32) -> Result<Option<MessageDetail>> {
        let store = self.store();
        let mb = store.find_mailbox(&account.id, mailbox)?;
        let row = store.find_message(mb.id, uid)?;
        if row.body_cached { Ok(Some(store.message_detail(row)?)) } else { Ok(None) }
    }

    pub async fn fetch_message_with_session<T: imap::ImapStream>(
        &self,
        mut session: async_imap::Session<T>,
        account: &Account,
        mailbox: &str,
        uid: u32,
    ) -> Result<MessageDetail> {
        let mb = self.store().find_mailbox(&account.id, mailbox)?;
        imap::select_state(&mut session, &mb.name).await?;
        let raw = imap::fetch_raw(&mut session, uid).await?;
        imap::logout(session).await?;
        let raw = raw.ok_or_else(|| Error::Invalid(format!("the server no longer has uid {uid} in {}", mb.name)))?;
        let parsed = crate::body::parse_body(&raw)?;
        let mut store = self.store();
        let row = store.find_message(mb.id, uid)?;
        store.set_message_body(row.id, &parsed)?;
        let row = store.find_message(mb.id, uid)?;
        store.message_detail(row)
    }

    /// Download one attachment (by the index `show` prints) into `dir`, never overwriting a file.
    pub async fn save_attachment(&self, key: &str, mailbox: &str, uid: u32, part_index: i64, dir: &std::path::Path) -> Result<SavedAttachment> {
        let account = self.store().find_account(key)?;
        let password = self.password_for(&account)?;
        let client = imap::connect_tls(&account.imap.host, account.imap.port).await?;
        let session = imap::login(client, &account.imap.username, &password).await?;
        self.save_attachment_with_session(session, &account, mailbox, uid, part_index, dir).await
    }

    pub async fn save_attachment_with_session<T: imap::ImapStream>(
        &self,
        mut session: async_imap::Session<T>,
        account: &Account,
        mailbox: &str,
        uid: u32,
        part_index: i64,
        dir: &std::path::Path,
    ) -> Result<SavedAttachment> {
        let mb = self.store().find_mailbox(&account.id, mailbox)?;
        imap::select_state(&mut session, &mb.name).await?;
        let raw = imap::fetch_raw(&mut session, uid).await?;
        imap::logout(session).await?;
        let raw = raw.ok_or_else(|| Error::Invalid(format!("the server no longer has uid {uid} in {}", mb.name)))?;
        let (info, bytes) = crate::body::extract_attachment(&raw, part_index)?
            .ok_or_else(|| Error::Invalid(format!("message {uid} has no attachment number {part_index}")))?;
        std::fs::create_dir_all(dir)?;
        let name = crate::body::sanitize_filename(info.filename.as_deref(), part_index);
        let (path, mut file) = create_unique(dir, &name)?;
        std::io::Write::write_all(&mut file, &bytes)?;
        Ok(SavedAttachment { path: path.display().to_string(), filename: name, content_type: info.content_type, bytes: bytes.len() })
    }

    /// Sync every worthwhile mailbox of an account over one connection.
    pub async fn sync_all(&self, key: &str, opts: SyncOptions) -> Result<SyncAllReport> {
        let account = self.store().find_account(key)?;
        let password = self.password_for(&account)?;
        let client = imap::connect_tls(&account.imap.host, account.imap.port).await?;
        let session = imap::login(client, &account.imap.username, &password).await?;
        self.sync_all_with_session(session, &account, opts).await
    }

    pub async fn sync_all_with_session<T: imap::ImapStream>(
        &self,
        mut session: async_imap::Session<T>,
        account: &Account,
        opts: SyncOptions,
    ) -> Result<SyncAllReport> {
        let infos = imap::list_mailboxes(&mut session).await?;
        {
            let store = self.store();
            for info in &infos {
                store.upsert_mailbox(&account.id, &info.name, info.delimiter.as_deref(), &info.attributes.join(" "))?;
            }
        }
        let mut out = SyncAllReport::default();
        for info in &infos {
            if !syncable_mailbox(&info.name, &info.attributes.join(" ")) {
                out.skipped.push(info.name.clone());
                continue;
            }
            match self.sync_step(session, account, &info.name, opts, false).await {
                Ok((mut report, s)) => {
                    report.mailboxes_seen = infos.len();
                    out.reports.push(report);
                    session = s;
                }
                // The session is consumed by a failed step, so a protocol error ends the run.
                Err(e) => {
                    out.errors.push((info.name.clone(), e.to_string()));
                    return Ok(out);
                }
            }
        }
        imap::logout(session).await?;
        Ok(out)
    }

    /// Search cached subjects, addresses and downloaded bodies. `account` limits to one account.
    pub fn search(&self, query: &str, account: Option<&str>, limit: usize) -> Result<Vec<SearchHit>> {
        let store = self.store();
        let account_id = match account {
            Some(k) => Some(store.find_account(k)?.id),
            None => None,
        };
        store.search(query, account_id.as_deref(), limit)
    }

    pub fn thread(&self, key: &str, thread_id: &str) -> Result<Vec<MessageRow>> {
        let store = self.store();
        let account = store.find_account(key)?;
        store.thread_messages(&account.id, thread_id)
    }
}

/// Create `dir/name`, or `dir/stem (1).ext`, `(2)`, ... if it exists. Never overwrites.
fn create_unique(dir: &std::path::Path, name: &str) -> Result<(PathBuf, std::fs::File)> {
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    for n in 0..1000 {
        let candidate = if n == 0 { name.to_string() } else { format!("{stem} ({n}){ext}") };
        let path = dir.join(&candidate);
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(f) => return Ok((path, f)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.into()),
        }
    }
    Err(Error::Other(format!("too many files named {name} in {}", dir.display())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::EncryptedFileStore;

    fn test_api() -> (Api, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("mailcore-api-{}", uuid::Uuid::new_v4()));
        let secrets = EncryptedFileStore::open_with_passphrase(&dir.join("secrets.enc.json"), "pw").unwrap();
        (Api::new(Store::open_in_memory().unwrap(), Box::new(secrets), dir.clone()), dir)
    }

    // ---- Phase 2 sync scenarios against the stateful fake IMAP server ----

    use crate::imap::fake_server::{FakeState, Shared};

    const MSG_HEAD: &str = "From: Sam <sam@example.com>\r\nTo: fred@example.com\r\nDate: Wed, 28 Oct 2026 10:00:00 +0000\r\n";

    fn headers(mid: &str, subject: &str, extra: &str) -> String {
        format!("{MSG_HEAD}Subject: {subject}\r\nMessage-ID: {mid}\r\n{extra}\r\n")
    }

    async fn fake_account(api: &Api, state: Shared) -> (Account, u16) {
        let port = crate::imap::fake_server::start_with(state).await;
        let acct = api
            .add_account(AddAccountRequest {
                name: "Fake".into(), replace: false, email: "fred@example.com".into(), provider: Provider::Custom,
                password: "good-password".into(), imap_host: Some("127.0.0.1".into()), imap_port: Some(port), smtp_host: None, smtp_port: None,
            })
            .unwrap();
        (acct, port)
    }

    async fn plain_session(port: u16, acct: &Account) -> async_imap::Session<tokio::net::TcpStream> {
        let c = crate::imap::connect_plain("127.0.0.1", port).await.unwrap();
        crate::imap::login(c, &acct.imap.username, "good-password").await.unwrap()
    }

    #[tokio::test]
    async fn incremental_sync_picks_up_new_flags_and_expunges() {
        let (api, dir) = test_api();
        let state: Shared = std::sync::Arc::new(std::sync::Mutex::new(FakeState::standard()));
        let (acct, port) = fake_account(&api, state.clone()).await;

        let r1 = api.sync_with_session(plain_session(port, &acct).await, &acct, "INBOX", SyncOptions::default()).await.unwrap();
        assert_eq!((r1.fetched, r1.stored, r1.cached_total, r1.exists), (2, 2, 2, 2));

        // Server side: a reply arrives, 1002 becomes read, 1001 is deleted.
        {
            let mut st = state.lock().unwrap();
            st.add(&headers("<m3@example.com>", "Re: Invitation: Dentist", "In-Reply-To: <m2@example.org>\r\nReferences: <m1@example.com> <m2@example.org>\r\n"), "Thursday works.\r\n", "");
            st.msgs.iter_mut().find(|m| m.uid == 1002).unwrap().flags = "\\Seen".into();
            st.msgs.retain(|m| m.uid != 1001);
            st.log.clear();
        }
        let r2 = api.sync_with_session(plain_session(port, &acct).await, &acct, "INBOX", SyncOptions::default()).await.unwrap();
        assert_eq!((r2.fetched, r2.stored, r2.flags_updated, r2.expunged, r2.cached_total), (1, 1, 1, 1, 2), "{r2:?}");

        // It asked only for what it needed: new mail from max+1, never a full re-download.
        let log = state.lock().unwrap().log.clone();
        assert!(log.iter().any(|l| l.contains("UID FETCH 1003:*")), "{log:?}");
        assert!(!log.iter().any(|l| l.contains("FETCH 1:") || l.contains("BODY.PEEK[]")), "{log:?}");

        let msgs = api.list_messages("Fake", "INBOX", 10).unwrap();
        assert_eq!(msgs.iter().map(|m| m.header.uid).collect::<Vec<_>>(), vec![1003, 1002]);
        assert_eq!(msgs[1].header.flags, "\\Seen");
        // 1002 (cached) and the reply 1003 share a thread via References, though 1001 is gone.
        assert_eq!(msgs[0].thread_id, msgs[1].thread_id);

        // A pass with nothing to do changes nothing.
        let r3 = api.sync_with_session(plain_session(port, &acct).await, &acct, "INBOX", SyncOptions::default()).await.unwrap();
        assert_eq!((r3.fetched, r3.stored, r3.flags_updated, r3.expunged), (0, 0, 0, 0));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn uidvalidity_change_discards_the_cache() {
        let (api, dir) = test_api();
        let state: Shared = std::sync::Arc::new(std::sync::Mutex::new(FakeState::standard()));
        let (acct, port) = fake_account(&api, state.clone()).await;
        api.sync_with_session(plain_session(port, &acct).await, &acct, "INBOX", SyncOptions::default()).await.unwrap();
        {
            let mut st = state.lock().unwrap();
            st.uidvalidity = 99;
            st.msgs.clear();
            st.uidnext = 1;
            st.add(&headers("<fresh@example.com>", "Brand new mailbox", ""), "x\r\n", "");
        }
        let r = api.sync_with_session(plain_session(port, &acct).await, &acct, "INBOX", SyncOptions::default()).await.unwrap();
        assert!(r.uidvalidity_changed);
        assert_eq!((r.stored, r.cached_total), (1, 1));
        let msgs = api.list_messages("Fake", "INBOX", 10).unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].header.subject.as_deref(), Some("Brand new mailbox"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn emptied_mailbox_expunges_everything_cached() {
        let (api, dir) = test_api();
        let state: Shared = std::sync::Arc::new(std::sync::Mutex::new(FakeState::standard()));
        let (acct, port) = fake_account(&api, state.clone()).await;
        api.sync_with_session(plain_session(port, &acct).await, &acct, "INBOX", SyncOptions::default()).await.unwrap();
        state.lock().unwrap().msgs.clear();
        let r = api.sync_with_session(plain_session(port, &acct).await, &acct, "INBOX", SyncOptions::default()).await.unwrap();
        assert_eq!((r.expunged, r.cached_total, r.exists), (2, 0, 0));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn backfill_walks_older_mail_in_batches() {
        let (api, dir) = test_api();
        let mut st = FakeState::standard();
        st.msgs.clear();
        st.uidnext = 100;
        for i in 0..30 {
            st.add(&headers(&format!("<b{i}@example.com>"), &format!("Message {i}"), ""), "body\r\n", "\\Seen");
        }
        let state: Shared = std::sync::Arc::new(std::sync::Mutex::new(st));
        let (acct, port) = fake_account(&api, state.clone()).await;

        let r1 = api.sync_with_session(plain_session(port, &acct).await, &acct, "INBOX", SyncOptions { initial_limit: 10, backfill: 0 }).await.unwrap();
        assert_eq!(r1.cached_total, 10);
        let r2 = api.sync_with_session(plain_session(port, &acct).await, &acct, "INBOX", SyncOptions { initial_limit: 10, backfill: 12 }).await.unwrap();
        assert_eq!((r2.backfilled, r2.cached_total), (12, 22));
        let r3 = api.sync_with_session(plain_session(port, &acct).await, &acct, "INBOX", SyncOptions { initial_limit: 10, backfill: 50 }).await.unwrap();
        assert_eq!((r3.backfilled, r3.cached_total), (8, 30), "stops at the start of the mailbox");
        let r4 = api.sync_with_session(plain_session(port, &acct).await, &acct, "INBOX", SyncOptions { initial_limit: 10, backfill: 50 }).await.unwrap();
        assert_eq!((r4.backfilled, r4.cached_total), (0, 30));
        let uids: Vec<u32> = api.list_messages("Fake", "INBOX", 100).unwrap().iter().map(|m| m.header.uid).collect();
        assert_eq!(uids.len(), 30);
        assert_eq!((uids.iter().min(), uids.iter().max()), (Some(&100), Some(&129)));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn body_is_downloaded_once_cached_and_becomes_searchable() {
        let (api, dir) = test_api();
        let state: Shared = std::sync::Arc::new(std::sync::Mutex::new(FakeState::standard()));
        let (acct, port) = fake_account(&api, state.clone()).await;
        api.sync_with_session(plain_session(port, &acct).await, &acct, "INBOX", SyncOptions::default()).await.unwrap();

        // Header-only: subject matches, body words do not.
        assert_eq!(api.search("invitation", Some("Fake"), 10).unwrap().len(), 1);
        assert_eq!(api.search("thursday", Some("Fake"), 10).unwrap().len(), 0);

        let d = api.fetch_message_with_session(plain_session(port, &acct).await, &acct, "INBOX", 1002).await.unwrap();
        assert!(d.body_text.as_deref().unwrap().contains("dentist appointment is Thursday"));
        assert_eq!(d.mailbox, "INBOX");
        assert!(d.row.body_cached);
        assert_eq!(d.row.preview.as_deref(), Some("Your dentist appointment is Thursday."));

        let hits = api.search("thursday", Some("Fake"), 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].row.header.uid, 1002);

        // The public fetch_message serves from cache without touching the network (host is fake).
        state.lock().unwrap().log.clear();
        let again = api.fetch_message("Fake", "INBOX", 1002).await.unwrap();
        assert_eq!(again.body_text, d.body_text);
        assert!(state.lock().unwrap().log.is_empty(), "no server traffic for a cached body");

        // Asking for a uid we never synced is a clear error, not a server round trip.
        assert!(matches!(api.fetch_message("Fake", "INBOX", 5555).await, Err(Error::Invalid(_))));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn watch_resyncs_when_the_server_pushes() {
        let (api, dir) = test_api();
        let state: Shared = std::sync::Arc::new(std::sync::Mutex::new(FakeState::standard()));
        state.lock().unwrap().push_on_idle = true;
        let (acct, port) = fake_account(&api, state.clone()).await;

        let mut reports: Vec<SyncReport> = Vec::new();
        let mut cycles = 0;
        api.watch_session(
            plain_session(port, &acct).await,
            &acct,
            "INBOX",
            SyncOptions::default(),
            std::time::Duration::from_secs(5),
            &mut |r: &SyncReport| {
                // New mail lands only after the first pass has finished, so the second pass can
                // only learn about it through the IDLE push.
                if reports.is_empty() {
                    state.lock().unwrap().add(&headers("<live@example.com>", "Arrived while idling", ""), "hi\r\n", "");
                }
                reports.push(r.clone());
            },
            &mut cycles,
            Some(2),
        )
        .await
        .unwrap();
        assert_eq!(reports.len(), 2);
        assert_eq!((reports[0].stored, reports[0].cached_total), (2, 2));
        assert_eq!((reports[1].stored, reports[1].cached_total), (1, 3), "{:?}", reports[1]);
        let _ = std::fs::remove_dir_all(dir);
    }

    // ---- Phase 2 outbox, attachments and sync-all ----

    use crate::smtp::fake_smtp::{Behavior, Shared as SmtpShared, State as SmtpState};

    async fn smtp_account(api: &Api, password: &str) -> (Account, SmtpShared) {
        let state: SmtpShared = std::sync::Arc::new(std::sync::Mutex::new(SmtpState::default()));
        let port = crate::smtp::fake_smtp::start(state.clone()).await;
        let acct = api
            .add_account(AddAccountRequest {
                name: "Mailer".into(), replace: false, email: "fred@example.com".into(), provider: Provider::Custom,
                password: password.into(), imap_host: Some("127.0.0.1".into()), imap_port: None,
                smtp_host: Some("127.0.0.1".into()), smtp_port: Some(port),
            })
            .unwrap();
        (acct, state)
    }

    fn req(subject: &str) -> SendRequest {
        SendRequest {
            account: "Mailer".into(),
            to: vec!["jill@example.com".into()],
            bcc: vec!["audit@example.com".into()],
            subject: subject.into(),
            body: "See you at six.".into(),
            ..Default::default()
        }
    }

    #[test]
    fn backoff_grows_then_plateaus() {
        let v: Vec<i64> = (1..=9).map(outbox_backoff_secs).collect();
        assert_eq!(v, vec![60, 300, 900, 3600, 21600, 43200, 86400, 86400, 86400]);
    }

    #[test]
    fn folder_filter_matches_real_server_attributes() {
        let cases = [
            ("INBOX", "NoInferiors", true),
            ("Archive", "", true),
            ("Notes", "", true),
            ("INBOX/Long term save", "", true),
            ("Orders", "Extension(\"\\HasNoChildren\")", true),
            ("Cabinet", "", true),
            ("[Gmail]/Sent Mail", "Extension(\"\\HasNoChildren\") Sent", true),
            ("[Gmail]/Drafts", "Drafts Extension(\"\\HasNoChildren\")", true),
            ("Sent Messages", "Sent", true),
            ("[Gmail]", "Extension(\"\\HasChildren\") NoSelect", false),
            ("[Gmail]/All Mail", "All Extension(\"\\HasNoChildren\")", false),
            ("[Gmail]/Spam", "Extension(\"\\HasNoChildren\") Junk", false),
            ("[Gmail]/Trash", "Extension(\"\\HasNoChildren\") Trash", false),
            ("[Gmail]/Starred", "Flagged Extension(\"\\HasNoChildren\")", false),
            ("[Gmail]/Important", "Extension(\"\\HasNoChildren\") Extension(\"\\Important\")", false),
            ("Deleted Messages", "Trash", false),
            ("Junk", "", false),
            ("Junk E-mail", "", false),
            ("Deleted Items", "", false),
        ];
        for (name, attrs, want) in cases {
            assert_eq!(syncable_mailbox(name, attrs), want, "{name} [{attrs}]");
        }
    }

    #[tokio::test]
    async fn send_delivers_once_with_bcc_in_the_envelope_only() {
        let (api, dir) = test_api();
        let api = api.with_plain_smtp();
        let (_acct, smtp) = smtp_account(&api, "good-password").await;
        let out = api.send(req("Dinner")).await.unwrap();
        assert!(out.delivered);
        assert_eq!((out.item.state.as_str(), out.item.attempts), ("sent", 1));
        let st = smtp.lock().unwrap();
        assert_eq!(st.received.len(), 1);
        assert_eq!(st.received[0].rcpts, vec!["jill@example.com", "audit@example.com"]);
        assert!(st.received[0].data.contains("Subject: Dinner"));
        assert!(!st.received[0].data.contains("audit@example.com"), "Bcc must not leak into the message");
        drop(st);
        // Delivered mail leaves the default outbox view, and the stored bytes are gone.
        assert!(api.outbox_list(Some("Mailer"), false, 10).unwrap().is_empty());
        assert_eq!(api.outbox_list(Some("Mailer"), true, 10).unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn temporary_failure_is_retried_on_schedule_and_only_when_due() {
        let (api, dir) = test_api();
        let api = api.with_plain_smtp();
        let (_acct, smtp) = smtp_account(&api, "good-password").await;
        smtp.lock().unwrap().script.push_back(Behavior::TempFailData);

        let out = api.send(req("Retry me")).await.unwrap();
        assert!(!out.delivered);
        let item = out.item;
        assert_eq!((item.state.as_str(), item.attempts), ("queued", 1));
        assert!(item.last_error.as_deref().unwrap().contains("451"), "{:?}", item.last_error);
        let due = item.next_attempt_at;

        // Too early: nothing is attempted and no connection is made.
        let before = smtp.lock().unwrap().connections;
        let r = api.outbox_flush_at(due - 1).await.unwrap();
        assert_eq!(r.attempted, 0);
        assert_eq!(smtp.lock().unwrap().connections, before);

        // Due: it goes out, exactly once.
        let r = api.outbox_flush_at(due).await.unwrap();
        assert_eq!((r.attempted, r.sent), (1, 1));
        assert_eq!(smtp.lock().unwrap().received.len(), 1);
        let r = api.outbox_flush_at(due + 10_000).await.unwrap();
        assert_eq!(r.attempted, 0, "a sent item is never sent again");
        assert_eq!(smtp.lock().unwrap().received.len(), 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn permanent_rejection_stops_retrying_until_the_user_retries() {
        let (api, dir) = test_api();
        let api = api.with_plain_smtp();
        let (_acct, smtp) = smtp_account(&api, "good-password").await;
        smtp.lock().unwrap().script.push_back(Behavior::RejectRcpt);
        let out = api.send(req("Bounce")).await.unwrap();
        assert_eq!(out.item.state, "failed");
        assert!(out.item.last_error.as_deref().unwrap().contains("550"), "{:?}", out.item.last_error);

        let conns = smtp.lock().unwrap().connections;
        let r = api.outbox_flush_at(i64::MAX / 2).await.unwrap();
        assert_eq!(r.attempted, 0);
        assert_eq!(smtp.lock().unwrap().connections, conns, "failed items are left alone");

        let queued = api.outbox_retry(&out.item.id[..8]).unwrap();
        assert_eq!((queued.state.as_str(), queued.attempts), ("queued", 0));
        let r = api.outbox_flush().await.unwrap();
        assert_eq!(r.sent, 1);
        assert!(api.outbox_retry(&out.item.id).is_err(), "a sent item cannot be retried");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn rejected_credentials_fail_immediately_without_retry_storms() {
        let (api, dir) = test_api();
        let api = api.with_plain_smtp();
        let (_acct, smtp) = smtp_account(&api, "wrong-password").await;
        let out = api.send(req("Never arrives")).await.unwrap();
        assert_eq!((out.item.state.as_str(), out.item.attempts), ("failed", 1));
        assert!(smtp.lock().unwrap().received.is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn gives_up_after_the_attempt_budget() {
        let (api, dir) = test_api();
        let api = api.with_plain_smtp();
        let (_acct, smtp) = smtp_account(&api, "good-password").await;
        smtp.lock().unwrap().script.extend(std::iter::repeat(Behavior::Drop).take(OUTBOX_MAX_ATTEMPTS as usize));
        let mut item = api.send(req("Doomed")).await.unwrap().item;
        assert_eq!((item.state.as_str(), item.attempts), ("queued", 1));
        let mut last_due = item.next_attempt_at;
        for _ in 2..=OUTBOX_MAX_ATTEMPTS {
            let r = api.outbox_flush_at(last_due).await.unwrap();
            item = r.items.into_iter().next().unwrap();
            last_due = item.next_attempt_at;
        }
        assert_eq!((item.state.as_str(), item.attempts), ("failed", OUTBOX_MAX_ATTEMPTS));
        assert!(item.last_error.as_deref().unwrap().contains("gave up after 8 attempts"), "{:?}", item.last_error);
        assert!(smtp.lock().unwrap().received.is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn concurrent_flushers_never_double_send() {
        let (api, dir) = test_api();
        let api = api.with_plain_smtp();
        let (_acct, smtp) = smtp_account(&api, "good-password").await;
        smtp.lock().unwrap().script.push_back(Behavior::TempFailData);
        let item = api.send(req("Once only")).await.unwrap().item;
        let due = item.next_attempt_at;
        let (a, b, c) = tokio::join!(api.outbox_flush_at(due), api.outbox_flush_at(due), api.outbox_flush_at(due));
        let sent = a.unwrap().sent + b.unwrap().sent + c.unwrap().sent;
        assert_eq!(sent, 1);
        assert_eq!(smtp.lock().unwrap().received.len(), 1, "the claim step stops duplicates");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn attachments_are_embedded_at_queue_time_and_bad_paths_queue_nothing() {
        let (api, dir) = test_api();
        let api = api.with_plain_smtp();
        let (_acct, smtp) = smtp_account(&api, "good-password").await;
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("plan.pdf");
        std::fs::write(&file, b"%PDF-1.4 hello").unwrap();

        let mut r = req("With file");
        r.attachments = vec![file.display().to_string()];
        let item = api.outbox_enqueue(r).unwrap();
        // Editing or deleting the file after queueing does not change what is sent.
        std::fs::remove_file(&file).unwrap();
        let rep = api.outbox_flush().await.unwrap();
        assert_eq!(rep.sent, 1);
        let data = smtp.lock().unwrap().received[0].data.clone();
        assert!(data.contains("plan.pdf") && data.contains("application/pdf"), "{data}");
        assert_eq!(item.state, "queued");

        let mut bad = req("Ghost");
        bad.attachments = vec![dir.join("missing.bin").display().to_string()];
        assert!(matches!(api.outbox_enqueue(bad), Err(Error::Invalid(_))));
        let mut dirpath = req("Dir");
        dirpath.attachments = vec![dir.display().to_string()];
        assert!(matches!(api.outbox_enqueue(dirpath), Err(Error::Invalid(_))));
        assert_eq!(api.outbox_list(Some("Mailer"), true, 10).unwrap().len(), 1, "failed enqueues leave no trace");

        // Cancel works on queued items and not on delivered ones.
        let q = api.outbox_enqueue(req("Changed my mind")).unwrap();
        assert_eq!(api.outbox_cancel(&q.id).unwrap().state, "cancelled");
        assert!(api.outbox_cancel(&item.id).is_err());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn saves_attachments_safely_and_never_overwrites() {
        let (api, dir) = test_api();
        let state: Shared = std::sync::Arc::new(std::sync::Mutex::new(FakeState::standard()));
        let raw_head = "From: a@example.com\r\nTo: fred@example.com\r\nSubject: Files\r\nMessage-ID: <att@example.com>\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=\"B\"\r\n\r\n";
        let multipart = "--B\r\nContent-Type: text/plain\r\n\r\nsee attached\r\n--B\r\nContent-Type: application/octet-stream\r\nContent-Disposition: attachment; filename=\"../../evil.sh\"\r\nContent-Transfer-Encoding: base64\r\n\r\nSGVsbG8gUERG\r\n--B--\r\n";
        let uid = state.lock().unwrap().add(raw_head, multipart, "");
        let (acct, port) = fake_account(&api, state).await;
        api.sync_with_session(plain_session(port, &acct).await, &acct, "INBOX", SyncOptions::default()).await.unwrap();

        let out = dir.join("downloads");
        let a = api.save_attachment_with_session(plain_session(port, &acct).await, &acct, "INBOX", uid, 0, &out).await.unwrap();
        assert_eq!(a.filename, "evil.sh", "path components are stripped");
        assert_eq!(std::path::Path::new(&a.path).parent().unwrap(), out.as_path());
        assert_eq!(std::fs::read(&a.path).unwrap(), b"Hello PDF");
        let b = api.save_attachment_with_session(plain_session(port, &acct).await, &acct, "INBOX", uid, 0, &out).await.unwrap();
        assert!(b.path.ends_with("evil (1).sh"), "{}", b.path);
        assert_eq!(std::fs::read_dir(&out).unwrap().count(), 2);
        assert!(!dir.join("evil.sh").exists());
        let err = api.save_attachment_with_session(plain_session(port, &acct).await, &acct, "INBOX", uid, 5, &out).await;
        assert!(matches!(err, Err(Error::Invalid(_))));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn sync_all_covers_normal_folders_and_skips_the_rest() {
        let (api, dir) = test_api();
        let mut st = FakeState::standard();
        st.extra_list = vec![
            "* LIST (\\HasNoChildren \\Trash) \"/\" \"Trash\"".into(),
            "* LIST (\\HasChildren \\Noselect) \"/\" \"[Gmail]\"".into(),
            "* LIST (\\HasNoChildren \\Junk) \"/\" \"Spam\"".into(),
            "* LIST (\\HasNoChildren) \"/\" \"Orders\"".into(),
        ];
        let state: Shared = std::sync::Arc::new(std::sync::Mutex::new(st));
        let (acct, port) = fake_account(&api, state).await;
        let rep = api.sync_all_with_session(plain_session(port, &acct).await, &acct, SyncOptions::default()).await.unwrap();
        let mut synced: Vec<&str> = rep.reports.iter().map(|r| r.mailbox.as_str()).collect();
        synced.sort();
        assert_eq!(synced, vec!["INBOX", "Orders", "Sent Messages"]);
        let mut skipped = rep.skipped.clone();
        skipped.sort();
        assert_eq!(skipped, vec!["Spam".to_string(), "Trash".to_string(), "[Gmail]".to_string()]);
        assert!(rep.errors.is_empty(), "{:?}", rep.errors);
        assert!(rep.reports.iter().all(|r| r.cached_total == 2));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn google_login_refresh_and_calendars_against_fake_provider() {
        use crate::oauth::fake_http;
        let (api, dir) = test_api();
        assert!(matches!(api.google_client_config(), Err(Error::Invalid(_))), "unconfigured client is a clear error");
        api.google_configure(ClientConfig { client_id: "cid".into(), client_secret: Some("sec".into()) }).unwrap();

        // Token endpoint serves the code exchange and one later refresh.
        let (tp, _) = fake_http::start(200, r#"{"access_token":"AT1","refresh_token":"RT","expires_in":1,"token_type":"Bearer"}"#, 2).await;
        let (up, _) = fake_http::start(200, r#"{"email":"fred@gmail.com"}"#, 1).await;
        let (cp, _) = fake_http::start(200, r#"{"items":[{"id":"fred@gmail.com","summary":"Fred","primary":true,"accessRole":"owner"}]}"#, 1).await;
        let api = api.with_google_endpoints(GoogleEndpoints {
            auth_url: "https://auth.example/o".into(),
            token_url: format!("http://127.0.0.1:{tp}/token"),
            userinfo_url: format!("http://127.0.0.1:{up}/userinfo"),
            calendar_base: format!("http://127.0.0.1:{cp}/v3"),
        });

        let identity = api
            .google_login(|url| {
                let u = url::Url::parse(url).unwrap();
                let q: std::collections::HashMap<_, _> = u.query_pairs().into_owned().collect();
                let redirect = q["redirect_uri"].clone();
                let state = q["state"].clone();
                tokio::spawn(async move {
                    crate::http_client().get(format!("{redirect}?code=C&state={state}")).send().await.unwrap();
                });
            })
            .await
            .unwrap();
        assert_eq!(identity.email, "fred@gmail.com");
        assert_eq!(api.google_identities().unwrap().len(), 1);

        // expires_in=1 leaves the cached token stale, so this exercises the refresh path.
        tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
        let cals = api.google_calendars("fred@gmail.com").await.unwrap();
        assert_eq!(cals[0].summary, "Fred");

        let gone = api.google_logout("fred@gmail.com").unwrap();
        assert_eq!(gone.id, identity.id);
        assert!(api.google_identities().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn end_to_end_sync_into_sqlite() {
        let (api, dir) = test_api();
        let acct = api
            .add_account(AddAccountRequest {
                name: "Fake".into(),
                replace: false,
                email: "fred@example.com".into(),
                provider: Provider::Custom,
                password: "good-password".into(),
                imap_host: Some("127.0.0.1".into()),
                imap_port: None,
                smtp_host: Some("127.0.0.1".into()),
                smtp_port: None,
            })
            .unwrap();
        assert_eq!(api.ping().unwrap().accounts, 1);

        let port = crate::imap::fake_server::start().await;
        let client = crate::imap::connect_plain("127.0.0.1", port).await.unwrap();
        let session = crate::imap::login(client, &acct.imap.username, "good-password").await.unwrap();
        let report = api.sync_with_session(session, &acct, "inbox", SyncOptions::default()).await.unwrap();
        assert_eq!(report.mailbox, "INBOX");
        assert_eq!(report.fetched, 2);
        assert_eq!(report.stored, 2);
        assert_eq!(report.mailboxes_seen, 2);
        assert!(!report.uidvalidity_changed);

        let msgs = api.list_messages("fake", "INBOX", 10).unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].header.subject.as_deref(), Some("Invitation: Dentist")); // newest first
        assert_eq!(msgs[1].header.subject.as_deref(), Some("Café meeting"));

        // Re-adding the same name without replace is refused; with replace it swaps cleanly.
        let dup = AddAccountRequest {
            name: "FAKE".into(), replace: false, email: "other@example.com".into(), provider: Provider::Custom,
            password: "x".into(), imap_host: Some("127.0.0.1".into()), imap_port: None, smtp_host: None, smtp_port: None,
        };
        assert!(matches!(api.add_account(dup.clone()), Err(Error::Invalid(_))));
        let swapped = api.add_account(AddAccountRequest { replace: true, ..dup }).unwrap();
        assert_ne!(swapped.id, acct.id);
        assert_eq!(api.list_accounts().unwrap().len(), 1);
        api.remove_account(&swapped.id).unwrap();
        let acct = api
            .add_account(AddAccountRequest {
                name: "Fake".into(), replace: false, email: "fred@example.com".into(), provider: Provider::Custom,
                password: "good-password".into(), imap_host: Some("127.0.0.1".into()), imap_port: None, smtp_host: None, smtp_port: None,
            })
            .unwrap();
        let removed = api.remove_account("fake").unwrap();
        assert_eq!(removed.id, acct.id);
        assert!(api.list_accounts().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }
}
