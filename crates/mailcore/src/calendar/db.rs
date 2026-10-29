//! SQL for calendars and events. Free functions over a connection so they work inside a transaction.

use rusqlite::{Connection, OptionalExtension, Row, params};

use super::model::{Calendar, Event};
use crate::error::{Error, Result};
use crate::store::Store;

const EVENT_COLS: &str = "id, calendar_id, uid, title, description, location, start_utc, end_utc, all_day, tz, rrule, exdates, status, organizer, attendees, source_message_id, sequence, updated_at";

fn row_to_event(r: &Row<'_>) -> rusqlite::Result<Event> {
    let exdates: String = r.get(11)?;
    let attendees: String = r.get(14)?;
    Ok(Event {
        id: r.get(0)?,
        calendar_id: r.get(1)?,
        uid: r.get(2)?,
        title: r.get(3)?,
        description: r.get(4)?,
        location: r.get(5)?,
        start_utc: r.get(6)?,
        end_utc: r.get(7)?,
        all_day: r.get::<_, i64>(8)? != 0,
        tz: r.get(9)?,
        rrule: r.get(10)?,
        exdates: serde_json::from_str(&exdates).unwrap_or_default(),
        status: r.get(12)?,
        organizer: r.get(13)?,
        attendees: serde_json::from_str(&attendees).unwrap_or_default(),
        source_message_id: r.get(15)?,
        sequence: r.get(16)?,
        updated_at: r.get(17)?,
    })
}

fn row_to_calendar(r: &Row<'_>) -> rusqlite::Result<Calendar> {
    Ok(Calendar { id: r.get(0)?, name: r.get(1)?, color: r.get(2)?, kind: r.get(3)?, read_only: r.get::<_, i64>(4)? != 0, visible: r.get::<_, i64>(5)? != 0 })
}

const CAL_COLS: &str = "id, name, color, kind, read_only, visible";

pub fn calendars(conn: &Connection) -> Result<Vec<Calendar>> {
    let mut stmt = conn.prepare(&format!("SELECT {CAL_COLS} FROM calendars ORDER BY rowid"))?;
    Ok(stmt.query_map([], row_to_calendar)?.collect::<std::result::Result<_, _>>()?)
}

pub fn calendar(conn: &Connection, id: &str) -> Result<Calendar> {
    conn.query_row(&format!("SELECT {CAL_COLS} FROM calendars WHERE id = ?1"), [id], row_to_calendar)
        .optional()?
        .ok_or_else(|| Error::Invalid(format!("no calendar '{id}'")))
}

/// Where new events go: the first calendar that can be written to.
pub fn default_calendar_id(conn: &Connection) -> Result<String> {
    conn.query_row("SELECT id FROM calendars WHERE read_only = 0 ORDER BY (kind = 'local') DESC, rowid LIMIT 1", [], |r| r.get(0))
        .optional()?
        .ok_or_else(|| Error::Invalid("there is no calendar that can be written to".into()))
}

pub fn event(conn: &Connection, id: &str) -> Result<Event> {
    conn.query_row(&format!("SELECT {EVENT_COLS} FROM events WHERE id = ?1 AND deleted = 0"), [id], row_to_event)
        .optional()?
        .ok_or_else(|| Error::Invalid(format!("no event '{id}'")))
}

pub fn event_by_uid(conn: &Connection, calendar_id: &str, uid: &str) -> Result<Option<Event>> {
    Ok(conn.query_row(&format!("SELECT {EVENT_COLS} FROM events WHERE calendar_id = ?1 AND uid = ?2 AND deleted = 0"), params![calendar_id, uid], row_to_event).optional()?)
}

/// Insert or update an event. It is marked dirty so a later sync pushes it.
pub fn put_event(conn: &Connection, ev: &Event, source_proposal: Option<i64>) -> Result<()> {
    conn.execute(
        "INSERT INTO events (id, calendar_id, uid, title, description, location, start_utc, end_utc, all_day, tz, rrule, exdates, status, organizer, attendees, source_proposal_id, source_message_id, sequence, dirty)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,1)
         ON CONFLICT(id) DO UPDATE SET calendar_id=?2, uid=?3, title=?4, description=?5, location=?6, start_utc=?7, end_utc=?8, all_day=?9, tz=?10,
             rrule=?11, exdates=?12, status=?13, organizer=?14, attendees=?15, sequence=?18, dirty=1, updated_at=strftime('%Y-%m-%dT%H:%M:%SZ','now')",
        params![
            ev.id, ev.calendar_id, ev.uid, ev.title, ev.description, ev.location, ev.start_utc, ev.end_utc, ev.all_day as i64, ev.tz, ev.rrule,
            serde_json::to_string(&ev.exdates).unwrap_or_else(|_| "[]".into()), ev.status, ev.organizer,
            serde_json::to_string(&ev.attendees).unwrap_or_else(|_| "[]".into()), source_proposal, ev.source_message_id, ev.sequence
        ],
    )?;
    Ok(())
}

