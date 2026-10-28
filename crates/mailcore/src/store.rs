//! SQLite persistence. One database per user; schema is versioned via `user_version`.

use std::path::Path;

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::account::{Account, AuthKind, Provider, ServerConfig};
use crate::error::{Error, Result};

const SCHEMA_VERSION: i64 = 1;

const MIGRATION_V1: &str = r#"
CREATE TABLE accounts (
    id            TEXT PRIMARY KEY,
    name          TEXT NOT NULL,
    email         TEXT NOT NULL,
    provider      TEXT NOT NULL,
    auth          TEXT NOT NULL,
    imap_host     TEXT NOT NULL,
    imap_port     INTEGER NOT NULL,
    imap_starttls INTEGER NOT NULL,
    imap_user     TEXT NOT NULL,
    smtp_host     TEXT NOT NULL,
    smtp_port     INTEGER NOT NULL,
    smtp_starttls INTEGER NOT NULL,
    smtp_user     TEXT NOT NULL,
    created_at    TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);

CREATE TABLE mailboxes (
    id             INTEGER PRIMARY KEY,
    account_id     TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    name           TEXT NOT NULL,
    delimiter      TEXT,
    attributes     TEXT NOT NULL DEFAULT '',
    uidvalidity    INTEGER,
    uidnext        INTEGER,
    highest_modseq INTEGER,
    exists_count   INTEGER,
    last_synced_at TEXT,
    UNIQUE(account_id, name)
);

CREATE TABLE messages (
    id            INTEGER PRIMARY KEY,
    account_id    TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    mailbox_id    INTEGER NOT NULL REFERENCES mailboxes(id) ON DELETE CASCADE,
    uid           INTEGER NOT NULL,
    message_id    TEXT,
    in_reply_to   TEXT,
    subject       TEXT,
    from_name     TEXT,
    from_addr     TEXT,
    to_addrs      TEXT,
    date          TEXT,
    internal_date TEXT,
    size          INTEGER,
    flags         TEXT NOT NULL DEFAULT '',
    UNIQUE(mailbox_id, uid)
);
CREATE INDEX messages_by_mailbox_date ON messages(mailbox_id, date DESC);
CREATE INDEX messages_by_message_id ON messages(message_id);
"#;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MailboxRow {
    pub id: i64,
    pub account_id: String,
    pub name: String,
    pub delimiter: Option<String>,
    pub attributes: String,
    pub uidvalidity: Option<u32>,
    pub uidnext: Option<u32>,
    pub exists_count: Option<u32>,
    pub last_synced_at: Option<String>,
}

/// A parsed message header ready to be stored. Produced by the IMAP layer.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct MessageHeader {
    pub uid: u32,
    pub message_id: Option<String>,
    pub in_reply_to: Option<String>,
    pub subject: Option<String>,
    pub from_name: Option<String>,
    pub from_addr: Option<String>,
    pub to_addrs: Option<String>,
    /// RFC 3339 from the Date header.
    pub date: Option<String>,
    /// RFC 3339 from IMAP INTERNALDATE.
    pub internal_date: Option<String>,
    pub size: Option<u32>,
    /// Space separated IMAP flags, e.g. `\Seen \Flagged`.
    pub flags: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageRow {
    pub id: i64,
    pub mailbox_id: i64,
    #[serde(flatten)]
    pub header: MessageHeader,
}

pub struct Store {
    conn: Connection,
}

