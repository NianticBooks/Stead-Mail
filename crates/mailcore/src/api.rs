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
use crate::store::{MailboxRow, MailboxStateUpdate, MessageDetail, MessageRow, OAuthIdentity, SearchHit, Store};
use crate::sync::{self, LocalView, SyncOptions};

const GOOGLE: &str = "google";
const GOOGLE_CLIENT_SECRET_KEY: &str = "oauth/google/client";

pub struct Api {
    store: Mutex<Store>,
    secrets: Box<dyn SecretStore>,
    data_dir: PathBuf,
    http: reqwest::Client,
    google_endpoints: GoogleEndpoints,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SendRequest {
    pub account: String,
    pub to: Vec<String>,
    pub subject: String,
    pub body: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SendReport {
    pub account: String,
    pub from: String,
    pub to: Vec<String>,
    pub smtp_host: String,
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
        Api { store: Mutex::new(store), secrets, data_dir, http, google_endpoints: GoogleEndpoints::default() }
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
        let smtp_error = smtp::verify_login(&account.smtp, &password).await.err().map(|e| e.to_string());
        Ok(ConnectionReport { account: account.name, imap_host: account.imap.host, mailboxes, smtp_host: account.smtp.host, smtp_error })
    }

    /// Send a plain-text message through the account's SMTP server.
    pub async fn send_text(&self, req: SendRequest) -> Result<SendReport> {
        let account = self.store().find_account(&req.account)?;
        let password = self.password_for(&account)?;
        let to: Vec<String> = req.to.iter().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
        if to.iter().any(|t| !t.contains('@')) {
            return Err(Error::Invalid("every recipient must be an email address".into()));
        }
        smtp::send_text(
            &account.smtp,
            &password,
            smtp::OutgoingText {
                from_name: Some(account.name.clone()).filter(|n| !n.is_empty()),
                from_email: account.email.clone(),
                to: to.clone(),
                subject: req.subject,
                body: req.body,
            },
        )
        .await?;
        Ok(SendReport { account: account.name, from: account.email, to, smtp_host: account.smtp.host })
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
