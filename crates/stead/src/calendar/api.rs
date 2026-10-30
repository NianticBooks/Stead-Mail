//! Calendar operations on `Api`. The CLI, the daemon and the GUI all call these.

use chrono::{DateTime, Duration, Utc};
use chrono_tz::Tz;
use serde::Deserialize;

use super::ical::{self, ImportReport};
use super::model::{self, Calendar, Event, EventInput, Occurrence, clean_text, clean_title, normalize, parse_instant, rfc3339};
use super::{db, recur};
use crate::api::Api;
use crate::error::{Error, Result};

/// The widest window one query may ask for.
pub const MAX_RANGE_DAYS: i64 = 400;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct CalendarPatch {
    pub name: Option<String>,
    pub color: Option<String>,
    pub visible: Option<bool>,
}

impl Api {
    /// The zone for typed times: the configured one, else the system's, else UTC.
    pub fn calendar_tz(&self) -> Tz {
        match self.ai_config() {
            Ok(c) => c.tz(),
            Err(_) => iana_time_zone::get_timezone().ok().and_then(|n| n.parse().ok()).unwrap_or(chrono_tz::UTC),
        }
    }

    pub fn calendars(&self) -> Result<Vec<Calendar>> {
        self.store().calendars()
    }

    pub fn calendar_create(&self, name: &str, color: &str) -> Result<Calendar> {
        self.store().create_calendar(name, color)
    }

    pub fn calendar_update(&self, id: &str, patch: CalendarPatch) -> Result<Calendar> {
        self.store().update_calendar(id, patch.name.as_deref(), patch.color.as_deref(), patch.visible)
    }

    pub fn calendar_delete(&self, id: &str) -> Result<()> {
        self.store().delete_calendar(id)
    }

    pub fn event_get(&self, id: &str) -> Result<Event> {
        self.store().event(id)
    }

    /// All events, soonest first (series appear once, at their first occurrence).
    pub fn event_list(&self, calendar: Option<&str>, limit: usize) -> Result<Vec<Event>> {
        self.store().list_events(calendar, limit)
    }

    pub fn event_create(&self, input: EventInput) -> Result<Event> {
        let tz = self.calendar_tz();
        let store = self.store();
        let ev = build_event(&store.conn, &input, None, tz)?;
        db::put_event(&store.conn, &ev, None)?;
        db::event(&store.conn, &ev.id)
    }

    pub fn event_update(&self, id: &str, input: EventInput) -> Result<Event> {
        let tz = self.calendar_tz();
        let store = self.store();
        let current = db::event(&store.conn, id)?;
        check_writable(&store.conn, &current.calendar_id)?;
        if let Some(target) = input.calendar_id.as_deref().filter(|c| *c != current.calendar_id) {
            let synced = |cal: &str| db::calendar(&store.conn, cal).map(|c| c.kind != "local");
            if synced(target)? || synced(&current.calendar_id)? {
                return Err(Error::Invalid("events cannot be moved to or from a Google or iCloud calendar yet; copy it instead".into()));
            }
        }
        let ev = build_event(&store.conn, &input, Some(&current), tz)?;
        db::put_event(&store.conn, &ev, None)?;
        db::event(&store.conn, id)
    }

    /// Delete an event, or only one occurrence of a series when `instance` (the occurrence's start)
    /// is given. Returns the series after the change, or None when it is gone.
    pub fn event_delete(&self, id: &str, instance: Option<&str>) -> Result<Option<Event>> {
        let store = self.store();
        let mut ev = db::event(&store.conn, id)?;
        check_writable(&store.conn, &ev.calendar_id)?;
        let Some(instance) = instance else {
            db::delete_event(&store.conn, id)?;
            return Ok(None);
        };
        if ev.rrule.is_none() {
            return Err(Error::Invalid("this event does not repeat".into()));
        }
        let at = parse_instant(instance)?;
        if ev.exdates.iter().any(|x| parse_instant(x).is_ok_and(|t| t == at)) {
            return Ok(Some(ev));
        }
        let found = recur::expand(&ev, at, at + Duration::seconds(1))?.iter().any(|(s, _)| *s == at);
        if !found {
            return Err(Error::Invalid("that occurrence is not part of this series".into()));
        }
        ev.exdates.push(rfc3339(at));
        ev.exdates.sort();
        ev.exdates.dedup();
        ev.sequence += 1;
        db::put_event(&store.conn, &ev, None)?;
        Ok(Some(db::event(&store.conn, id)?))
    }