/// Remove an event. In a synced calendar it becomes a tombstone so the deletion can be pushed.
pub fn delete_event(conn: &Connection, id: &str) -> Result<()> {
    let ev = event(conn, id)?;
    let kind: String = conn.query_row("SELECT kind FROM calendars WHERE id = ?1", [&ev.calendar_id], |r| r.get(0))?;
    if kind == "local" {
        conn.execute("DELETE FROM events WHERE id = ?1", [id])?;
    } else {
        conn.execute("UPDATE events SET deleted = 1, dirty = 1, updated_at = strftime('%Y-%m-%dT%H:%M:%SZ','now') WHERE id = ?1", [id])?;
    }
    Ok(())
}

/// Candidate events for a window: single events that overlap it, and every series that has started
/// before it ends (expansion decides which occurrences really fall inside).
pub fn events_near(conn: &Connection, from: &str, to: &str) -> Result<Vec<(Event, String, bool)>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {}, c.color, c.visible FROM events e JOIN calendars c ON c.id = e.calendar_id
         WHERE e.deleted = 0 AND e.start_utc < ?2 AND (e.rrule IS NOT NULL OR e.end_utc > ?1)
         ORDER BY e.start_utc",
        EVENT_COLS.split(", ").map(|c| format!("e.{c}")).collect::<Vec<_>>().join(", ")
    ))?;
    let rows = stmt.query_map(params![from, to], |r| Ok((row_to_event(r)?, r.get::<_, String>(18)?, r.get::<_, i64>(19)? != 0)))?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

pub fn all_events(conn: &Connection, calendar_id: Option<&str>, limit: usize) -> Result<Vec<Event>> {
    let mut stmt = conn.prepare(&format!("SELECT {EVENT_COLS} FROM events WHERE deleted = 0 AND (?1 IS NULL OR calendar_id = ?1) ORDER BY start_utc LIMIT ?2"))?;
    Ok(stmt.query_map(params![calendar_id, limit as i64], row_to_event)?.collect::<std::result::Result<_, _>>()?)
}

impl Store {
    pub fn calendars(&self) -> Result<Vec<Calendar>> {
        calendars(&self.conn)
    }

    pub fn calendar(&self, id: &str) -> Result<Calendar> {
        calendar(&self.conn, id)
    }

    pub fn create_calendar(&self, name: &str, color: &str) -> Result<Calendar> {
        let name = super::model::clean_text(name, 60).replace('\n', " ");
        if name.is_empty() {
            return Err(Error::Invalid("give the calendar a name".into()));
        }
        check_color(color)?;
        if self.conn.query_row("SELECT COUNT(*) FROM calendars WHERE name = ?1 COLLATE NOCASE", [&name], |r| r.get::<_, i64>(0))? > 0 {
            return Err(Error::Invalid(format!("a calendar named '{name}' already exists")));
        }
        let id = uuid::Uuid::new_v4().simple().to_string();
        self.conn.execute("INSERT INTO calendars (id, name, color, kind) VALUES (?1, ?2, ?3, 'local')", params![id, name, color])?;
        self.calendar(&id)
    }

    pub fn update_calendar(&self, id: &str, name: Option<&str>, color: Option<&str>, visible: Option<bool>) -> Result<Calendar> {
        let current = self.calendar(id)?;
        if let Some(n) = name {
            let n = super::model::clean_text(n, 60).replace('\n', " ");
            if n.is_empty() {
                return Err(Error::Invalid("give the calendar a name".into()));
            }
            if self.conn.query_row("SELECT COUNT(*) FROM calendars WHERE name = ?1 COLLATE NOCASE AND id <> ?2", params![n, id], |r| r.get::<_, i64>(0))? > 0 {
                return Err(Error::Invalid(format!("a calendar named '{n}' already exists")));
            }
            self.conn.execute("UPDATE calendars SET name = ?1 WHERE id = ?2", params![n, id])?;
        }
        if let Some(c) = color {
            check_color(c)?;
            self.conn.execute("UPDATE calendars SET color = ?1 WHERE id = ?2", params![c, id])?;
        }
        if let Some(v) = visible {
            self.conn.execute("UPDATE calendars SET visible = ?1 WHERE id = ?2", params![v as i64, id])?;
        }
        let _ = current;
        self.calendar(id)
    }

    /// Delete a local calendar and its events. The last writable calendar cannot be deleted.
    pub fn delete_calendar(&self, id: &str) -> Result<()> {
        let cal = self.calendar(id)?;
        if cal.kind != "local" {
            return Err(Error::Invalid("only local calendars can be deleted here".into()));
        }
        let others: i64 = self.conn.query_row("SELECT COUNT(*) FROM calendars WHERE kind = 'local' AND id <> ?1", [id], |r| r.get(0))?;
        if others == 0 {
            return Err(Error::Invalid("keep at least one local calendar".into()));
        }
        self.conn.execute("DELETE FROM calendars WHERE id = ?1", [id])?;
        Ok(())
    }

    pub fn event(&self, id: &str) -> Result<Event> {
        event(&self.conn, id)
    }

    pub fn list_events(&self, calendar_id: Option<&str>, limit: usize) -> Result<Vec<Event>> {
        all_events(&self.conn, calendar_id, limit)
    }
}

fn check_color(c: &str) -> Result<()> {
    let ok = c.len() == 7 && c.starts_with('#') && c[1..].chars().all(|ch| ch.is_ascii_hexdigit());
    if ok { Ok(()) } else { Err(Error::Invalid("a colour looks like #7aa2f7".into())) }
}
