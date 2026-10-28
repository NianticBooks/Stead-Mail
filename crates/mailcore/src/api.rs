//! The single public surface of mailcore. Everything a UI, CLI or agent can do goes through
//! [`Api`]; the JSON-RPC layer in [`crate::rpc`] is a thin serialiser over these methods.

use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::account::{Account, Provider};
use crate::error::{Error, Result};
use crate::imap;
use crate::secrets::SecretStore;
use crate::store::{MailboxRow, MessageRow, Store};

pub struct Api {
    store: Mutex<Store>,
    secrets: Box<dyn SecretStore>,
    data_dir: PathBuf,
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
        Api { store: Mutex::new(store), secrets, data_dir }
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

    /// Log in, list mailboxes, log out. Proves credentials and TLS without touching the DB.
    pub async fn test_connection(&self, key: &str) -> Result<ConnectionReport> {
        let account = self.store().find_account(key)?;
        let password = self.password_for(&account)?;
        let client = imap::connect_tls(&account.imap.host, account.imap.port).await?;
        let mut session = imap::login(client, &account.imap.username, &password).await?;
        let mailboxes = imap::list_mailboxes(&mut session).await?;
        imap::logout(session).await?;
        Ok(ConnectionReport { account: account.name, imap_host: account.imap.host, mailboxes })
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