    /// Every occurrence in `[from, to)`, soonest first. Hidden calendars are left out unless named.
    pub fn calendar_range(&self, from: &str, to: &str, calendars: Option<&[String]>) -> Result<Vec<Occurrence>> {
        let (from, to) = (parse_instant(from)?, parse_instant(to)?);
        occurrences_in(&self.store().conn, from, to, calendars, None)
    }

    /// Events that overlap the given time, for warning before something is added.
    pub fn calendar_conflicts(&self, start: &str, end: Option<&str>, exclude_event: Option<&str>) -> Result<Vec<Occurrence>> {
        let start = parse_instant(start)?;
        let end = end.and_then(|e| parse_instant(e).ok()).filter(|e| *e > start).unwrap_or(start + Duration::hours(1));
        let all: Vec<Occurrence> = occurrences_in(&self.store().conn, start, end, None, exclude_event)?;
        // A day-long banner (birthday, holiday) is not a scheduling conflict.
        Ok(all.into_iter().filter(|o| !o.all_day && o.status != "cancelled").collect())
    }

    pub fn calendar_export(&self, id: &str) -> Result<String> {
        let store = self.store();
        let cal = db::calendar(&store.conn, id)?;
        let events = db::all_events(&store.conn, Some(id), 100_000)?;
        Ok(ical::export(&cal.name, &events))
    }

    /// Add the events of an .ics file to a calendar. An event with a UID already in the calendar
    /// updates that event instead of duplicating it.
    pub fn calendar_import(&self, id: &str, text: &str) -> Result<ImportReport> {
        if text.len() > ical::MAX_IMPORT_BYTES {
            return Err(Error::Invalid("that file is too large to import".into()));
        }
        let tz = self.calendar_tz();
        let mut report = ImportReport::default();
        let items = ical::parse_import(text, tz, &mut report.skipped);
        let mut store = self.store();
        check_writable(&store.conn, id)?;
        let tx = store.conn.transaction()?;
        for it in items {
            let uid = it.uid.clone().unwrap_or_else(|| format!("{}@stead", uuid::Uuid::new_v4()));
            let existing = db::event_by_uid(&tx, id, &uid)?;
            let ev = Event {
                id: existing.as_ref().map(|e| e.id.clone()).unwrap_or_else(|| uuid::Uuid::new_v4().simple().to_string()),
                calendar_id: id.to_string(),
                uid,
                title: match clean_title(&it.title) {
                    Ok(t) => t,
                    Err(_) => {
                        report.skipped.push("an event without a title".into());
                        continue;
                    }
                },
                description: it.description.as_deref().map(|d| clean_text(d, 5000)).filter(|d| !d.is_empty()),
                location: it.location.as_deref().map(|d| clean_text(d, 300)).filter(|d| !d.is_empty()),
                start_utc: rfc3339(it.start),
                end_utc: rfc3339(it.end),
                all_day: it.all_day,
                tz: it.tz,
                rrule: it.rrule,
                exdates: it.exdates,
                status: it.status,
                organizer: it.organizer,
                attendees: it.attendees.into_iter().take(50).collect(),
                source_message_id: existing.as_ref().and_then(|e| e.source_message_id),
                sequence: it.sequence.max(existing.as_ref().map(|e| e.sequence + 1).unwrap_or(0)),
                updated_at: String::new(),
            };
            if ev.end_utc <= ev.start_utc || it.end - it.start > Duration::days(model::MAX_DURATION_DAYS) {
                report.skipped.push(format!("{}: the times are not valid", ev.title));
                continue;
            }
            db::put_event(&tx, &ev, None)?;
            if existing.is_some() { report.updated += 1 } else { report.added += 1 }
        }
        tx.commit()?;
        Ok(report)
    }
}

fn check_writable(conn: &rusqlite::Connection, calendar_id: &str) -> Result<()> {
    if db::calendar(conn, calendar_id)?.read_only {
        return Err(Error::Invalid("that calendar is read-only".into()));
    }
    Ok(())
}

