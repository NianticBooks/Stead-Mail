//! SQLite persistence. One database per user; schema is versioned via `user_version`.

use std::path::Path;

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::account::{Account, AuthKind, Provider, ServerConfig};
use crate::error::{Error, Result};

const SCHEMA_VERSION: i64 = 13;

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

const MIGRATION_V2: &str = r#"
CREATE TABLE oauth_identities (
    id         TEXT PRIMARY KEY,
    provider   TEXT NOT NULL,
    email      TEXT NOT NULL,
    scopes     TEXT NOT NULL,
    client_id  TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    UNIQUE(provider, email)
);
"#;

const MIGRATION_V3: &str = r#"
ALTER TABLE messages ADD COLUMN references_hdr   TEXT;
ALTER TABLE messages ADD COLUMN thread_id        TEXT;
ALTER TABLE messages ADD COLUMN preview          TEXT;
ALTER TABLE messages ADD COLUMN body_text        TEXT;
ALTER TABLE messages ADD COLUMN body_html        TEXT;
ALTER TABLE messages ADD COLUMN body_fetched_at  TEXT;
ALTER TABLE messages ADD COLUMN has_attachments  INTEGER NOT NULL DEFAULT 0;
CREATE INDEX messages_by_thread ON messages(account_id, thread_id);

CREATE TABLE attachments (
    id           INTEGER PRIMARY KEY,
    message_id   INTEGER NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
    part_index   INTEGER NOT NULL,
    filename     TEXT,
    content_type TEXT NOT NULL,
    size         INTEGER NOT NULL,
    content_id   TEXT,
    inline       INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX attachments_by_message ON attachments(message_id);

-- External-content FTS5: text stays in `messages`, the index only holds tokens. Flag-only
-- updates never touch it because the update trigger lists the indexed columns.
CREATE VIRTUAL TABLE message_fts USING fts5(
    subject, from_name, from_addr, to_addrs, body_text,
    content='messages', content_rowid='id', tokenize='porter unicode61 remove_diacritics 2'
);
CREATE TRIGGER messages_fts_ai AFTER INSERT ON messages BEGIN
    INSERT INTO message_fts(rowid, subject, from_name, from_addr, to_addrs, body_text)
    VALUES (new.id, new.subject, new.from_name, new.from_addr, new.to_addrs, new.body_text);
END;
CREATE TRIGGER messages_fts_ad AFTER DELETE ON messages BEGIN
    INSERT INTO message_fts(message_fts, rowid, subject, from_name, from_addr, to_addrs, body_text)
    VALUES ('delete', old.id, old.subject, old.from_name, old.from_addr, old.to_addrs, old.body_text);
END;
CREATE TRIGGER messages_fts_au AFTER UPDATE OF subject, from_name, from_addr, to_addrs, body_text ON messages BEGIN
    INSERT INTO message_fts(message_fts, rowid, subject, from_name, from_addr, to_addrs, body_text)
    VALUES ('delete', old.id, old.subject, old.from_name, old.from_addr, old.to_addrs, old.body_text);
    INSERT INTO message_fts(rowid, subject, from_name, from_addr, to_addrs, body_text)
    VALUES (new.id, new.subject, new.from_name, new.from_addr, new.to_addrs, new.body_text);
END;
INSERT INTO message_fts(message_fts) VALUES ('rebuild');
"#;

/// Rows cached before v3 have no thread id. Give each one a stable id: its parent's message id
/// when it is a reply (so siblings and replies to one message still group), else its own.
const MIGRATION_V4: &str = r#"
UPDATE messages
   SET thread_id = COALESCE(in_reply_to, message_id, 'local:' || mailbox_id || ':' || uid)
 WHERE thread_id IS NULL;
"#;

const MIGRATION_V5: &str = r#"
CREATE TABLE outbox (
    id              TEXT PRIMARY KEY,
    account_id      TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    message_id      TEXT NOT NULL,
    from_addr       TEXT NOT NULL,
    rcpts           TEXT NOT NULL,
    subject         TEXT NOT NULL,
    raw             BLOB NOT NULL,
    size            INTEGER NOT NULL,
    state           TEXT NOT NULL CHECK (state IN ('queued','sending','sent','failed','cancelled')),
    attempts        INTEGER NOT NULL DEFAULT 0,
    next_attempt_at INTEGER NOT NULL,
    locked_at       INTEGER,
    last_error      TEXT,
    created_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    sent_at         TEXT
);
CREATE INDEX outbox_due ON outbox(state, next_attempt_at);
"#;

/// Server-reported unread count per mailbox (NULL = unknown). Cached rows alone cannot answer this:
/// only the newest slice of a large mailbox is stored locally.
const MIGRATION_V6: &str = "ALTER TABLE mailboxes ADD COLUMN unseen INTEGER;";

const MIGRATION_V7: &str = r#"
ALTER TABLE messages ADD COLUMN calendar_data TEXT;
ALTER TABLE messages ADD COLUMN ai_events_at TEXT;

CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);

CREATE TABLE ai_triage (
    message_id     INTEGER PRIMARY KEY REFERENCES messages(id) ON DELETE CASCADE,
    category       TEXT NOT NULL,
    needs_reply    INTEGER NOT NULL,
    urgency        TEXT NOT NULL,
    summary        TEXT NOT NULL,
    model          TEXT NOT NULL,
    prompt_version INTEGER NOT NULL,
    created_at     TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);

CREATE TABLE event_proposals (
    id          INTEGER PRIMARY KEY,
    message_id  INTEGER NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
    source      TEXT NOT NULL CHECK (source IN ('ics','ai')),
    method      TEXT,
    ics_uid     TEXT,
    title       TEXT NOT NULL,
    start_utc   TEXT NOT NULL,
    end_utc     TEXT,
    all_day     INTEGER NOT NULL DEFAULT 0,
    tz          TEXT,
    location    TEXT,
    attendees   TEXT NOT NULL DEFAULT '[]',
    rrule       TEXT,
    confidence  REAL,
    quote       TEXT,
    model       TEXT,
    state       TEXT NOT NULL DEFAULT 'pending' CHECK (state IN ('pending','accepted','dismissed')),
    created_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    UNIQUE(message_id, title, start_utc)
);
CREATE INDEX proposals_by_message ON event_proposals(message_id);

-- Every model call, local or cloud, by size only. Message content is never stored here.
CREATE TABLE ai_calls (
    id        INTEGER PRIMARY KEY,
    at        TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    provider  TEXT NOT NULL,
    model     TEXT NOT NULL,
    task      TEXT NOT NULL,
    bytes_out INTEGER NOT NULL,
    bytes_in  INTEGER NOT NULL,
    millis    INTEGER NOT NULL,
    ok        INTEGER NOT NULL,
    error     TEXT
);

-- Events the user accepted. The calendar views and sync arrive in a later phase; this is where
-- accepted proposals land until then.
CREATE TABLE calendar_events (
    id                 INTEGER PRIMARY KEY,
    title              TEXT NOT NULL,
    start_utc          TEXT NOT NULL,
    end_utc            TEXT,
    all_day            INTEGER NOT NULL DEFAULT 0,
    tz                 TEXT,
    location           TEXT,
    attendees          TEXT NOT NULL DEFAULT '[]',
    rrule              TEXT,
    ics_uid            TEXT,
    source_proposal_id INTEGER,
    source_message_id  INTEGER,
    created_at         TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);
CREATE UNIQUE INDEX calendar_events_by_proposal ON calendar_events(source_proposal_id) WHERE source_proposal_id IS NOT NULL;
"#;

/// The calendar proper: named calendars and the events in them. Events carry what two-way sync needs
/// (remote id, etag, dirty flag, tombstone), so the sync phase adds no migration here. The minimal
/// calendar_events table from the AI phase is folded into a default local calendar.
const MIGRATION_V9: &str = r#"
CREATE TABLE calendars (
    id         TEXT PRIMARY KEY,
    name       TEXT NOT NULL,
    color      TEXT NOT NULL,
    kind       TEXT NOT NULL DEFAULT 'local',
    read_only  INTEGER NOT NULL DEFAULT 0,
    visible    INTEGER NOT NULL DEFAULT 1,
    account_id TEXT,
    remote_id  TEXT,
    sync_token TEXT,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now'))
);
INSERT INTO calendars (id, name, color, kind) VALUES ('personal', 'Personal', '#7aa2f7', 'local');

CREATE TABLE events (
    id                 TEXT PRIMARY KEY,
    calendar_id        TEXT NOT NULL REFERENCES calendars(id) ON DELETE CASCADE,
    uid                TEXT NOT NULL,
    title              TEXT NOT NULL,
    description        TEXT,
    location           TEXT,
    start_utc          TEXT NOT NULL,
    end_utc            TEXT NOT NULL,
    all_day            INTEGER NOT NULL DEFAULT 0,
    tz                 TEXT,
    rrule              TEXT,
    exdates            TEXT NOT NULL DEFAULT '[]',
    status             TEXT NOT NULL DEFAULT 'confirmed',
    organizer          TEXT,
    attendees          TEXT NOT NULL DEFAULT '[]',
    source_proposal_id INTEGER,
    source_message_id  INTEGER,
    sequence           INTEGER NOT NULL DEFAULT 0,
    remote_id          TEXT,
    etag               TEXT,
    dirty              INTEGER NOT NULL DEFAULT 1,
    deleted            INTEGER NOT NULL DEFAULT 0,
    created_at         TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now')),
    updated_at         TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now'))
);
CREATE UNIQUE INDEX events_by_uid ON events(calendar_id, uid) WHERE deleted = 0;
CREATE UNIQUE INDEX events_by_proposal ON events(source_proposal_id) WHERE source_proposal_id IS NOT NULL;
CREATE INDEX events_by_start ON events(start_utc);

