//! SQL for two-way sync: which calendars mirror a remote one, which events changed locally, and
//! the log of what sync did.

use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

use super::db::{EVENT_COLS, row_to_event};
use super::model::Event;
use crate::error::Result;

/// A local calendar that mirrors a Google or CalDAV calendar.
#[derive(Debug, Clone)]
pub struct SyncCal {
    pub id: String,
    pub name: String,
    pub kind: String,
    /// Google identity id, or mail account id for CalDAV.
    pub account_id: String,
    /// Google calendar id, or CalDAV collection URL.
    pub remote_id: String,
    /// Google syncToken, or the CalDAV ctag last seen.
    pub sync_token: Option<String>,
    pub read_only: bool,
}

/// An event row together with its sync bookkeeping.
#[derive(Debug, Clone)]
pub struct SyncEvent {
    pub event: Event,
    pub remote_id: Option<String>,
    pub etag: Option<String>,
    pub dirty: bool,
    pub deleted: bool,
    pub extra: Option<String>,
}

const SYNC_COLS: &str = "remote_id, etag, dirty, deleted, extra";

fn row_to_sync(r: &rusqlite::Row<'_>) -> rusqlite::Result<SyncEvent> {
    Ok(SyncEvent {
        event: row_to_event(r)?,
        remote_id: r.get(18)?,
        etag: r.get(19)?,
        dirty: r.get::<_, i64>(20)? != 0,
        deleted: r.get::<_, i64>(21)? != 0,
        extra: r.get(22)?,
    })
}

fn row_to_cal(r: &rusqlite::Row<'_>) -> rusqlite::Result<SyncCal> {
    Ok(SyncCal { id: r.get(0)?, name: r.get(1)?, kind: r.get(2)?, account_id: r.get(3)?, remote_id: r.get(4)?, sync_token: r.get(5)?, read_only: r.get::<_, i64>(6)? != 0 })
}

const SYNC_CAL_COLS: &str = "id, name, kind, account_id, remote_id, sync_token, read_only";

pub fn sync_calendars(conn: &Connection) -> Result<Vec<SyncCal>> {
    let mut stmt = conn.prepare(&format!("SELECT {SYNC_CAL_COLS} FROM calendars WHERE kind <> 'local' AND account_id IS NOT NULL AND remote_id IS NOT NULL ORDER BY rowid"))?;
    Ok(stmt.query_map([], row_to_cal)?.collect::<std::result::Result<_, _>>()?)
}

pub fn sync_calendar(conn: &Connection, id: &str) -> Result<Option<SyncCal>> {
    Ok(conn
        .query_row(&format!("SELECT {SYNC_CAL_COLS} FROM calendars WHERE id = ?1 AND kind <> 'local' AND account_id IS NOT NULL AND remote_id IS NOT NULL"), [id], row_to_cal)
        .optional()?)
}

pub fn linked(conn: &Connection, kind: &str, account_id: &str, remote_id: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row("SELECT id FROM calendars WHERE kind = ?1 AND account_id = ?2 AND remote_id = ?3", params![kind, account_id, remote_id], |r| r.get(0))
        .optional()?)
}

pub fn set_sync_state(conn: &Connection, id: &str, token: Option<&str>, error: Option<&str>) -> Result<()> {
    conn.execute(
        "UPDATE calendars SET sync_token = COALESCE(?2, sync_token), last_sync_error = ?3,
             last_sync_at = CASE WHEN ?3 IS NULL THEN strftime('%Y-%m-%dT%H:%M:%SZ','now') ELSE last_sync_at END WHERE id = ?1",
        params![id, token, error],
    )?;
    Ok(())
}

pub fn clear_sync_token(conn: &Connection, id: &str) -> Result<()> {
    conn.execute("UPDATE calendars SET sync_token = NULL WHERE id = ?1", [id])?;
    Ok(())
}

/// Every row of a calendar, tombstones included.
pub fn sync_events(conn: &Connection, calendar_id: &str) -> Result<Vec<SyncEvent>> {
    let mut stmt = conn.prepare(&format!("SELECT {EVENT_COLS}, {SYNC_COLS} FROM events WHERE calendar_id = ?1 ORDER BY rowid"))?;
    Ok(stmt.query_map([calendar_id], row_to_sync)?.collect::<std::result::Result<_, _>>()?)
}

pub fn sync_event(conn: &Connection, id: &str) -> Result<Option<SyncEvent>> {
    Ok(conn.query_row(&format!("SELECT {EVENT_COLS}, {SYNC_COLS} FROM events WHERE id = ?1"), [id], row_to_sync).optional()?)
}