/// Validate input and build the event to store. `current` carries identity and sync state on edit.
fn build_event(conn: &rusqlite::Connection, input: &EventInput, current: Option<&Event>, default_tz: Tz) -> Result<Event> {
    let title = clean_title(&input.title)?;
    let n = normalize(input, default_tz)?;
    let calendar_id = match (&input.calendar_id, current) {
        (Some(c), _) => c.clone(),
        (None, Some(cur)) => cur.calendar_id.clone(),
        (None, None) => db::default_calendar_id(conn)?,
    };
    check_writable(conn, &calendar_id)?;
    let status = match input.status.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        None => current.map(|c| c.status.clone()).unwrap_or_else(|| "confirmed".into()),
        Some(s @ ("confirmed" | "tentative" | "cancelled")) => s.to_string(),
        Some(other) => return Err(Error::Invalid(format!("'{other}' is not a status (confirmed, tentative or cancelled)"))),
    };
    let mut attendees: Vec<String> = Vec::new();
    for a in &input.attendees {
        let a = a.trim().trim_start_matches("mailto:").to_string();
        if a.is_empty() {
            continue;
        }
        if a.len() > 200 || !a.contains('@') || a.chars().any(|c| c.is_whitespace() || c.is_control()) {
            return Err(Error::Invalid(format!("'{a}' is not an email address")));
        }
        if !attendees.iter().any(|x| x.eq_ignore_ascii_case(&a)) {
            attendees.push(a);
        }
    }
    if attendees.len() > 50 {
        return Err(Error::Invalid("an event can have at most 50 attendees".into()));
    }
    let (start_utc, end_utc) = (rfc3339(n.start), rfc3339(n.end));
    // Moving a series or changing its rule makes the old exceptions meaningless.
    let same_shape = current.is_some_and(|c| c.start_utc == start_utc && c.rrule == n.rrule && c.tz.as_deref() == Some(n.tz.name()));
    Ok(Event {
        id: current.map(|c| c.id.clone()).unwrap_or_else(|| uuid::Uuid::new_v4().simple().to_string()),
        calendar_id,
        uid: current.map(|c| c.uid.clone()).unwrap_or_else(|| format!("{}@stead", uuid::Uuid::new_v4())),
        title,
        description: input.description.as_deref().map(|d| clean_text(d, 5000)).filter(|d| !d.is_empty()),
        location: input.location.as_deref().map(|d| clean_text(d, 300).replace('\n', " ")).filter(|d| !d.is_empty()),
        start_utc,
        end_utc,
        all_day: n.all_day,
        tz: Some(n.tz.name().to_string()),
        rrule: n.rrule,
        exdates: if same_shape { current.map(|c| c.exdates.clone()).unwrap_or_default() } else { vec![] },
        status,
        organizer: current.and_then(|c| c.organizer.clone()),
        attendees,
        source_message_id: current.and_then(|c| c.source_message_id),
        sequence: current.map(|c| c.sequence + 1).unwrap_or(0),
        updated_at: String::new(),
    })
}