INSERT INTO events (id, calendar_id, uid, title, location, start_utc, end_utc, all_day, tz, rrule, attendees, source_proposal_id, source_message_id, created_at)
SELECT lower(hex(randomblob(16))), 'personal',
       COALESCE(ics_uid, lower(hex(randomblob(12))) || '@stead'),
       title, location, start_utc,
       CASE WHEN end_utc IS NOT NULL AND end_utc > start_utc THEN end_utc
            WHEN all_day = 1 THEN strftime('%Y-%m-%dT%H:%M:%SZ', start_utc, '+1 day')
            ELSE strftime('%Y-%m-%dT%H:%M:%SZ', start_utc, '+1 hour') END,
       all_day, tz, rrule, attendees, source_proposal_id, source_message_id, created_at
FROM calendar_events;
DROP TABLE calendar_events;
"#;

/// Two-way calendar sync: a log of what sync did to the user's data (conflicts above all), plus
/// the calendar-side bookkeeping. `extra` keeps parts of a remote event we do not model (alarms,
/// overridden occurrences) so that pushing an edit does not delete them from the server.
/// Reminders belong to this machine: they are never pushed to Google or iCloud.
/// `reminders_fired` remembers what already went off so a restart does not repeat it.
const MIGRATION_V12: &str = r#"
CREATE TABLE reminders (
    event_id TEXT NOT NULL REFERENCES events(id) ON DELETE CASCADE,
    minutes  INTEGER NOT NULL CHECK (minutes >= 0),
    PRIMARY KEY (event_id, minutes)
);
CREATE TABLE reminders_fired (
    event_id TEXT NOT NULL REFERENCES events(id) ON DELETE CASCADE,
    instance TEXT NOT NULL,
    minutes  INTEGER NOT NULL,
    fired_at INTEGER NOT NULL,
    PRIMARY KEY (event_id, instance, minutes)
);
"#;

/// A sent message is also filed in the account's Sent folder (iCloud does not do it by itself).
/// `sent_copy`: pending -> done | skipped | failed; the raw message is kept until it settles.
const MIGRATION_V11: &str = r#"
ALTER TABLE outbox ADD COLUMN sent_copy TEXT NOT NULL DEFAULT 'skipped';
ALTER TABLE outbox ADD COLUMN copy_error TEXT;
-- NULL = never read; '' = read, no Cc. Reply-all needs it.
ALTER TABLE messages ADD COLUMN cc_addrs TEXT;
-- Unsent messages, kept on this machine. `body` and the lists are what the compose window held.
CREATE TABLE drafts (
    id          TEXT PRIMARY KEY,
    account_id  TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    data        TEXT NOT NULL,
    subject     TEXT NOT NULL DEFAULT '',
    updated_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);
"#;

const MIGRATION_V10: &str = r#"
CREATE TABLE sync_log (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    at            TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now')),
    calendar_id   TEXT,
    calendar_name TEXT NOT NULL DEFAULT '',
    kind          TEXT NOT NULL,
    title         TEXT NOT NULL DEFAULT '',
    detail        TEXT NOT NULL DEFAULT ''
);
ALTER TABLE events ADD COLUMN extra TEXT;
ALTER TABLE calendars ADD COLUMN last_sync_at TEXT;
ALTER TABLE calendars ADD COLUMN last_sync_error TEXT;
CREATE INDEX events_by_remote ON events(calendar_id, remote_id);
"#;

/// Triage can flag a message that tried to instruct an AI.
const MIGRATION_V8: &str = "ALTER TABLE ai_triage ADD COLUMN suspicious INTEGER NOT NULL DEFAULT 0;";

/// Mailing-list headers, read lazily from the server. A row means "looked": both values are
/// empty strings for a message that offers no unsubscribe. `unsub_senders` remembers what was
/// done about a sender so it stops being suggested.
const MIGRATION_V13: &str = r#"
CREATE TABLE message_unsub (
    message_id        INTEGER PRIMARY KEY REFERENCES messages(id) ON DELETE CASCADE,
    list_unsubscribe  TEXT NOT NULL,
    list_post         TEXT NOT NULL
);
CREATE TABLE unsub_senders (
    account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    sender     TEXT NOT NULL,
    state      TEXT NOT NULL,
    method     TEXT,
    at         TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    PRIMARY KEY (account_id, sender)
);
"#;

/// One cached message that offers an unsubscribe header, as the suggestions need it.
#[derive(Debug, Clone)]
pub struct UnsubRow {
    pub message_id: i64,
    pub uid: u32,
    pub from_addr: String,
    pub from_name: Option<String>,
    pub subject: Option<String>,
    pub date: Option<String>,
    pub flags: String,
    pub category: Option<String>,
    pub list_unsubscribe: String,
    pub list_post: String,
}

/// An OAuth2 login (e.g. a Google account authorised for Calendar). The refresh token lives in
/// the secret store under [`OAuthIdentity::refresh_secret_key`], never in SQLite.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OAuthIdentity {
    pub id: String,
    pub provider: String,
    pub email: String,
    pub scopes: Vec<String>,
    pub client_id: String,
    pub created_at: String,
}

impl OAuthIdentity {
    pub fn refresh_secret_key(&self) -> String {
        format!("oauth/{}/refresh_token", self.id)
    }
    pub fn access_secret_key(&self) -> String {
        format!("oauth/{}/access_token", self.id)
    }
}

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
    /// Unread messages on the server as of the last sync.
    pub unseen: Option<u32>,
}

