//! Reminders: "tell me N minutes before". Stored locally per event and never synced.
//!
//! `reminders_poll` is the one entry point a shell needs: call it every so often and show what it
//! returns. It remembers what it already returned, so calling it twice never repeats a reminder.

use chrono::{DateTime, Duration, Utc};
use rusqlite::params;
use serde::{Deserialize, Serialize};

use super::api::occurrences_in;
use super::db;
use super::model::{parse_instant, rfc3339};
use crate::api::Api;
use crate::error::{Error, Result};

/// A reminder that is due is dropped silently once the event started this long ago.
const STALE_AFTER_START_MINUTES: i64 = 5;
/// The longest lead time accepted: one week.
pub const MAX_LEAD_MINUTES: i64 = 7 * 24 * 60;
/// Fired-markers older than this are forgotten.
const KEEP_FIRED_DAYS: i64 = 14;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DueReminder {
    pub event_id: String,
    pub title: String,
    pub location: Option<String>,
    pub start_utc: String,
    pub instance: String,
    pub minutes: i64,
    pub all_day: bool,
}

impl Api {
    /// Minutes-before values set on an event, smallest first.
    pub fn event_reminders(&self, event_id: &str) -> Result<Vec<i64>> {
        let store = self.store();
        db::event(&store.conn, event_id)?;
        let mut stmt = store.conn.prepare("SELECT minutes FROM reminders WHERE event_id = ?1 ORDER BY minutes")?;
        Ok(stmt.query_map([event_id], |r| r.get(0))?.collect::<std::result::Result<_, _>>()?)
    }

    /// Replace an event's reminders. Works on read-only and synced calendars too, since reminders
    /// never leave this machine.
    pub fn event_set_reminders(&self, event_id: &str, minutes: &[i64]) -> Result<Vec<i64>> {
        if let Some(bad) = minutes.iter().find(|m| !(0..=MAX_LEAD_MINUTES).contains(*m)) {
            return Err(Error::Invalid(format!("a reminder of {bad} minutes is out of range (0 to {MAX_LEAD_MINUTES})")));
        }
        {
            let mut store = self.store();
            db::event(&store.conn, event_id)?;
            let tx = store.conn.transaction()?;
            tx.execute("DELETE FROM reminders WHERE event_id = ?1", [event_id])?;
            for m in minutes {
                tx.execute("INSERT OR IGNORE INTO reminders (event_id, minutes) VALUES (?1, ?2)", params![event_id, m])?;
            }
            tx.commit()?;
        }
        self.event_reminders(event_id)
    }

    /// Reminders that have come due at `now` (unix seconds) and were not returned before.
    pub fn reminders_poll_at(&self, now: i64) -> Result<Vec<DueReminder>> {
        let now_t: DateTime<Utc> = DateTime::from_timestamp(now, 0).ok_or_else(|| Error::Invalid("bad time".into()))?;
        let mut store = self.store();
        let longest: Option<i64> = store.conn.query_row("SELECT MAX(minutes) FROM reminders", [], |r| r.get(0))?;
        let Some(longest) = longest else { return Ok(Vec::new()) };

        let from = now_t - Duration::minutes(STALE_AFTER_START_MINUTES);
        let to = now_t + Duration::minutes(longest) + Duration::seconds(1);
        let occs = occurrences_in(&store.conn, from, to, None, None)?;

        let mut due = Vec::new();
        let tx = store.conn.transaction()?;
        for o in occs.iter().filter(|o| o.status != "cancelled") {
            let Ok(start) = parse_instant(&o.start_utc) else { continue };
            if start + Duration::minutes(STALE_AFTER_START_MINUTES) <= now_t {
                continue;
            }
            let mut stmt = tx.prepare("SELECT minutes FROM reminders WHERE event_id = ?1 ORDER BY minutes DESC")?;
            let leads: Vec<i64> = stmt.query_map([&o.event_id], |r| r.get(0))?.collect::<std::result::Result<_, _>>()?;
            drop(stmt);
            for m in leads {
                if start - Duration::minutes(m) > now_t {
                    continue;
                }
                let fresh = tx.execute(
                    "INSERT OR IGNORE INTO reminders_fired (event_id, instance, minutes, fired_at) VALUES (?1, ?2, ?3, ?4)",
                    params![o.event_id, o.instance, m, now],
                )?;
                if fresh == 1 {
                    due.push(DueReminder {
                        event_id: o.event_id.clone(), title: o.title.clone(), location: o.location.clone(),
                        start_utc: rfc3339(start), instance: o.instance.clone(), minutes: m, all_day: o.all_day,
                    });
                }
            }
        }
        tx.execute("DELETE FROM reminders_fired WHERE fired_at < ?1", [now - KEEP_FIRED_DAYS * 86400])?;
        tx.commit()?;
        // When several reminders of one occurrence come due together, only the closest one is worth showing.
        due.sort_by(|a, b| (&a.event_id, &a.instance, a.minutes).cmp(&(&b.event_id, &b.instance, b.minutes)));
        due.dedup_by(|later, first| later.event_id == first.event_id && later.instance == first.instance);
        due.sort_by(|a, b| a.start_utc.cmp(&b.start_utc));
        Ok(due)
    }