pub(super) fn occurrences_in(conn: &rusqlite::Connection, from: DateTime<Utc>, to: DateTime<Utc>, calendars: Option<&[String]>, exclude: Option<&str>) -> Result<Vec<Occurrence>> {
    if to <= from {
        return Err(Error::Invalid("the range must end after it starts".into()));
    }
    if to - from > Duration::days(MAX_RANGE_DAYS) {
        return Err(Error::Invalid(format!("ask for at most {MAX_RANGE_DAYS} days at a time")));
    }
    let mut out = Vec::new();
    for (ev, color, visible) in db::events_near(conn, &rfc3339(from), &rfc3339(to))? {
        if exclude == Some(ev.id.as_str()) {
            continue;
        }
        let wanted = match calendars {
            Some(list) => list.contains(&ev.calendar_id),
            None => visible,
        };
        if !wanted {
            continue;
        }
        // One broken series must not hide the rest of the calendar.
        if let Ok(mut o) = recur::occurrences(&ev, &color, from, to) {
            out.append(&mut o);
        }
    }
    out.sort_by(|a, b| (&a.start_utc, &a.title).cmp(&(&b.start_utc, &b.title)));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::AiConfig;
    use crate::secrets::EncryptedFileStore;
    use crate::store::Store;

    fn api() -> Api {
        let dir = std::env::temp_dir().join(format!("stead-cal-{}", uuid::Uuid::new_v4()));
        let secrets = EncryptedFileStore::open_with_passphrase(&dir.join("secrets.enc.json"), "pw").unwrap();
        let api = Api::new(Store::open_in_memory().unwrap(), Box::new(secrets), dir);
        api.ai_set_config(AiConfig { timezone: "America/Chicago".into(), ..api.ai_config().unwrap() }).unwrap();
        api
    }

    fn timed(title: &str, start: &str, end: Option<&str>) -> EventInput {
        EventInput { title: title.into(), start: start.into(), end: end.map(String::from), ..Default::default() }
    }

    fn titles(o: &[Occurrence]) -> Vec<&str> {
        o.iter().map(|x| x.title.as_str()).collect()
    }

    #[test]
    fn create_edit_delete_and_query_a_single_event() {
        let api = api();
        assert_eq!(api.calendars().unwrap().len(), 1, "a Personal calendar exists from the start");
        let ev = api.event_create(EventInput { location: Some("Café".into()), attendees: vec!["a@example.com".into(), "A@example.com".into()], ..timed("  Dentist \n", "2026-10-29T14:30", Some("2026-10-29T15:15")) }).unwrap();
        assert_eq!((ev.title.as_str(), ev.start_utc.as_str(), ev.end_utc.as_str(), ev.calendar_id.as_str()), ("Dentist", "2026-10-29T19:30:00Z", "2026-10-29T20:15:00Z", "personal"));
        assert_eq!((ev.tz.as_deref(), ev.attendees.len(), ev.sequence), (Some("America/Chicago"), 1, 0));
        assert!(ev.uid.ends_with("@stead"));

        let day = api.calendar_range("2026-10-29T00:00:00Z", "2026-10-30T00:00:00Z", None).unwrap();
        assert_eq!(titles(&day), vec!["Dentist"]);
        assert!(api.calendar_range("2026-10-30T00:00:00Z", "2026-10-31T00:00:00Z", None).unwrap().is_empty());

        let moved = api.event_update(&ev.id, timed("Dentist (moved)", "2026-10-30T09:00", None)).unwrap();
        assert_eq!((moved.id.as_str(), moved.uid.as_str(), moved.sequence), (ev.id.as_str(), ev.uid.as_str(), 1), "identity is kept and the sequence grows");
        assert_eq!(moved.start_utc, "2026-10-30T14:00:00Z");
        assert!(api.calendar_range("2026-10-29T00:00:00Z", "2026-10-30T00:00:00Z", None).unwrap().is_empty());

        assert_eq!(api.event_delete(&ev.id, None).unwrap(), None);
        assert!(api.event_get(&ev.id).is_err());
        assert!(api.calendar_range("2026-10-01T00:00:00Z", "2026-12-01T00:00:00Z", None).unwrap().is_empty());
        // Invalid input is refused with a reason and nothing is stored.
        assert!(api.event_create(timed("", "2026-10-29T14:30", None)).is_err());
        assert!(api.event_create(EventInput { attendees: vec!["not an address".into()], ..timed("x", "2026-10-29T14:30", None) }).is_err());
        assert!(api.event_create(EventInput { status: Some("maybe".into()), ..timed("x", "2026-10-29T14:30", None) }).is_err());
        assert!(api.event_list(None, 10).unwrap().is_empty());
    }

    #[test]
    fn a_weekly_series_survives_daylight_saving_and_single_occurrences_can_be_removed() {
        let api = api();
        let series = api.event_create(EventInput { rrule: Some("FREQ=WEEKLY;BYDAY=TH".into()), ..timed("Standup", "2026-10-29T09:00", Some("2026-10-29T09:30")) }).unwrap();
        let occ = api.calendar_range("2026-10-28T00:00:00Z", "2026-11-20T00:00:00Z", None).unwrap();
        assert_eq!(occ.iter().map(|o| o.start_utc.as_str()).collect::<Vec<_>>(), vec!["2026-10-29T14:00:00Z", "2026-11-05T15:00:00Z", "2026-11-12T15:00:00Z", "2026-11-19T15:00:00Z"]);
        assert!(occ.iter().all(|o| o.recurring));

        // Remove only Nov 5; the others stay.
        let after = api.event_delete(&series.id, Some("2026-11-05T15:00:00Z")).unwrap().unwrap();
        assert_eq!(after.exdates, vec!["2026-11-05T15:00:00Z"]);
        let occ = api.calendar_range("2026-10-28T00:00:00Z", "2026-11-20T00:00:00Z", None).unwrap();
        assert_eq!(occ.len(), 3);
        assert!(api.event_delete(&series.id, Some("2026-11-06T15:00:00Z")).is_err(), "not an occurrence");
        assert!(api.event_delete(&series.id, Some("2026-11-05T15:00:00Z")).is_ok(), "removing twice is harmless");
        assert_eq!(api.event_get(&series.id).unwrap().exdates.len(), 1);

        // Renaming keeps the exceptions; moving the series drops them.
        let renamed = api.event_update(&series.id, EventInput { rrule: Some("FREQ=WEEKLY;BYDAY=TH".into()), ..timed("Daily sync", "2026-10-29T09:00", Some("2026-10-29T09:30")) }).unwrap();
        assert_eq!(renamed.exdates.len(), 1);
        let moved = api.event_update(&series.id, EventInput { rrule: Some("FREQ=WEEKLY;BYDAY=TH".into()), ..timed("Daily sync", "2026-10-29T10:00", Some("2026-10-29T10:30")) }).unwrap();
        assert!(moved.exdates.is_empty());
        // Deleting the whole series removes every occurrence.
        api.event_delete(&series.id, None).unwrap();
        assert!(api.calendar_range("2026-10-28T00:00:00Z", "2026-11-20T00:00:00Z", None).unwrap().is_empty());
        assert!(api.event_delete(&series.id, Some("2026-11-05T15:00:00Z")).is_err());
    }

    #[test]
    fn queries_are_bounded() {
        let api = api();
        assert!(api.calendar_range("2026-01-01T00:00:00Z", "2028-01-01T00:00:00Z", None).unwrap_err().to_string().contains("at most"));
        assert!(api.calendar_range("2026-02-01T00:00:00Z", "2026-01-01T00:00:00Z", None).is_err());
        assert!(api.calendar_range("yesterday", "today", None).is_err());
    }

    #[test]
    fn calendars_can_be_created_hidden_and_deleted() {
        let api = api();
        let work = api.calendar_create("Work", "#e0af68").unwrap();
        assert!(api.calendar_create("work", "#e0af68").is_err(), "names are unique regardless of case");
        assert!(api.calendar_create("Bad", "red").is_err());
        assert!(api.calendar_create("  ", "#e0af68").is_err());
        api.event_create(EventInput { calendar_id: Some(work.id.clone()), ..timed("Review", "2026-10-29T10:00", None) }).unwrap();
        api.event_create(timed("Lunch", "2026-10-29T12:00", None)).unwrap();
        let win = ("2026-10-29T00:00:00Z", "2026-10-30T00:00:00Z");
        let all = api.calendar_range(win.0, win.1, None).unwrap();
        assert_eq!(titles(&all), vec!["Review", "Lunch"]);
        assert_eq!(all[0].color, "#e0af68");

        api.calendar_update(&work.id, CalendarPatch { visible: Some(false), ..Default::default() }).unwrap();
        assert_eq!(titles(&api.calendar_range(win.0, win.1, None).unwrap()), vec!["Lunch"], "hidden calendars drop out of views");
        assert_eq!(titles(&api.calendar_range(win.0, win.1, Some(std::slice::from_ref(&work.id))).unwrap()), vec!["Review"], "but can be asked for by name");
        let renamed = api.calendar_update(&work.id, CalendarPatch { name: Some("Office".into()), color: Some("#ff0000".into()), visible: Some(true) }).unwrap();
        assert_eq!((renamed.name.as_str(), renamed.color.as_str(), renamed.visible), ("Office", "#ff0000", true));
        assert!(api.calendar_update(&work.id, CalendarPatch { name: Some("personal".into()), ..Default::default() }).is_err());

        api.calendar_delete(&work.id).unwrap();
        assert_eq!(titles(&api.calendar_range(win.0, win.1, None).unwrap()), vec!["Lunch"]);
        assert!(api.calendar_delete("personal").is_err(), "the last local calendar stays");
        assert!(api.event_create(EventInput { calendar_id: Some(work.id), ..timed("Ghost", "2026-10-29T10:00", None) }).is_err());
    }

    #[test]
    fn conflicts_ignore_all_day_banners_and_the_event_being_edited() {
        let api = api();
        let meeting = api.event_create(timed("Meeting", "2026-10-29T14:00", Some("2026-10-29T15:00"))).unwrap();
        api.event_create(EventInput { all_day: true, ..timed("Holiday", "2026-10-29", None) }).unwrap();
        let c = api.calendar_conflicts("2026-10-29T19:30:00Z", Some("2026-10-29T20:30:00Z"), None).unwrap();
        assert_eq!(titles(&c), vec!["Meeting"], "the all-day banner is not a conflict");
        assert!(api.calendar_conflicts("2026-10-29T20:00:00Z", Some("2026-10-29T21:00:00Z"), None).unwrap().is_empty(), "back to back is fine");
        assert!(api.calendar_conflicts("2026-10-29T19:30:00Z", None, Some(&meeting.id)).unwrap().is_empty(), "an event does not conflict with itself");
    }

    #[test]
    fn export_then_import_moves_events_between_calendars_without_duplicating() {
        let api = api();
        api.event_create(EventInput { rrule: Some("FREQ=WEEKLY;BYDAY=TH".into()), description: Some("Bring notes, snacks; maybe".into()), ..timed("Standup", "2026-10-29T09:00", Some("2026-10-29T09:30")) }).unwrap();
        api.event_create(EventInput { all_day: true, ..timed("Trip", "2026-10-31", Some("2026-11-02")) }).unwrap();
        let text = api.calendar_export("personal").unwrap();
        assert!(text.starts_with("BEGIN:VCALENDAR\r\n") && text.trim_end().ends_with("END:VCALENDAR"));

        let other = api.calendar_create("Copy", "#9ece6a").unwrap();
        let r = api.calendar_import(&other.id, &text).unwrap();
        assert_eq!((r.added, r.updated, r.skipped.len()), (2, 0, 0));
        let again = api.calendar_import(&other.id, &text).unwrap();
        assert_eq!((again.added, again.updated), (0, 2), "the same UIDs update instead of duplicating");
        assert_eq!(api.event_list(Some(&other.id), 10).unwrap().len(), 2);

        let a = api.calendar_range("2026-10-28T00:00:00Z", "2026-11-20T00:00:00Z", Some(&["personal".to_string()])).unwrap();
        let b = api.calendar_range("2026-10-28T00:00:00Z", "2026-11-20T00:00:00Z", Some(std::slice::from_ref(&other.id))).unwrap();
        let key = |o: &[Occurrence]| o.iter().map(|x| (x.title.clone(), x.start_utc.clone(), x.end_utc.clone(), x.all_day)).collect::<Vec<_>>();
        assert_eq!(key(&a), key(&b), "the copy has exactly the same occurrences");
        assert_eq!(api.event_list(Some(&other.id), 10).unwrap().iter().find(|e| e.title == "Standup").unwrap().description.as_deref(), Some("Bring notes, snacks; maybe"));

        assert!(api.calendar_import("nope", &text).is_err());
        assert!(api.calendar_import(&other.id, &"x".repeat(6 * 1024 * 1024)).unwrap_err().to_string().contains("too large"));
    }

    #[test]
    fn importing_a_foreign_file_with_odd_events_reports_what_it_skipped() {
        let api = api();
        let ics = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:one@x\r\nDTSTART;TZID=Central Standard Time:20261029T090000\r\nDURATION:PT45M\r\nSUMMARY:Windows zone\r\nEND:VEVENT\r\n\
BEGIN:VEVENT\r\nUID:two@x\r\nSUMMARY:Broken\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let r = api.calendar_import("personal", ics).unwrap();
        assert_eq!((r.added, r.skipped.len()), (1, 1));
        let ev = &api.event_list(None, 10).unwrap()[0];
        assert_eq!((ev.start_utc.as_str(), ev.end_utc.as_str()), ("2026-10-29T14:00:00Z", "2026-10-29T14:45:00Z"));
    }
}