/// A parsed message header ready to be stored. Produced by the IMAP layer.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct MessageHeader {
    pub uid: u32,
    pub message_id: Option<String>,
    pub in_reply_to: Option<String>,
    /// Space separated Message-IDs from the References header, oldest first.
    pub references: Option<String>,
    pub subject: Option<String>,
    pub from_name: Option<String>,
    pub from_addr: Option<String>,
    pub to_addrs: Option<String>,
    /// Cc header; `Some("")` when the header was read and is absent.
    pub cc_addrs: Option<String>,
    /// RFC 3339 from the Date header.
    pub date: Option<String>,
    /// RFC 3339 from IMAP INTERNALDATE.
    pub internal_date: Option<String>,
    pub size: Option<u32>,
    /// Space separated IMAP flags in canonical (sorted) order, e.g. `\Flagged \Seen`.
    pub flags: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MessageRow {
    pub id: i64,
    pub mailbox_id: i64,
    #[serde(flatten)]
    pub header: MessageHeader,
    pub thread_id: Option<String>,
    pub preview: Option<String>,
    pub has_attachments: bool,
    pub body_cached: bool,
    /// AI triage, when it has been run for this message.
    pub triage: Option<TriageBrief>,
    /// Event proposals from this message that the user has not decided about yet.
    pub pending_events: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TriageBrief {
    pub category: String,
    pub needs_reply: bool,
    pub urgency: String,
    pub summary: String,
    /// The message tried to give instructions to an AI.
    pub suspicious: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EventProposal {
    pub id: i64,
    pub message_id: i64,
    #[serde(flatten)]
    pub proposal: crate::ai::events::NewProposal,
    /// pending | accepted | dismissed
    pub state: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AiCall {
    pub id: i64,
    pub at: String,
    pub provider: String,
    pub model: String,
    pub task: String,
    pub bytes_out: i64,
    pub bytes_in: i64,
    pub millis: i64,
    pub ok: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AiUsage {
    pub provider: String,
    pub calls: i64,
    pub bytes_out: i64,
    pub bytes_in: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AttachmentRow {
    pub part_index: i64,
    pub filename: Option<String>,
    pub content_type: String,
    pub size: i64,
    pub content_id: Option<String>,
    pub inline: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MessageDetail {
    #[serde(flatten)]
    pub row: MessageRow,
    pub mailbox: String,
    pub body_text: Option<String>,
    pub body_html: Option<String>,
    pub attachments: Vec<AttachmentRow>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SearchHit {
    pub mailbox: String,
    pub snippet: String,
    #[serde(flatten)]
    pub row: MessageRow,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApplyCounts {
    pub inserted: usize,
    pub flags_updated: usize,
    pub deleted: usize,
}

/// What the server told us about a mailbox when it was selected.
#[derive(Debug, Clone, Copy, Default)]
pub struct MailboxStateUpdate {
    pub uidvalidity: Option<u32>,
    pub uidnext: Option<u32>,
    pub exists: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OutboxItem {
    pub id: String,
    pub account_id: String,
    pub message_id: String,
    pub from: String,
    pub rcpts: Vec<String>,
    pub subject: String,
    pub size: i64,
    /// queued | sending | sent | failed | cancelled
    pub state: String,
    pub attempts: i64,
    /// Unix seconds; a queued item is due when this is in the past.
    pub next_attempt_at: i64,
    pub last_error: Option<String>,
    pub created_at: String,
    pub sent_at: Option<String>,
    /// pending | done | skipped | failed: whether the copy in the Sent folder is filed.
    pub sent_copy: String,
    pub copy_error: Option<String>,
}

pub struct NewOutbox<'a> {
    pub account_id: &'a str,
    pub message_id: &'a str,
    pub from: &'a str,
    pub rcpts: &'a [String],
    pub subject: &'a str,
    pub raw: &'a [u8],
    pub now: i64,
}

const OUTBOX_COLS: &str = "id, account_id, message_id, from_addr, rcpts, subject, size, state, attempts, next_attempt_at, last_error, created_at, sent_at, sent_copy, copy_error";

fn row_to_outbox(r: &rusqlite::Row<'_>) -> rusqlite::Result<OutboxItem> {
    let rcpts: String = r.get(4)?;
    Ok(OutboxItem {
        id: r.get(0)?,
        account_id: r.get(1)?,
        message_id: r.get(2)?,
        from: r.get(3)?,
        rcpts: serde_json::from_str(&rcpts).unwrap_or_default(),
        subject: r.get(5)?,
        size: r.get(6)?,
        state: r.get(7)?,
        attempts: r.get(8)?,
        next_attempt_at: r.get(9)?,
        last_error: r.get(10)?,
        created_at: r.get(11)?,
        sent_at: r.get(12)?,
        sent_copy: r.get(13)?,
        copy_error: r.get(14)?,
    })
}

fn msg_cols(prefix: &str) -> String {
    // Correlated subqueries must name the outer row explicitly: an unqualified `id` would bind to
    // the inner table when it has an `id` column of its own (event_proposals does).
    let outer = if prefix.is_empty() { "messages.".to_string() } else { prefix.to_string() };
    [
        "id", "mailbox_id", "uid", "message_id", "in_reply_to", "references_hdr", "subject", "from_name", "from_addr",
        "to_addrs", "date", "internal_date", "size", "flags", "thread_id", "preview", "has_attachments",
    ]
    .iter()
    .map(|c| format!("{prefix}{c}"))
    .collect::<Vec<_>>()
    .join(", ")
        + &format!(", {prefix}body_fetched_at IS NOT NULL")
        + &format!(", (SELECT category FROM ai_triage WHERE message_id = {outer}id)")
        + &format!(", (SELECT needs_reply FROM ai_triage WHERE message_id = {outer}id)")
        + &format!(", (SELECT urgency FROM ai_triage WHERE message_id = {outer}id)")
        + &format!(", (SELECT summary FROM ai_triage WHERE message_id = {outer}id)")
        + &format!(", (SELECT suspicious FROM ai_triage WHERE message_id = {outer}id)")
        + &format!(", (SELECT COUNT(*) FROM event_proposals WHERE event_proposals.message_id = {outer}id AND event_proposals.state = 'pending')")
}

fn row_to_message(r: &rusqlite::Row<'_>) -> rusqlite::Result<MessageRow> {
    Ok(MessageRow {
        id: r.get(0)?,
        mailbox_id: r.get(1)?,
        header: MessageHeader {
            uid: r.get(2)?,
            message_id: r.get(3)?,
            in_reply_to: r.get(4)?,
            references: r.get(5)?,
            subject: r.get(6)?,
            from_name: r.get(7)?,
            from_addr: r.get(8)?,
            to_addrs: r.get(9)?,
            date: r.get(10)?,
            internal_date: r.get(11)?,
            size: r.get(12)?,
            flags: r.get(13)?,
            cc_addrs: None,
        },
        thread_id: r.get(14)?,
        preview: r.get(15)?,
        has_attachments: r.get::<_, i64>(16)? != 0,
        body_cached: r.get::<_, i64>(17)? != 0,
        triage: match r.get::<_, Option<String>>(18)? {
            Some(category) => Some(TriageBrief {
                category,
                needs_reply: r.get::<_, Option<i64>>(19)?.unwrap_or(0) != 0,
                urgency: r.get::<_, Option<String>>(20)?.unwrap_or_default(),
                summary: r.get::<_, Option<String>>(21)?.unwrap_or_default(),
                suspicious: r.get::<_, Option<i64>>(22)?.unwrap_or(0) != 0,
            }),
            None => None,
        },
        pending_events: r.get::<_, Option<i64>>(23)?.unwrap_or(0),
    })
}

/// Turn free text into a safe FTS5 query: every word becomes a quoted phrase (so operators like
/// `OR`, `-`, `:` or `*` typed by the user are inert) and the last word is a prefix match.
pub fn fts_query(text: &str) -> Option<String> {
    let words: Vec<String> = text
        .split_whitespace()
        .map(|w| w.replace('"', ""))
        .filter(|w| !w.is_empty())
        .collect();
    if words.is_empty() {
        return None;
    }
    let last = words.len() - 1;
    Some(
        words
            .iter()
            .enumerate()
            .map(|(i, w)| if i == last { format!("\"{w}\"*") } else { format!("\"{w}\"") })
            .collect::<Vec<_>>()
            .join(" "),
    )
}

pub struct Store {
    pub(crate) conn: Connection,
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
        let tx = self.conn.transaction()?;
        if version < 1 {
            tx.execute_batch(MIGRATION_V1)?;
        }
        if version < 2 {
            tx.execute_batch(MIGRATION_V2)?;
        }
        if version < 3 {
            tx.execute_batch(MIGRATION_V3)?;
        }
        if version < 4 {
            tx.execute_batch(MIGRATION_V4)?;
        }
        if version < 5 {
            tx.execute_batch(MIGRATION_V5)?;
        }
        if version < 6 {
            tx.execute_batch(MIGRATION_V6)?;
        }
        if version < 7 {
            tx.execute_batch(MIGRATION_V7)?;
        }
        if version < 8 {
            tx.execute_batch(MIGRATION_V8)?;
        }
        if version < 9 {
            tx.execute_batch(MIGRATION_V9)?;
        }
        if version < 10 {
            tx.execute_batch(MIGRATION_V10)?;
        }
        if version < 11 {
            tx.execute_batch(MIGRATION_V11)?;
        }
        if version < 12 {
            tx.execute_batch(MIGRATION_V12)?;
        }
        if version < 13 {
            tx.execute_batch(MIGRATION_V13)?;
        }
        if version < SCHEMA_VERSION {
            tx.execute_batch(&format!("PRAGMA user_version = {SCHEMA_VERSION}"))?;
        }
        tx.commit()?;
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

    // ----- oauth identities -----

    pub fn upsert_oauth_identity(&self, id: &OAuthIdentity) -> Result<OAuthIdentity> {
        self.conn.execute(
            "INSERT INTO oauth_identities (id, provider, email, scopes, client_id) VALUES (?1,?2,?3,?4,?5)
             ON CONFLICT(provider, email) DO UPDATE SET scopes = excluded.scopes, client_id = excluded.client_id",
            params![id.id, id.provider, id.email, id.scopes.join(" "), id.client_id],
        )?;
        self.find_oauth_identity(&id.provider, &id.email)
    }

    pub fn list_oauth_identities(&self, provider: &str) -> Result<Vec<OAuthIdentity>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, provider, email, scopes, client_id, created_at FROM oauth_identities WHERE provider = ?1 ORDER BY created_at",
        )?;
        let rows = stmt.query_map([provider], row_to_identity)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Look up by id or by email (case-insensitive).
    pub fn find_oauth_identity(&self, provider: &str, key: &str) -> Result<OAuthIdentity> {
        self.conn
            .query_row(
                "SELECT id, provider, email, scopes, client_id, created_at FROM oauth_identities
                 WHERE provider = ?1 AND (id = ?2 OR lower(email) = lower(?2)) LIMIT 1",
                params![provider, key],
                row_to_identity,
            )
            .optional()?
            .ok_or_else(|| Error::AccountNotFound(format!("{provider} identity '{key}'")))
    }

    pub fn delete_oauth_identity(&self, id: &str) -> Result<bool> {
        Ok(self.conn.execute("DELETE FROM oauth_identities WHERE id = ?1", [id])? > 0)
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
            "SELECT id, account_id, name, delimiter, attributes, uidvalidity, uidnext, exists_count, last_synced_at, unseen
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
                unseen: r.get(9)?,
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

    // ----- drafts -----

    /// Insert or replace a draft. `data` is the JSON the API layer owns; the store only indexes it.
    pub fn draft_put(&self, id: &str, account_id: &str, subject: &str, data: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO drafts (id, account_id, data, subject) VALUES (?1,?2,?3,?4)
             ON CONFLICT(id) DO UPDATE SET account_id=?2, data=?3, subject=?4, updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now')",
            params![id, account_id, data, subject],
        )?;
        Ok(())
    }

    /// (id, account_id, data, updated_at), newest first.
    pub fn draft_list(&self) -> Result<Vec<(String, String, String, String)>> {
        let mut stmt = self.conn.prepare("SELECT id, account_id, data, updated_at FROM drafts ORDER BY updated_at DESC, rowid DESC")?;
        Ok(stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?.collect::<std::result::Result<_, _>>()?)
    }

    pub fn draft_get(&self, id: &str) -> Result<(String, String, String, String)> {
        self.conn
            .query_row("SELECT id, account_id, data, updated_at FROM drafts WHERE id=?1", [id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .map_err(|_| Error::Invalid(format!("no draft '{id}'")))
    }

    pub fn draft_delete(&self, id: &str) -> Result<bool> {
        Ok(self.conn.execute("DELETE FROM drafts WHERE id=?1", [id])? == 1)
    }

    /// The stored Cc header of a cached message (`None` = not captured yet).
    pub fn message_cc(&self, mailbox_id: i64, uid: u32) -> Result<Option<String>> {
        Ok(self.conn.query_row("SELECT cc_addrs FROM messages WHERE mailbox_id=?1 AND uid=?2", params![mailbox_id, uid], |r| r.get(0))?)
    }

    pub fn set_message_cc(&self, mailbox_id: i64, uid: u32, cc: &str) -> Result<()> {
        self.conn.execute("UPDATE messages SET cc_addrs=?3 WHERE mailbox_id=?1 AND uid=?2", params![mailbox_id, uid, cc])?;
        Ok(())
    }

    /// Drop every cached message in a mailbox. Required when UIDVALIDITY changes.
    pub fn clear_mailbox_messages(&self, mailbox_id: i64) -> Result<usize> {
        Ok(self.conn.execute("DELETE FROM messages WHERE mailbox_id = ?1", [mailbox_id])?)
    }

    // ----- messages -----

    /// (uid, flags) of every cached message in a mailbox, ascending by uid.
    pub fn mailbox_id_of(&self, message_id: i64) -> Result<i64> {
        Ok(self.conn.query_row("SELECT mailbox_id FROM messages WHERE id = ?1", [message_id], |r| r.get(0))?)
    }

    pub fn local_uid_flags(&self, mailbox_id: i64) -> Result<Vec<(u32, String)>> {
        let mut stmt = self.conn.prepare("SELECT uid, flags FROM messages WHERE mailbox_id = ?1 ORDER BY uid")?;
        let rows = stmt.query_map([mailbox_id], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn mailbox_by_id(&self, mailbox_id: i64) -> Result<MailboxRow> {
        self.conn
            .query_row(
                "SELECT id, account_id, name, delimiter, attributes, uidvalidity, uidnext, exists_count, last_synced_at, unseen
                 FROM mailboxes WHERE id = ?1",
                [mailbox_id],
                |r| {
                    Ok(MailboxRow {
                        id: r.get(0)?, account_id: r.get(1)?, name: r.get(2)?, delimiter: r.get(3)?, attributes: r.get(4)?,
                        uidvalidity: r.get(5)?, uidnext: r.get(6)?, exists_count: r.get(7)?, last_synced_at: r.get(8)?,
                        unseen: r.get(9)?,
                    })
                },
            )
            .optional()?
            .ok_or_else(|| Error::MailboxNotFound(mailbox_id.to_string()))
    }

    /// Apply one sync result atomically: optional cache reset, new messages (with threading),
    /// flag changes, expunges, and the mailbox's server state.
    pub fn apply_sync(
        &mut self,
        account_id: &str,
        mailbox_id: i64,
        reset: bool,
        new: &[MessageHeader],
        flag_updates: &[(u32, String)],
        expunged: &[u32],
        state: MailboxStateUpdate,
    ) -> Result<ApplyCounts> {
        let tx = self.conn.transaction()?;
        let mut counts = ApplyCounts::default();
        if reset {
            counts.deleted += tx.execute("DELETE FROM messages WHERE mailbox_id = ?1", [mailbox_id])?;
        }
        {
            let mut insert = tx.prepare(
                "INSERT INTO messages (account_id, mailbox_id, uid, message_id, in_reply_to, references_hdr, subject,
                                       from_name, from_addr, to_addrs, date, internal_date, size, flags, thread_id, cc_addrs)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)
                 ON CONFLICT(mailbox_id, uid) DO NOTHING",
            )?;
            for h in new {
                let thread = thread_for(&tx, account_id, mailbox_id, h)?;
                let n = insert.execute(params![
                    account_id, mailbox_id, h.uid, h.message_id, h.in_reply_to, h.references, h.subject, h.from_name,
                    h.from_addr, h.to_addrs, h.date, h.internal_date, h.size, h.flags, thread, h.cc_addrs,
                ])?;
                counts.inserted += n;
                // Messages that used this one as a virtual thread root join its real thread.
                if n == 1 {
                    if let Some(mid) = &h.message_id {
                        if *mid != thread {
                            tx.execute(
                                "UPDATE messages SET thread_id = ?1 WHERE account_id = ?2 AND thread_id = ?3 AND message_id IS NOT ?3",
                                params![thread, account_id, mid],
                            )?;
                        }
                    }
                }
            }
            let mut upd = tx.prepare("UPDATE messages SET flags = ?3 WHERE mailbox_id = ?1 AND uid = ?2 AND flags != ?3")?;
            for (uid, flags) in flag_updates {
                counts.flags_updated += upd.execute(params![mailbox_id, uid, flags])?;
            }
            let mut del = tx.prepare("DELETE FROM messages WHERE mailbox_id = ?1 AND uid = ?2")?;
            for uid in expunged {
                counts.deleted += del.execute(params![mailbox_id, uid])?;
            }
        }
        tx.execute(
            "UPDATE mailboxes SET uidvalidity = ?2, uidnext = ?3, exists_count = ?4,
                    last_synced_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = ?1",
            params![mailbox_id, state.uidvalidity, state.uidnext, state.exists],
        )?;
        tx.commit()?;
        Ok(counts)
    }

    pub fn list_messages(&self, mailbox_id: i64, limit: usize) -> Result<Vec<MessageRow>> {
        self.list_messages_page(mailbox_id, limit, 0)
    }

    pub fn list_messages_page(&self, mailbox_id: i64, limit: usize, offset: usize) -> Result<Vec<MessageRow>> {
        let sql = format!(
            "SELECT {} FROM messages WHERE mailbox_id = ?1 ORDER BY COALESCE(date, internal_date) DESC, uid DESC LIMIT ?2 OFFSET ?3",
            msg_cols("")
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params![mailbox_id, limit as i64, offset as i64], row_to_message)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn set_mailbox_unseen(&self, mailbox_id: i64, unseen: Option<u32>) -> Result<()> {
        self.conn.execute("UPDATE mailboxes SET unseen = ?2 WHERE id = ?1", params![mailbox_id, unseen])?;
        Ok(())
    }

    /// Record a flag change made through this app, keeping the unread count in step without
    /// waiting for the next sync.
    pub fn set_local_flag(&self, mailbox_id: i64, uid: u32, flag: &str, on: bool) -> Result<String> {
        let current: String = self
            .conn
            .query_row("SELECT flags FROM messages WHERE mailbox_id = ?1 AND uid = ?2", params![mailbox_id, uid], |r| r.get(0))
            .optional()?
            .ok_or_else(|| Error::Invalid(format!("message uid {uid} is not cached")))?;
        let mut flags: Vec<String> = current.split_whitespace().map(String::from).collect();
        let had = flags.iter().any(|f| f == flag);
        if on && !had {
            flags.push(flag.to_string());
        } else if !on {
            flags.retain(|f| f != flag);
        }
        flags.sort();
        let new = flags.join(" ");
        self.conn.execute("UPDATE messages SET flags = ?3 WHERE mailbox_id = ?1 AND uid = ?2", params![mailbox_id, uid, new])?;
        if flag == "\\Seen" && had != on {
            // Seen gained = one fewer unread; Seen lost = one more.
            let delta: i64 = if on { -1 } else { 1 };
            self.conn.execute("UPDATE mailboxes SET unseen = MAX(0, unseen + ?2) WHERE id = ?1 AND unseen IS NOT NULL", params![mailbox_id, delta])?;
        }
        Ok(new)
    }

    /// Drop one message from the cache after it left the mailbox on the server, keeping the
    /// unread count in step. Returns false when it was not cached.
    pub fn remove_local_message(&self, mailbox_id: i64, uid: u32) -> Result<bool> {
        let flags: Option<String> = self
            .conn
            .query_row("SELECT flags FROM messages WHERE mailbox_id = ?1 AND uid = ?2", params![mailbox_id, uid], |r| r.get(0))
            .optional()?;
        let Some(flags) = flags else { return Ok(false) };
        self.conn.execute("DELETE FROM messages WHERE mailbox_id = ?1 AND uid = ?2", params![mailbox_id, uid])?;
        if !flags.split_whitespace().any(|f| f == "\\Seen") {
            self.conn.execute("UPDATE mailboxes SET unseen = MAX(0, unseen - 1) WHERE id = ?1 AND unseen IS NOT NULL", [mailbox_id])?;
        }
        Ok(true)
    }

    // ----- unsubscribe -----

    /// Newest cached messages whose list headers have not been read yet: `(message id, uid)`.
    pub fn unscanned_for_unsub(&self, mailbox_id: i64, limit: usize) -> Result<Vec<(i64, u32)>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, uid FROM messages WHERE mailbox_id = ?1 AND from_addr IS NOT NULL
               AND NOT EXISTS (SELECT 1 FROM message_unsub WHERE message_id = messages.id)
             ORDER BY uid DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![mailbox_id, limit as i64], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// UIDs of cached messages from one sender in a mailbox (address compared case-insensitively).
    pub fn sender_uids(&self, mailbox_id: i64, sender: &str) -> Result<Vec<u32>> {
        let mut stmt = self.conn.prepare("SELECT uid FROM messages WHERE mailbox_id = ?1 AND lower(from_addr) = lower(?2) ORDER BY uid")?;
        let rows = stmt.query_map(params![mailbox_id, sender], |r| r.get(0))?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn has_unsub_row(&self, message_id: i64) -> Result<bool> {
        Ok(self.conn.query_row("SELECT EXISTS(SELECT 1 FROM message_unsub WHERE message_id = ?1)", [message_id], |r| r.get(0))?)
    }

    pub fn set_unsub_headers(&mut self, rows: &[(i64, String, String)]) -> Result<()> {
        let tx = self.conn.transaction()?;
        {
            let mut ins = tx.prepare("INSERT OR REPLACE INTO message_unsub (message_id, list_unsubscribe, list_post) VALUES (?1,?2,?3)")?;
            for (id, list, post) in rows {
                ins.execute(params![id, list, post])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Every cached message in the mailbox that offers an unsubscribe header.
    pub fn unsub_rows(&self, mailbox_id: i64) -> Result<Vec<UnsubRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT m.id, m.uid, m.from_addr, m.from_name, m.subject, m.date, m.flags,
                    (SELECT category FROM ai_triage WHERE message_id = m.id), u.list_unsubscribe, u.list_post
             FROM messages m JOIN message_unsub u ON u.message_id = m.id
             WHERE m.mailbox_id = ?1 AND m.from_addr IS NOT NULL AND u.list_unsubscribe != ''
             ORDER BY COALESCE(m.date, m.internal_date) DESC, m.uid DESC",
        )?;
        let rows = stmt.query_map([mailbox_id], |r| {
            Ok(UnsubRow {
                message_id: r.get(0)?,
                uid: r.get(1)?,
                from_addr: r.get(2)?,
                from_name: r.get(3)?,
                subject: r.get(4)?,
                date: r.get(5)?,
                flags: r.get(6)?,
                category: r.get(7)?,
                list_unsubscribe: r.get(8)?,
                list_post: r.get(9)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Total and unread cached messages per sender in a mailbox (lower-cased address).
    pub fn sender_counts(&self, mailbox_id: i64) -> Result<std::collections::HashMap<String, (u32, u32)>> {
        let mut stmt = self.conn.prepare(
            "SELECT lower(from_addr), COUNT(*), SUM(CASE WHEN (' ' || flags || ' ') LIKE '% \\Seen %' THEN 0 ELSE 1 END)
             FROM messages WHERE mailbox_id = ?1 AND from_addr IS NOT NULL GROUP BY lower(from_addr)",
        )?;
        let rows = stmt.query_map([mailbox_id], |r| Ok((r.get::<_, String>(0)?, (r.get::<_, u32>(1)?, r.get::<_, u32>(2)?))))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    pub fn unsub_sender_states(&self, account_id: &str) -> Result<std::collections::HashMap<String, String>> {
        let mut stmt = self.conn.prepare("SELECT sender, state FROM unsub_senders WHERE account_id = ?1")?;
        let rows = stmt.query_map([account_id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    pub fn set_unsub_sender(&self, account_id: &str, sender: &str, state: &str, method: Option<&str>) -> Result<()> {
        self.conn.execute(
            "INSERT INTO unsub_senders (account_id, sender, state, method) VALUES (?1,?2,?3,?4)
             ON CONFLICT(account_id, sender) DO UPDATE SET state = excluded.state, method = excluded.method,
                                                          at = strftime('%Y-%m-%dT%H:%M:%fZ','now')",
            params![account_id, sender.to_ascii_lowercase(), state, method],
        )?;
        Ok(())
    }

    pub fn count_messages(&self, mailbox_id: i64) -> Result<i64> {
        Ok(self.conn.query_row("SELECT COUNT(*) FROM messages WHERE mailbox_id = ?1", [mailbox_id], |r| r.get(0))?)
    }

    pub fn find_message(&self, mailbox_id: i64, uid: u32) -> Result<MessageRow> {
        let sql = format!("SELECT {} FROM messages WHERE mailbox_id = ?1 AND uid = ?2", msg_cols(""));
        self.conn
            .query_row(&sql, params![mailbox_id, uid], row_to_message)
            .optional()?
            .ok_or_else(|| Error::Invalid(format!("message uid {uid} is not cached; sync the mailbox first")))
    }

    pub fn message_detail(&self, row: MessageRow) -> Result<MessageDetail> {
        let (text, html): (Option<String>, Option<String>) =
            self.conn.query_row("SELECT body_text, body_html FROM messages WHERE id = ?1", [row.id], |r| Ok((r.get(0)?, r.get(1)?)))?;
        let mailbox: String = self.conn.query_row("SELECT name FROM mailboxes WHERE id = ?1", [row.mailbox_id], |r| r.get(0))?;
        let mut stmt = self.conn.prepare(
            "SELECT part_index, filename, content_type, size, content_id, inline FROM attachments WHERE message_id = ?1 ORDER BY part_index",
        )?;
        let attachments = stmt
            .query_map([row.id], |r| {
                Ok(AttachmentRow {
                    part_index: r.get(0)?, filename: r.get(1)?, content_type: r.get(2)?, size: r.get(3)?,
                    content_id: r.get(4)?, inline: r.get::<_, i64>(5)? != 0,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(MessageDetail { row, mailbox, body_text: text, body_html: html, attachments })
    }

    pub fn set_message_body(&mut self, message_id: i64, body: &crate::body::ParsedBody) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "UPDATE messages SET body_text = ?2, body_html = ?3, preview = ?4, has_attachments = ?5, calendar_data = ?6,
                    body_fetched_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = ?1",
            params![message_id, body.text, body.html, body.preview(), !body.attachments.is_empty() as i64, body.calendar],
        )?;
        tx.execute("DELETE FROM attachments WHERE message_id = ?1", [message_id])?;
        for a in &body.attachments {
            tx.execute(
                "INSERT INTO attachments (message_id, part_index, filename, content_type, size, content_id, inline)
                 VALUES (?1,?2,?3,?4,?5,?6,?7)",
                params![message_id, a.part_index, a.filename, a.content_type, a.size, a.content_id, a.inline as i64],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Full-text search over cached subjects, addresses and fetched bodies, best match first.
    pub fn search(&self, query: &str, account_id: Option<&str>, limit: usize) -> Result<Vec<SearchHit>> {
        let Some(q) = fts_query(query) else { return Ok(Vec::new()) };
        let sql = format!(
            "SELECT {}, mb.name, snippet(message_fts, -1, '[', ']', '…', 12)
             FROM message_fts JOIN messages m ON m.id = message_fts.rowid JOIN mailboxes mb ON mb.id = m.mailbox_id
             WHERE message_fts MATCH ?1 AND (?2 IS NULL OR m.account_id = ?2)
             ORDER BY rank LIMIT ?3",
            msg_cols("m.")
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params![q, account_id, limit as i64], |r| {
            Ok(SearchHit { row: row_to_message(r)?, mailbox: r.get(24)?, snippet: r.get(25)? })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Every cached message in a thread, oldest first.
    // ----- settings -----

    pub fn get_setting(&self, key: &str) -> Result<Option<String>> {
        Ok(self.conn.query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| r.get(0)).optional()?)
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute("INSERT INTO settings (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value", params![key, value])?;
        Ok(())
    }

    // ----- AI: triage, proposals, audit, accepted events -----

    pub fn calendar_data(&self, message_id: i64) -> Result<Option<String>> {
        Ok(self.conn.query_row("SELECT calendar_data FROM messages WHERE id = ?1", [message_id], |r| r.get(0))?)
    }

    pub fn ai_events_done(&self, message_id: i64) -> Result<bool> {
        Ok(self.conn.query_row("SELECT ai_events_at IS NOT NULL FROM messages WHERE id = ?1", [message_id], |r| r.get::<_, i64>(0))? != 0)
    }

    pub fn mark_ai_events_done(&self, message_id: i64) -> Result<()> {
        self.conn.execute("UPDATE messages SET ai_events_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = ?1", [message_id])?;
        Ok(())
    }

    pub fn save_triage(&self, message_id: i64, t: &crate::ai::triage::Triage) -> Result<()> {
        self.conn.execute(
            "INSERT INTO ai_triage (message_id, category, needs_reply, urgency, summary, model, prompt_version, suspicious)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8)
             ON CONFLICT(message_id) DO UPDATE SET category=excluded.category, needs_reply=excluded.needs_reply, urgency=excluded.urgency,
                 summary=excluded.summary, model=excluded.model, prompt_version=excluded.prompt_version, suspicious=excluded.suspicious,
                 created_at=strftime('%Y-%m-%dT%H:%M:%fZ','now')",
            params![message_id, t.category.as_str(), t.needs_reply as i64, t.urgency.as_str(), t.summary, t.model, t.prompt_version, t.suspicious as i64],
        )?;
        Ok(())
    }

    /// Stored triage and the prompt version it was made with, if any.
    pub fn triage_version(&self, message_id: i64) -> Result<Option<u32>> {
        Ok(self.conn.query_row("SELECT prompt_version FROM ai_triage WHERE message_id = ?1", [message_id], |r| r.get(0)).optional()?)
    }

    /// Store proposals for a message. Pending proposals from the same source kinds are replaced;
    /// proposals the user already accepted or dismissed are never touched or duplicated.
    pub fn store_proposals(&mut self, message_id: i64, replace_sources: &[&str], new: &[crate::ai::events::NewProposal]) -> Result<Vec<EventProposal>> {
        let tx = self.conn.transaction()?;
        for src in replace_sources {
            tx.execute("DELETE FROM event_proposals WHERE message_id = ?1 AND source = ?2 AND state = 'pending'", params![message_id, src])?;
        }
        for p in new {
            tx.execute(
                "INSERT OR IGNORE INTO event_proposals (message_id, source, method, ics_uid, title, start_utc, end_utc, all_day, tz, location,
                                                        attendees, rrule, confidence, quote, model)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)",
                params![
                    message_id, p.source, p.method, p.ics_uid, p.title, p.start_utc, p.end_utc, p.all_day as i64, p.tz, p.location,
                    serde_json::to_string(&p.attendees).map_err(|e| Error::Other(e.to_string()))?, p.rrule, p.confidence, p.quote, p.model,
                ],
            )?;
        }
        tx.commit()?;
        self.proposals_for_message(message_id)
    }

    fn row_to_proposal(r: &rusqlite::Row<'_>) -> rusqlite::Result<EventProposal> {
        let attendees: String = r.get(11)?;
        Ok(EventProposal {
            id: r.get(0)?,
            message_id: r.get(1)?,
            proposal: crate::ai::events::NewProposal {
                source: r.get(2)?,
                method: r.get(3)?,
                ics_uid: r.get(4)?,
                title: r.get(5)?,
                start_utc: r.get(6)?,
                end_utc: r.get(7)?,
                all_day: r.get::<_, i64>(8)? != 0,
                tz: r.get(9)?,
                location: r.get(10)?,
                attendees: serde_json::from_str(&attendees).unwrap_or_default(),
                rrule: r.get(12)?,
                confidence: r.get(13)?,
                quote: r.get(14)?,
                model: r.get(15)?,
            },
            state: r.get(16)?,
        })
    }

    const PROPOSAL_COLS: &'static str =
        "id, message_id, source, method, ics_uid, title, start_utc, end_utc, all_day, tz, location, attendees, rrule, confidence, quote, model, state";

    pub fn proposals_for_message(&self, message_id: i64) -> Result<Vec<EventProposal>> {
        let mut stmt = self.conn.prepare(&format!("SELECT {} FROM event_proposals WHERE message_id = ?1 ORDER BY start_utc, id", Self::PROPOSAL_COLS))?;
        Ok(stmt.query_map([message_id], Self::row_to_proposal)?.collect::<std::result::Result<_, _>>()?)
    }

    pub fn pending_proposals(&self, account_id: Option<&str>, limit: usize) -> Result<Vec<EventProposal>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {} FROM event_proposals p WHERE state = 'pending' AND (?1 IS NULL OR message_id IN (SELECT id FROM messages WHERE account_id = ?1))
             ORDER BY start_utc LIMIT ?2",
            Self::PROPOSAL_COLS
        ))?;
        Ok(stmt.query_map(params![account_id, limit as i64], Self::row_to_proposal)?.collect::<std::result::Result<_, _>>()?)
    }

    pub fn get_proposal(&self, id: i64) -> Result<EventProposal> {
        self.conn
            .query_row(&format!("SELECT {} FROM event_proposals WHERE id = ?1", Self::PROPOSAL_COLS), [id], Self::row_to_proposal)
            .optional()?
            .ok_or_else(|| Error::Invalid(format!("no event proposal {id}")))
    }

    pub fn dismiss_proposal(&self, id: i64) -> Result<EventProposal> {
        let n = self.conn.execute("UPDATE event_proposals SET state = 'dismissed' WHERE id = ?1 AND state = 'pending'", [id])?;
        let p = self.get_proposal(id)?;
        if n == 0 && p.state != "dismissed" {
            return Err(Error::Invalid(format!("proposal {id} is already {}", p.state)));
        }
        Ok(p)
    }

    /// Accept a pending proposal: it becomes an event in the default calendar, atomically.
    /// Cancellation notices are information, not events to add. If the invitation is already on
    /// the calendar (same UID), the proposal is marked accepted and the existing event is returned.
    pub fn accept_proposal(&mut self, id: i64) -> Result<crate::calendar::Event> {
        use crate::calendar::model::{rfc3339, parse_instant};
        let tx = self.conn.transaction()?;
        #[allow(clippy::type_complexity)]
        let row: Option<(String, Option<String>, String, String, String, Option<String>, bool, Option<String>, Option<String>, String, Option<String>, Option<String>, i64)> = tx
            .query_row(
                "SELECT state, method, title, start_utc, COALESCE(end_utc, ''), location, all_day, tz, rrule, attendees, ics_uid, NULL, message_id FROM event_proposals WHERE id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get::<_, i64>(6)? != 0, r.get(7)?, r.get(8)?, r.get(9)?, r.get(10)?, r.get::<_, Option<String>>(11)?, r.get(12)?)),
            )
            .optional()?;
        let Some((state, method, title, start, end, location, all_day, tz, rrule, attendees, uid, organizer, message_id)) = row else {
            return Err(Error::Invalid(format!("no event proposal {id}")));
        };
        if state != "pending" {
            return Err(Error::Invalid(format!("this proposal is already {state}")));
        }
        if method.as_deref() == Some("CANCEL") {
            return Err(Error::Invalid("this is a cancellation notice, not an event to add".into()));
        }
        let cal = crate::calendar::db::default_calendar_id(&tx)?;
        let uid = uid.unwrap_or_else(|| format!("{}@stead", uuid::Uuid::new_v4()));
        if let Some(existing) = crate::calendar::db::event_by_uid(&tx, &cal, &uid)? {
            tx.execute("UPDATE event_proposals SET state = 'accepted' WHERE id = ?1", [id])?;
            tx.commit()?;
            return Ok(existing);
        }
        let start_t = parse_instant(&start)?;
        let end_t = parse_instant(&end).ok().filter(|e| *e > start_t).unwrap_or(start_t + if all_day { chrono::Duration::days(1) } else { chrono::Duration::hours(1) });
        let ev = crate::calendar::Event {
            id: uuid::Uuid::new_v4().simple().to_string(),
            calendar_id: cal,
            uid,
            title,
            description: None,
            location,
            start_utc: rfc3339(start_t),
            end_utc: rfc3339(end_t),
            all_day,
            tz,
            rrule,
            exdates: vec![],
            status: "confirmed".into(),
            organizer,
            attendees: serde_json::from_str(&attendees).unwrap_or_default(),
            source_message_id: Some(message_id),
            sequence: 0,
            updated_at: String::new(),
        };
        crate::calendar::db::put_event(&tx, &ev, Some(id))?;
        tx.execute("UPDATE event_proposals SET state = 'accepted' WHERE id = ?1", [id])?;
        let saved = crate::calendar::db::event(&tx, &ev.id)?;
        tx.commit()?;
        Ok(saved)
    }

    pub fn record_ai_call(&self, provider: &str, model: &str, task: &str, bytes_out: usize, bytes_in: usize, millis: u64, error: Option<&str>) -> Result<()> {
        self.conn.execute(
            "INSERT INTO ai_calls (provider, model, task, bytes_out, bytes_in, millis, ok, error) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            params![provider, model, task, bytes_out as i64, bytes_in as i64, millis as i64, error.is_none() as i64, error],
        )?;
        Ok(())
    }

    pub fn ai_calls(&self, limit: usize) -> Result<Vec<AiCall>> {
        let mut stmt = self.conn.prepare("SELECT id, at, provider, model, task, bytes_out, bytes_in, millis, ok, error FROM ai_calls ORDER BY id DESC LIMIT ?1")?;
        Ok(stmt
            .query_map([limit as i64], |r| {
                Ok(AiCall { id: r.get(0)?, at: r.get(1)?, provider: r.get(2)?, model: r.get(3)?, task: r.get(4)?, bytes_out: r.get(5)?, bytes_in: r.get(6)?, millis: r.get(7)?, ok: r.get::<_, i64>(8)? != 0, error: r.get(9)? })
            })?
            .collect::<std::result::Result<_, _>>()?)
    }

    /// Calls and bytes per provider over the last 30 days.
    pub fn ai_usage(&self) -> Result<Vec<AiUsage>> {
        let mut stmt = self.conn.prepare(
            "SELECT provider, COUNT(*), COALESCE(SUM(bytes_out),0), COALESCE(SUM(bytes_in),0) FROM ai_calls
             WHERE at >= strftime('%Y-%m-%dT%H:%M:%fZ','now','-30 days') GROUP BY provider ORDER BY provider",
        )?;
        Ok(stmt.query_map([], |r| Ok(AiUsage { provider: r.get(0)?, calls: r.get(1)?, bytes_out: r.get(2)?, bytes_in: r.get(3)? }))?.collect::<std::result::Result<_, _>>()?)
    }

    // ----- outbox -----

    pub fn outbox_insert(&self, n: &NewOutbox<'_>) -> Result<OutboxItem> {
        let id = uuid::Uuid::new_v4().to_string();
        self.conn.execute(
            "INSERT INTO outbox (id, account_id, message_id, from_addr, rcpts, subject, raw, size, state, next_attempt_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,'queued',?9)",
            params![
                id, n.account_id, n.message_id, n.from,
                serde_json::to_string(n.rcpts).map_err(|e| Error::Other(e.to_string()))?,
                n.subject, n.raw, n.raw.len() as i64, n.now,
            ],
        )?;
        self.outbox_get(&id)
    }

    /// Look up by full id or by an unambiguous id prefix (what the CLI prints).
    pub fn outbox_get(&self, id: &str) -> Result<OutboxItem> {
        let mut stmt = self.conn.prepare(&format!("SELECT {OUTBOX_COLS} FROM outbox WHERE id = ?1 OR id LIKE ?2 || '%' LIMIT 2"))?;
        let mut rows: Vec<OutboxItem> = stmt.query_map(params![id, id], row_to_outbox)?.collect::<std::result::Result<_, _>>()?;
        match rows.len() {
            1 => Ok(rows.remove(0)),
            0 => Err(Error::Invalid(format!("no outbox item '{id}'"))),
            _ => Err(Error::Invalid(format!("outbox id prefix '{id}' is ambiguous"))),
        }
    }

    pub fn outbox_raw(&self, id: &str) -> Result<Vec<u8>> {
        Ok(self.conn.query_row("SELECT raw FROM outbox WHERE id = ?1", [id], |r| r.get(0))?)
    }

    pub fn outbox_due(&self, now: i64, account_id: Option<&str>) -> Result<Vec<OutboxItem>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {OUTBOX_COLS} FROM outbox WHERE state = 'queued' AND next_attempt_at <= ?1 AND (?2 IS NULL OR account_id = ?2)
             ORDER BY next_attempt_at, created_at"
        ))?;
        Ok(stmt.query_map(params![now, account_id], row_to_outbox)?.collect::<std::result::Result<_, _>>()?)
    }

    pub fn outbox_list(&self, account_id: Option<&str>, include_done: bool, limit: usize) -> Result<Vec<OutboxItem>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {OUTBOX_COLS} FROM outbox WHERE (?1 IS NULL OR account_id = ?1) AND (?2 OR state IN ('queued','sending','failed'))
             ORDER BY created_at DESC LIMIT ?3"
        ))?;
        Ok(stmt.query_map(params![account_id, include_done, limit as i64], row_to_outbox)?.collect::<std::result::Result<_, _>>()?)
    }

    /// Claim an item for sending. Returns false if someone else already did.
    pub fn outbox_lock(&self, id: &str, now: i64) -> Result<bool> {
        Ok(self.conn.execute("UPDATE outbox SET state='sending', locked_at=?2 WHERE id=?1 AND state='queued'", params![id, now])? == 1)
    }

    /// The bytes are no longer needed once the server has accepted the message.
    pub fn outbox_mark_sent(&self, id: &str, keep_copy: bool) -> Result<()> {
        self.conn.execute(
            "UPDATE outbox SET state='sent', attempts=attempts+1, locked_at=NULL, last_error=NULL,
                    sent_at=strftime('%Y-%m-%dT%H:%M:%fZ','now'), sent_copy=?2,
                    raw=CASE WHEN ?2='pending' THEN raw ELSE x'' END WHERE id=?1",
            params![id, if keep_copy { "pending" } else { "skipped" }],
        )?;
        Ok(())
    }

    /// Record how filing the Sent copy went; the raw bytes are only dropped once it settles.
    pub fn outbox_set_copy(&self, id: &str, state: &str, error: Option<&str>) -> Result<()> {
        self.conn.execute(
            "UPDATE outbox SET sent_copy=?2, copy_error=?3, raw=CASE WHEN ?2='failed' THEN raw ELSE x'' END WHERE id=?1",
            params![id, state, error],
        )?;
        Ok(())
    }

    /// Sent messages whose Sent-folder copy still has to be filed (never filed, or failed before).
    pub fn outbox_copy_due(&self) -> Result<Vec<OutboxItem>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {OUTBOX_COLS} FROM outbox WHERE state='sent' AND sent_copy IN ('pending','failed') AND length(raw) > 0 ORDER BY created_at"
        ))?;
        Ok(stmt.query_map([], row_to_outbox)?.collect::<std::result::Result<_, _>>()?)
    }

    pub fn outbox_mark_retry(&self, id: &str, next_attempt_at: i64, error: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE outbox SET state='queued', attempts=attempts+1, next_attempt_at=?2, locked_at=NULL, last_error=?3 WHERE id=?1",
            params![id, next_attempt_at, error],
        )?;
        Ok(())
    }

    pub fn outbox_mark_failed(&self, id: &str, error: &str) -> Result<()> {
        self.conn.execute("UPDATE outbox SET state='failed', attempts=attempts+1, locked_at=NULL, last_error=?2 WHERE id=?1", params![id, error])?;
        Ok(())
    }

    /// Put a failed item back in the queue, due now, with a fresh attempt budget.
    pub fn outbox_requeue(&self, id: &str, now: i64) -> Result<bool> {
        Ok(self.conn.execute("UPDATE outbox SET state='queued', attempts=0, next_attempt_at=?2, last_error=NULL WHERE id=?1 AND state='failed'", params![id, now])? == 1)
    }

    pub fn outbox_cancel(&self, id: &str) -> Result<bool> {
        Ok(self.conn.execute("UPDATE outbox SET state='cancelled', raw=x'', locked_at=NULL WHERE id=?1 AND state IN ('queued','failed')", [id])? == 1)
    }

    /// Items stuck in `sending` because a process died mid-send go back to the queue. The window is
    /// deliberately long: a crash after the server accepted the message can cause one duplicate,
    /// but a short window would double-send while a slow connection is still working.
    pub fn outbox_recover_stuck(&self, now: i64, stale_secs: i64) -> Result<usize> {
        Ok(self.conn.execute("UPDATE outbox SET state='queued', locked_at=NULL WHERE state='sending' AND locked_at < ?1", [now - stale_secs])?)
    }

    pub fn thread_messages(&self, account_id: &str, thread_id: &str) -> Result<Vec<MessageRow>> {
        let sql = format!(
            "SELECT {} FROM messages WHERE account_id = ?1 AND thread_id = ?2 ORDER BY COALESCE(date, internal_date), uid",
            msg_cols("")
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params![account_id, thread_id], row_to_message)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }
}

/// Decide a new message's thread. A cached parent wins; otherwise the oldest referenced id acts
/// as a virtual root so replies to a message we have not fetched still group together.
fn thread_for(tx: &rusqlite::Transaction<'_>, account_id: &str, mailbox_id: i64, h: &MessageHeader) -> Result<String> {
    let own = h.message_id.clone().unwrap_or_else(|| format!("local:{mailbox_id}:{}", h.uid));
    let refs: Vec<&str> = h.references.as_deref().unwrap_or("").split_whitespace().collect();
    let parent = h.in_reply_to.as_deref().or_else(|| refs.last().copied());
    if let Some(p) = parent {
        let cached: Option<String> = tx
            .query_row(
                "SELECT thread_id FROM messages WHERE account_id = ?1 AND message_id = ?2 AND thread_id IS NOT NULL LIMIT 1",
                params![account_id, p],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(t) = cached {
            return Ok(t);
        }
    }
    Ok(refs.first().copied().or(parent).map(String::from).unwrap_or(own))
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

fn row_to_identity(r: &rusqlite::Row<'_>) -> rusqlite::Result<OAuthIdentity> {
    let scopes: String = r.get(3)?;
    Ok(OAuthIdentity {
        id: r.get(0)?,
        provider: r.get(1)?,
        email: r.get(2)?,
        scopes: scopes.split_whitespace().map(String::from).collect(),
        client_id: r.get(4)?,
        created_at: r.get(5)?,
    })
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
    #[test]
    fn sync_bookkeeping_columns_exist() {
        let s = Store::open_in_memory().unwrap();
        s.conn.execute("INSERT INTO sync_log (kind) VALUES ('info')", []).unwrap();
        s.conn.execute("UPDATE events SET extra = NULL", []).unwrap();
        s.conn.execute("UPDATE calendars SET last_sync_at = NULL, last_sync_error = NULL", []).unwrap();
    }

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
    fn oauth_identity_upsert_is_keyed_by_email() {
        let s = Store::open_in_memory().unwrap();
        let v: i64 = s.conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(v, SCHEMA_VERSION);
        let a = OAuthIdentity { id: "id1".into(), provider: "google".into(), email: "Fred@gmail.com".into(), scopes: vec!["a".into()], client_id: "c".into(), created_at: String::new() };
        let stored = s.upsert_oauth_identity(&a).unwrap();
        assert_eq!(stored.id, "id1");
        let b = OAuthIdentity { id: "id2".into(), scopes: vec!["a".into(), "b".into()], ..a.clone() };
        let again = s.upsert_oauth_identity(&b).unwrap();
        assert_eq!(again.id, "id1", "same email keeps the original id");
        assert_eq!(again.scopes, vec!["a", "b"]);
        assert_eq!(s.list_oauth_identities("google").unwrap().len(), 1);
        assert_eq!(s.find_oauth_identity("google", "fred@GMAIL.com").unwrap().id, "id1");
        assert!(s.delete_oauth_identity("id1").unwrap());
    }

    fn hdr(uid: u32, mid: &str, irt: Option<&str>, refs: Option<&str>) -> MessageHeader {
        MessageHeader {
            uid,
            message_id: Some(mid.into()),
            in_reply_to: irt.map(Into::into),
            references: refs.map(Into::into),
            subject: Some(format!("subject {uid}")),
            ..Default::default()
        }
    }

    fn setup() -> (Store, String, i64) {
        let s = Store::open_in_memory().unwrap();
        let a = Account::from_preset("g", "f@gmail.com", Provider::Gmail).unwrap();
        s.insert_account(&a).unwrap();
        let mb = s.upsert_mailbox(&a.id, "INBOX", Some("/"), "").unwrap();
        (s, a.id, mb)
    }

    #[test]
    fn apply_sync_is_idempotent_and_updates_flags_and_deletes() {
        let (mut s, acct, mb) = setup();
        let st = MailboxStateUpdate { uidvalidity: Some(1), uidnext: Some(10), exists: Some(2) };
        let c = s.apply_sync(&acct, mb, false, &[hdr(7, "<a>", None, None), hdr(8, "<b>", None, None)], &[], &[], st).unwrap();
        assert_eq!(c.inserted, 2);
        // Re-applying the same headers inserts nothing.
        let c = s.apply_sync(&acct, mb, false, &[hdr(7, "<a>", None, None)], &[], &[], st).unwrap();
        assert_eq!(c.inserted, 0);
        let c = s.apply_sync(&acct, mb, false, &[], &[(7, "\\Seen".into()), (8, "".into())], &[], st).unwrap();
        assert_eq!(c.flags_updated, 1, "unchanged flags are not counted");
        let c = s.apply_sync(&acct, mb, false, &[], &[], &[8], st).unwrap();
        assert_eq!(c.deleted, 1);
        let rows = s.list_messages(mb, 10).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].header.flags, "\\Seen");
        let mbrow = s.mailbox_by_id(mb).unwrap();
        assert_eq!((mbrow.uidvalidity, mbrow.uidnext, mbrow.exists_count), (Some(1), Some(10), Some(2)));
        // reset wipes the cache before inserting
        let c = s.apply_sync(&acct, mb, true, &[hdr(1, "<z>", None, None)], &[], &[], st).unwrap();
        assert_eq!((c.inserted, c.deleted), (1, 1));
        assert_eq!(s.count_messages(mb).unwrap(), 1);
    }

    #[test]
    fn threads_group_regardless_of_arrival_order() {
        let (mut s, acct, mb) = setup();
        let st = MailboxStateUpdate::default();
        // Newest reply first (parent <a> and root <root> not cached yet): virtual root = <root>.
        s.apply_sync(&acct, mb, false, &[hdr(30, "<c>", Some("<a>"), Some("<root> <a>"))], &[], &[], st).unwrap();
        // The middle message arrives next; its only reference is the root.
        s.apply_sync(&acct, mb, false, &[hdr(20, "<a>", Some("<root>"), Some("<root>"))], &[], &[], st).unwrap();
        // Finally the root itself.
        s.apply_sync(&acct, mb, false, &[hdr(10, "<root>", None, None)], &[], &[], st).unwrap();
        let t = s.thread_messages(&acct, "<root>").unwrap();
        assert_eq!(t.iter().map(|m| m.header.uid).collect::<Vec<_>>(), vec![10, 20, 30]);
        // An unrelated message stays alone.
        s.apply_sync(&acct, mb, false, &[hdr(40, "<other>", None, None)], &[], &[], st).unwrap();
        assert_eq!(s.thread_messages(&acct, "<other>").unwrap().len(), 1);
    }

    #[test]
    fn late_parent_pulls_virtual_root_children_into_its_thread() {
        let (mut s, acct, mb) = setup();
        let st = MailboxStateUpdate::default();
        // <p> is cached in thread <t>. <r> replies to <x>, which is not cached: virtual root <x>.
        s.apply_sync(&acct, mb, false, &[hdr(1, "<t>", None, None), hdr(2, "<p>", Some("<t>"), Some("<t>"))], &[], &[], st).unwrap();
        s.apply_sync(&acct, mb, false, &[hdr(5, "<r>", Some("<x>"), Some("<x>"))], &[], &[], st).unwrap();
        assert_eq!(s.thread_messages(&acct, "<x>").unwrap().len(), 1);
        // <x> arrives and turns out to be a reply to <p>: its thread is <t>, and <r> follows.
        s.apply_sync(&acct, mb, false, &[hdr(4, "<x>", Some("<p>"), Some("<t> <p>"))], &[], &[], st).unwrap();
        let t: Vec<u32> = s.thread_messages(&acct, "<t>").unwrap().iter().map(|m| m.header.uid).collect();
        assert_eq!(t, vec![1, 2, 4, 5]);
        assert!(s.thread_messages(&acct, "<x>").unwrap().is_empty());
    }

    #[test]
    fn fts_follows_inserts_updates_and_deletes_but_not_flag_changes() {
        let (mut s, acct, mb) = setup();
        let st = MailboxStateUpdate::default();
        let mut h = hdr(1, "<a>", None, None);
        h.subject = Some("Quarterly budget review".into());
        h.from_name = Some("Jill Book".into());
        s.apply_sync(&acct, mb, false, &[h], &[], &[], st).unwrap();
        assert_eq!(s.search("budget", None, 10).unwrap().len(), 1);
        assert_eq!(s.search("bud", None, 10).unwrap().len(), 1, "last word is a prefix match");
        assert_eq!(s.search("jill", Some(&acct), 10).unwrap().len(), 1);
        assert_eq!(s.search("jill", Some("other-account"), 10).unwrap().len(), 0);
        assert_eq!(s.search("budget OR -\"x:*", None, 10).unwrap().len(), 0, "operators are inert, not a syntax error");
        // Body arrives later and becomes searchable.
        let row = s.find_message(mb, 1).unwrap();
        let body = crate::body::ParsedBody { text: Some("Please approve the zeppelin purchase".into()), ..Default::default() };
        s.set_message_body(row.id, &body).unwrap();
        let hits = s.search("zeppelin", None, 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].snippet.contains("[zeppelin]"), "{}", hits[0].snippet);
        assert!(hits[0].row.body_cached);
        // Flag-only updates leave the index alone and still searchable.
        s.apply_sync(&acct, mb, false, &[], &[(1, "\\Seen".into())], &[], st).unwrap();
        assert_eq!(s.search("zeppelin", None, 10).unwrap().len(), 1);
        // Expunge removes it from the index.
        s.apply_sync(&acct, mb, false, &[], &[], &[1], st).unwrap();
        assert_eq!(s.search("zeppelin", None, 10).unwrap().len(), 0);
        assert_eq!(s.search("", None, 10).unwrap().len(), 0);
    }

    #[test]
    fn upgrading_a_v2_database_threads_old_rows_and_indexes_them() {
        // Build a genuine v2 database (what Phase 0/1 users have), with rows that predate threading.
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(MIGRATION_V1).unwrap();
        conn.execute_batch(MIGRATION_V2).unwrap();
        conn.execute_batch("PRAGMA user_version = 2").unwrap();
        conn.execute("INSERT INTO accounts VALUES ('a','n','e@x.com','custom','password','h',993,0,'u','h',587,1,'u',datetime('now'))", []).unwrap();
        conn.execute("INSERT INTO mailboxes (id, account_id, name) VALUES (1,'a','INBOX')", []).unwrap();
        for (uid, mid, irt, subject) in [(1, Some("<p>"), None, "Plan"), (2, Some("<r1>"), Some("<p>"), "Re: Plan"), (3, Some("<r2>"), Some("<p>"), "Re: Plan"), (4, None, None, "No id")] {
            conn.execute(
                "INSERT INTO messages (account_id, mailbox_id, uid, message_id, in_reply_to, subject, flags) VALUES ('a',1,?1,?2,?3,?4,'')",
                params![uid, mid, irt, subject],
            ).unwrap();
        }
        let s = Store::init(conn).unwrap();
        let v: i64 = s.conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(v, SCHEMA_VERSION);
        let thread = |uid: u32| s.find_message(1, uid).unwrap().thread_id.unwrap();
        assert_eq!(thread(1), "<p>");
        assert_eq!(thread(2), "<p>");
        assert_eq!(thread(3), "<p>", "replies to one message share its thread");
        assert_eq!(thread(4), "local:1:4", "messages with no ids still get a unique thread");
        assert_eq!(s.thread_messages("a", "<p>").unwrap().len(), 3);
        // The pre-existing rows were indexed by the migration's FTS rebuild.
        assert_eq!(s.search("plan", None, 10).unwrap().len(), 3);
    }

    #[test]
    fn outbox_state_machine_and_locking() {
        let (s, acct, _) = setup();
        let rcpts = vec!["a@x.com".to_string(), "b@x.com".to_string()];
        let new = |now| NewOutbox { account_id: &acct, message_id: "<m@x>", from: "f@x.com", rcpts: &rcpts, subject: "hi", raw: b"RAW", now };
        let it = s.outbox_insert(&new(100)).unwrap();
        assert_eq!((it.state.as_str(), it.attempts, it.rcpts.len(), it.size), ("queued", 0, 2, 3));
        assert_eq!(s.outbox_get(&it.id[..8]).unwrap().id, it.id, "unambiguous prefix works");
        assert!(s.outbox_due(99, None).unwrap().is_empty(), "not due before next_attempt_at");
        assert_eq!(s.outbox_due(100, Some(&acct)).unwrap().len(), 1);
        assert!(s.outbox_due(100, Some("other")).unwrap().is_empty());

        assert!(s.outbox_lock(&it.id, 100).unwrap());
        assert!(!s.outbox_lock(&it.id, 100).unwrap(), "a second claimant loses");
        assert!(s.outbox_due(1000, None).unwrap().is_empty(), "a locked item is not due");

        // Temporary failure schedules a retry and keeps the bytes.
        s.outbox_mark_retry(&it.id, 160, "451 try later").unwrap();
        let r = s.outbox_get(&it.id).unwrap();
        assert_eq!((r.state.as_str(), r.attempts, r.next_attempt_at, r.last_error.as_deref()), ("queued", 1, 160, Some("451 try later")));
        assert_eq!(s.outbox_raw(&it.id).unwrap(), b"RAW");

        // Permanent failure keeps the bytes for a manual retry; requeue resets the budget.
        s.outbox_lock(&it.id, 160).unwrap();
        s.outbox_mark_failed(&it.id, "550 no such user").unwrap();
        assert_eq!(s.outbox_get(&it.id).unwrap().state, "failed");
        assert!(s.outbox_requeue(&it.id, 500).unwrap());
        let r = s.outbox_get(&it.id).unwrap();
        assert_eq!((r.state.as_str(), r.attempts, r.next_attempt_at), ("queued", 0, 500));
        assert!(!s.outbox_requeue(&it.id, 500).unwrap(), "only failed items can be requeued");

        // Success drops the bytes but keeps the record.
        s.outbox_lock(&it.id, 500).unwrap();
        s.outbox_mark_sent(&it.id, false).unwrap();
        let r = s.outbox_get(&it.id).unwrap();
        assert_eq!((r.state.as_str(), r.attempts), ("sent", 1));
        assert!(r.sent_at.is_some() && r.last_error.is_none());
        assert!(s.outbox_raw(&it.id).unwrap().is_empty());
        assert!(!s.outbox_cancel(&it.id).unwrap(), "a sent item cannot be cancelled");
        assert!(s.outbox_list(None, false, 10).unwrap().is_empty(), "done items are hidden by default");
        assert_eq!(s.outbox_list(None, true, 10).unwrap().len(), 1);

        // Crash recovery only touches stale locks.
        let b = s.outbox_insert(&new(1000)).unwrap();
        s.outbox_lock(&b.id, 1000).unwrap();
        assert_eq!(s.outbox_recover_stuck(1100, 300).unwrap(), 0, "fresh lock is left alone");
        assert_eq!(s.outbox_recover_stuck(1400, 300).unwrap(), 1);
        assert_eq!(s.outbox_get(&b.id).unwrap().state, "queued");
        assert!(s.outbox_cancel(&b.id).unwrap());
        assert!(s.outbox_raw(&b.id).unwrap().is_empty());
    }

    #[test]
    fn fts_query_quotes_every_word() {
        assert_eq!(fts_query("hello wor").unwrap(), "\"hello\" \"wor\"*");
        assert_eq!(fts_query("  a\"b  ").unwrap(), "\"ab\"*");
        assert_eq!(fts_query("   "), None);
    }

    #[test]
    fn migration_v9_folds_old_calendar_events_into_the_personal_calendar() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        for m in [MIGRATION_V1, MIGRATION_V2, MIGRATION_V3, MIGRATION_V4, MIGRATION_V5, MIGRATION_V6, MIGRATION_V7, MIGRATION_V8] {
            conn.execute_batch(m).unwrap();
        }
        conn.execute_batch("PRAGMA user_version = 8").unwrap();
        conn.execute(
            "INSERT INTO calendar_events (title, start_utc, end_utc, all_day, tz, location, attendees, rrule, ics_uid, source_proposal_id, source_message_id) VALUES
             ('Dinner', '2026-10-29T19:30:00Z', '2026-10-29T21:00:00Z', 0, 'America/Chicago', 'Chez Louis', '[\"a@example.com\"]', NULL, 'inv-1', NULL, NULL),
             ('No end', '2026-10-30T15:00:00Z', NULL, 0, NULL, NULL, '[]', NULL, NULL, NULL, NULL),
             ('Party', '2026-10-31T05:00:00Z', NULL, 1, 'America/Chicago', NULL, '[]', NULL, 'p-1', NULL, NULL)",
            [],
        )
        .unwrap();
        let s = Store::init(conn).unwrap();
        let v: i64 = s.conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(v, SCHEMA_VERSION);
        assert_eq!(s.calendars().unwrap().iter().map(|c| c.id.as_str()).collect::<Vec<_>>(), vec!["personal"]);
        assert!(s.conn.query_row("SELECT 1 FROM sqlite_master WHERE name = 'calendar_events'", [], |r| r.get::<_, i64>(0)).is_err(), "the old table is gone");
        let evs = s.list_events(None, 10).unwrap();
        assert_eq!(evs.len(), 3);
        assert_eq!((evs[0].title.as_str(), evs[0].uid.as_str(), evs[0].end_utc.as_str(), evs[0].attendees.clone()), ("Dinner", "inv-1", "2026-10-29T21:00:00Z", vec!["a@example.com".to_string()]));
        assert_eq!(evs[1].end_utc, "2026-10-30T16:00:00Z", "a missing end becomes an hour");
        assert!(evs[1].uid.ends_with("@stead"), "a missing UID is generated");
        assert_eq!(evs[2].end_utc, "2026-11-01T05:00:00Z", "an all-day event without an end lasts one day");
        assert!(evs.iter().all(|e| e.calendar_id == "personal"));
    }
}
