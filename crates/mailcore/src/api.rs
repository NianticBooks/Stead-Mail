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
use crate::store::{MailboxRow, MessageRow, OAuthIdentity, Store};

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncReport {
    pub account: String,
    pub mailbox: String,
    pub exists: u32,
    pub uidvalidity: Option<u32>,
    pub uidvalidity_changed: bool,
    pub fetched: usize,
    pub stored: usize,
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

    /// Refresh the mailbox list and pull the newest `limit` headers of one mailbox into SQLite.
    pub async fn sync_mailbox(&self, key: &str, mailbox: &str, limit: u32) -> Result<SyncReport> {
        let account = self.store().find_account(key)?;
        let password = self.password_for(&account)?;
        let client = imap::connect_tls(&account.imap.host, account.imap.port).await?;
        let session = imap::login(client, &account.imap.username, &password).await?;
        self.sync_with_session(session, &account, mailbox, limit).await
    }

    /// Runtime-agnostic body of `sync_mailbox`; tests drive it with a plaintext session.
    pub async fn sync_with_session<T: imap::ImapStream>(
        &self,
        mut session: async_imap::Session<T>,
        account: &Account,
        mailbox: &str,
        limit: u32,
    ) -> Result<SyncReport> {
        let infos = imap::list_mailboxes(&mut session).await?;
        {
            let store = self.store();
            for info in &infos {
                store.upsert_mailbox(&account.id, &info.name, info.delimiter.as_deref(), &info.attributes.join(" "))?;
            }
        }
        let (state, headers) = imap::fetch_recent_headers(&mut session, mailbox, limit).await?;
        imap::logout(session).await?;

        let mut store = self.store();
        let row = store.find_mailbox(&account.id, mailbox)?;
        let uidvalidity_changed = matches!((row.uidvalidity, state.uidvalidity), (Some(old), Some(new)) if old != new);
        if uidvalidity_changed {
            tracing::warn!(mailbox, "UIDVALIDITY changed; dropping cached messages");
            store.clear_mailbox_messages(row.id)?;
        }
        store.update_mailbox_state(row.id, state.uidvalidity, state.uidnext, Some(state.exists))?;
        let stored = store.upsert_messages(&account.id, row.id, &headers)?;
        Ok(SyncReport {
            account: account.name.clone(),
            mailbox: row.name,
            exists: state.exists,
            uidvalidity: state.uidvalidity,
            uidvalidity_changed,
            fetched: headers.len(),
            stored,
            mailboxes_seen: infos.len(),
        })
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
        let report = api.sync_with_session(session, &acct, "inbox", 50).await.unwrap();
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