    pub fn reminders_poll(&self) -> Result<Vec<DueReminder>> {
        self.reminders_poll_at(Self::now())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::calendar::EventInput;
    use crate::secrets::EncryptedFileStore;
    use crate::store::Store;

    fn api() -> (Api, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("mailcore-remind-{}", uuid::Uuid::new_v4()));
        let secrets = EncryptedFileStore::open_with_passphrase(&dir.join("s.json"), "pw").unwrap();
        (Api::new(Store::open_in_memory().unwrap(), Box::new(secrets), dir.clone()), dir)
    }

    fn ts(s: &str) -> i64 {
        parse_instant(s).unwrap().timestamp()
    }

    fn make(api: &Api, title: &str, start: &str, rrule: Option<&str>) -> String {
        api.event_create(EventInput { title: title.into(), start: start.into(), tz: Some("UTC".into()), rrule: rrule.map(String::from), ..Default::default() }).unwrap().id
    }

    #[test]
    fn fires_once_when_due_and_not_before() {
        let (api, dir) = api();
        let id = make(&api, "Dentist", "2026-10-30T15:00", None);
        assert_eq!(api.event_set_reminders(&id, &[60, 10]).unwrap(), vec![10, 60]);
        assert!(api.reminders_poll_at(ts("2026-10-30T13:59:00Z")).unwrap().is_empty(), "too early");
        let got = api.reminders_poll_at(ts("2026-10-30T14:00:30Z")).unwrap();
        assert_eq!((got.len(), got[0].minutes, got[0].title.as_str()), (1, 60, "Dentist"));
        assert!(api.reminders_poll_at(ts("2026-10-30T14:01:00Z")).unwrap().is_empty(), "never repeats");
        let got = api.reminders_poll_at(ts("2026-10-30T14:50:00Z")).unwrap();
        assert_eq!((got.len(), got[0].minutes), (1, 10));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_late_poll_reports_only_the_closest_lead_and_drops_the_stale() {
        let (api, dir) = api();
        let id = make(&api, "Standup", "2026-10-30T09:00", None);
        api.event_set_reminders(&id, &[60, 10]).unwrap();
        // The app was closed; it opens at 08:55. Both leads are past due: show one, not two.
        let got = api.reminders_poll_at(ts("2026-10-30T08:55:00Z")).unwrap();
        assert_eq!((got.len(), got[0].minutes), (1, 10));
        // Opened long after the start: nothing to say.
        let id2 = make(&api, "Old", "2026-10-30T07:00", None);
        api.event_set_reminders(&id2, &[10]).unwrap();
        assert!(api.reminders_poll_at(ts("2026-10-30T09:30:00Z")).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn each_occurrence_of_a_series_reminds_separately_and_cancelled_ones_stay_quiet() {
        let (api, dir) = api();
        let id = make(&api, "Gym", "2026-10-30T07:00", Some("FREQ=DAILY"));
        api.event_set_reminders(&id, &[15]).unwrap();
        assert_eq!(api.reminders_poll_at(ts("2026-10-30T06:46:00Z")).unwrap().len(), 1);
        assert_eq!(api.reminders_poll_at(ts("2026-10-31T06:46:00Z")).unwrap().len(), 1);
        api.event_delete(&id, Some("2026-11-01T07:00:00Z")).unwrap();
        assert!(api.reminders_poll_at(ts("2026-11-01T06:46:00Z")).unwrap().is_empty(), "a removed occurrence does not remind");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn rejects_bad_values_and_follows_the_event() {
        let (api, dir) = api();
        let id = make(&api, "X", "2026-10-30T09:00", None);
        assert!(api.event_set_reminders(&id, &[-1]).is_err());
        assert!(api.event_set_reminders(&id, &[MAX_LEAD_MINUTES + 1]).is_err());
        assert!(api.event_set_reminders("nope", &[5]).is_err());
        api.event_set_reminders(&id, &[5]).unwrap();
        api.event_delete(&id, None).unwrap();
        assert!(api.event_reminders(&id).is_err());
        assert!(api.reminders_poll_at(ts("2026-10-30T08:56:00Z")).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }
}