impl Store {
    pub fn open(path: &Path) -> Result<Store> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        Self::init(conn)
    }

    pub fn open_in_memory() -> Result<Store> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Store> {
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
        let mut store = Store { conn };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&mut self) -> Result<()> {
        let version: i64 = self.conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version < 1 {
            let tx = self.conn.transaction()?;
            tx.execute_batch(MIGRATION_V1)?;
            tx.execute_batch(&format!("PRAGMA user_version = {SCHEMA_VERSION}"))?;
            tx.commit()?;
        }
        Ok(())
    }

    // ----- accounts -----

    pub fn insert_account(&self, a: &Account) -> Result<()> {
        self.conn.execute(
            "INSERT INTO accounts (id,name,email,provider,auth,imap_host,imap_port,imap_starttls,imap_user,
                                   smtp_host,smtp_port,smtp_starttls,smtp_user)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
            params![
                a.id, a.name, a.email, provider_str(a.provider), auth_str(a.auth),
                a.imap.host, a.imap.port, a.imap.starttls as i64, a.imap.username,
                a.smtp.host, a.smtp.port, a.smtp.starttls as i64, a.smtp.username,
            ],
        )?;
        Ok(())
    }

    pub fn delete_account(&self, id: &str) -> Result<bool> {
        Ok(self.conn.execute("DELETE FROM accounts WHERE id = ?1", [id])? > 0)
    }

    pub fn list_accounts(&self) -> Result<Vec<Account>> {
        let mut stmt = self.conn.prepare("SELECT * FROM accounts ORDER BY created_at")?;
        let rows = stmt.query_map([], row_to_account)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Look up by id, or by name (case-insensitive), or by email.
    pub fn find_account(&self, key: &str) -> Result<Account> {
        self.conn
            .query_row(
                "SELECT * FROM accounts WHERE id = ?1 OR lower(name) = lower(?1) OR lower(email) = lower(?1) LIMIT 1",
                [key],
                row_to_account,
            )
            .optional()?
            .ok_or_else(|| Error::AccountNotFound(key.to_string()))
    }

    // ----- mailboxes -----

    pub fn upsert_mailbox(&self, account_id: &str, name: &str, delimiter: Option<&str>, attributes: &str) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO mailboxes (account_id, name, delimiter, attributes) VALUES (?1,?2,?3,?4)
             ON CONFLICT(account_id, name) DO UPDATE SET delimiter = excluded.delimiter, attributes = excluded.attributes",
            params![account_id, name, delimiter, attributes],
        )?;
        Ok(self.conn.query_row(
            "SELECT id FROM mailboxes WHERE account_id = ?1 AND name = ?2",
            params![account_id, name],
            |r| r.get(0),
        )?)
    }

    pub fn update_mailbox_state(&self, mailbox_id: i64, uidvalidity: Option<u32>, uidnext: Option<u32>, exists: Option<u32>) -> Result<()> {
        self.conn.execute(
            "UPDATE mailboxes SET uidvalidity = ?2, uidnext = ?3, exists_count = ?4,
                    last_synced_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = ?1",
            params![mailbox_id, uidvalidity, uidnext, exists],
        )?;
        Ok(())
    }

    pub fn list_mailboxes(&self, account_id: &str) -> Result<Vec<MailboxRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, account_id, name, delimiter, attributes, uidvalidity, uidnext, exists_count, last_synced_at
             FROM mailboxes WHERE account_id = ?1 ORDER BY name",
        )?;
        let rows = stmt.query_map([account_id], |r| {
            Ok(MailboxRow {
                id: r.get(0)?,
                account_id: r.get(1)?,
                name: r.get(2)?,
                delimiter: r.get(3)?,
                attributes: r.get(4)?,
                uidvalidity: r.get(5)?,
                uidnext: r.get(6)?,
                exists_count: r.get(7)?,
                last_synced_at: r.get(8)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn find_mailbox(&self, account_id: &str, name: &str) -> Result<MailboxRow> {
        self.list_mailboxes(account_id)?
            .into_iter()
            .find(|m| m.name.eq_ignore_ascii_case(name))
            .ok_or_else(|| Error::MailboxNotFound(name.to_string()))
    }

    /// Drop every cached message in a mailbox. Required when UIDVALIDITY changes.
    pub fn clear_mailbox_messages(&self, mailbox_id: i64) -> Result<usize> {
        Ok(self.conn.execute("DELETE FROM messages WHERE mailbox_id = ?1", [mailbox_id])?)
    }

    // ----- messages -----

    pub fn upsert_messages(&mut self, account_id: &str, mailbox_id: i64, headers: &[MessageHeader]) -> Result<usize> {
        let tx = self.conn.transaction()?;
        let mut n = 0;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO messages (account_id, mailbox_id, uid, message_id, in_reply_to, subject, from_name, from_addr,
                                       to_addrs, date, internal_date, size, flags)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)
                 ON CONFLICT(mailbox_id, uid) DO UPDATE SET flags = excluded.flags",
            )?;
            for h in headers {
                n += stmt.execute(params![
                    account_id, mailbox_id, h.uid, h.message_id, h.in_reply_to, h.subject, h.from_name, h.from_addr,
                    h.to_addrs, h.date, h.internal_date, h.size, h.flags,
                ])?;
            }
        }
        tx.commit()?;
        Ok(n)
    }

    pub fn list_messages(&self, mailbox_id: i64, limit: usize) -> Result<Vec<MessageRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, mailbox_id, uid, message_id, in_reply_to, subject, from_name, from_addr, to_addrs, date,
                    internal_date, size, flags
             FROM messages WHERE mailbox_id = ?1
             ORDER BY COALESCE(date, internal_date) DESC, uid DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![mailbox_id, limit as i64], |r| {
            Ok(MessageRow {
                id: r.get(0)?,
                mailbox_id: r.get(1)?,
                header: MessageHeader {
                    uid: r.get(2)?,
                    message_id: r.get(3)?,
                    in_reply_to: r.get(4)?,
                    subject: r.get(5)?,
                    from_name: r.get(6)?,
                    from_addr: r.get(7)?,
                    to_addrs: r.get(8)?,
                    date: r.get(9)?,
                    internal_date: r.get(10)?,
                    size: r.get(11)?,
                    flags: r.get(12)?,
                },
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn count_messages(&self, mailbox_id: i64) -> Result<i64> {
        Ok(self.conn.query_row("SELECT COUNT(*) FROM messages WHERE mailbox_id = ?1", [mailbox_id], |r| r.get(0))?)
    }
}

fn provider_str(p: Provider) -> &'static str {
    match p {
        Provider::Icloud => "icloud",
        Provider::Gmail => "gmail",
        Provider::Custom => "custom",
    }
}
fn auth_str(a: AuthKind) -> &'static str {
    match a {
        AuthKind::Password => "password",
        AuthKind::OAuth2 => "oauth2",
    }
}

fn row_to_account(r: &rusqlite::Row<'_>) -> rusqlite::Result<Account> {
    let provider: String = r.get("provider")?;
    let auth: String = r.get("auth")?;
    Ok(Account {
        id: r.get("id")?,
        name: r.get("name")?,
        email: r.get("email")?,
        provider: provider.parse().unwrap_or(Provider::Custom),
        auth: if auth == "oauth2" { AuthKind::OAuth2 } else { AuthKind::Password },
        imap: ServerConfig {
            host: r.get("imap_host")?,
            port: r.get::<_, i64>("imap_port")? as u16,
            starttls: r.get::<_, i64>("imap_starttls")? != 0,
            username: r.get("imap_user")?,
        },
        smtp: ServerConfig {
            host: r.get("smtp_host")?,
            port: r.get::<_, i64>("smtp_port")? as u16,
            starttls: r.get::<_, i64>("smtp_starttls")? != 0,
            username: r.get("smtp_user")?,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_roundtrip_and_lookup() {
        let s = Store::open_in_memory().unwrap();
        let a = Account::from_preset("iCloud", "fred@icloud.com", Provider::Icloud).unwrap();
        s.insert_account(&a).unwrap();
        assert_eq!(s.list_accounts().unwrap(), vec![a.clone()]);
        assert_eq!(s.find_account("icloud").unwrap().id, a.id);
        assert_eq!(s.find_account("FRED@ICLOUD.COM").unwrap().id, a.id);
        assert!(matches!(s.find_account("nope"), Err(Error::AccountNotFound(_))));
    }

    #[test]
    fn mailbox_and_message_upsert_is_idempotent() {
        let mut s = Store::open_in_memory().unwrap();
        let a = Account::from_preset("g", "f@gmail.com", Provider::Gmail).unwrap();
        s.insert_account(&a).unwrap();
        let mb = s.upsert_mailbox(&a.id, "INBOX", Some("/"), "").unwrap();
        let mb2 = s.upsert_mailbox(&a.id, "INBOX", Some("."), "\\HasNoChildren").unwrap();
        assert_eq!(mb, mb2);
        let h = MessageHeader { uid: 7, subject: Some("hi".into()), flags: "".into(), ..Default::default() };
        s.upsert_messages(&a.id, mb, &[h.clone()]).unwrap();
        let h2 = MessageHeader { flags: "\\Seen".into(), ..h };
        s.upsert_messages(&a.id, mb, &[h2]).unwrap();
        let rows = s.list_messages(mb, 10).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].header.flags, "\\Seen");
        assert_eq!(s.count_messages(mb).unwrap(), 1);
    }
}