pub fn dirty_events(conn: &Connection, calendar_id: &str) -> Result<Vec<SyncEvent>> {
    let mut stmt = conn.prepare(&format!("SELECT {EVENT_COLS}, {SYNC_COLS} FROM events WHERE calendar_id = ?1 AND dirty = 1 ORDER BY rowid"))?;
    Ok(stmt.query_map([calendar_id], row_to_sync)?.collect::<std::result::Result<_, _>>()?)
}

/// Store an event exactly as the server has it: clean, live, with the server's version tag.
pub fn put_remote(conn: &Connection, ev: &Event, remote_id: &str, etag: Option<&str>, extra: Option<&str>) -> Result<()> {
    // A tombstone with the same uid would collide with the live row's unique index.
    conn.execute("DELETE FROM events WHERE calendar_id = ?1 AND uid = ?2 AND deleted = 1 AND id <> ?3", params![ev.calendar_id, ev.uid, ev.id])?;
    conn.execute(
        "INSERT INTO events (id, calendar_id, uid, title, description, location, start_utc, end_utc, all_day, tz, rrule, exdates, status, organizer, attendees, sequence, remote_id, etag, extra, dirty, deleted)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,0,0)
         ON CONFLICT(id) DO UPDATE SET uid=?3, title=?4, description=?5, location=?6, start_utc=?7, end_utc=?8, all_day=?9, tz=?10, rrule=?11,
             exdates=?12, status=?13, organizer=?14, attendees=?15, sequence=?16, remote_id=?17, etag=?18, extra=?19, dirty=0, deleted=0,
             updated_at=strftime('%Y-%m-%dT%H:%M:%SZ','now')",
        params![
            ev.id, ev.calendar_id, ev.uid, ev.title, ev.description, ev.location, ev.start_utc, ev.end_utc, ev.all_day as i64, ev.tz, ev.rrule,
            serde_json::to_string(&ev.exdates).unwrap_or_else(|_| "[]".into()), ev.status, ev.organizer,
            serde_json::to_string(&ev.attendees).unwrap_or_else(|_| "[]".into()), ev.sequence, remote_id, etag, extra
        ],
    )?;
    Ok(())
}

/// Record that a push succeeded. If the event was edited again while the push was in flight it
/// stays dirty so the newer edit is pushed next time.
pub fn mark_pushed(conn: &Connection, pushed: &Event, remote_id: &str, etag: Option<&str>) -> Result<()> {
    conn.execute(
        "UPDATE events SET remote_id = ?2, etag = ?3, dirty = CASE WHEN sequence = ?4 AND updated_at = ?5 THEN 0 ELSE dirty END WHERE id = ?1",
        params![pushed.id, remote_id, etag, pushed.sequence, pushed.updated_at],
    )?;
    Ok(())
}

pub fn hard_delete(conn: &Connection, id: &str) -> Result<()> {
    conn.execute("DELETE FROM events WHERE id = ?1", [id])?;
    Ok(())
}

// ----- the log -----

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SyncLogEntry {
    pub id: i64,
    pub at: String,
    pub calendar_id: Option<String>,
    pub calendar: String,
    /// conflict | deleted | error | info
    pub kind: String,
    pub title: String,
    pub detail: String,
}

const LOG_KEEP: i64 = 500;

pub fn log(conn: &Connection, cal: &SyncCal, kind: &str, title: &str, detail: &str) -> Result<()> {
    conn.execute("INSERT INTO sync_log (calendar_id, calendar_name, kind, title, detail) VALUES (?1,?2,?3,?4,?5)", params![cal.id, cal.name, kind, title, detail])?;
    conn.execute("DELETE FROM sync_log WHERE id <= (SELECT MAX(id) FROM sync_log) - ?1", [LOG_KEEP])?;
    Ok(())
}

pub fn read_log(conn: &Connection, limit: usize) -> Result<Vec<SyncLogEntry>> {
    let mut stmt = conn.prepare("SELECT id, at, calendar_id, calendar_name, kind, title, detail FROM sync_log ORDER BY id DESC LIMIT ?1")?;
    Ok(stmt
        .query_map([limit as i64], |r| {
            Ok(SyncLogEntry { id: r.get(0)?, at: r.get(1)?, calendar_id: r.get(2)?, calendar: r.get(3)?, kind: r.get(4)?, title: r.get(5)?, detail: r.get(6)? })
        })?
        .collect::<std::result::Result<_, _>>()?)
}

pub fn clear_log(conn: &Connection) -> Result<()> {
    conn.execute("DELETE FROM sync_log", [])?;
    Ok(())
}
