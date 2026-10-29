//! iCalendar (RFC 5545) export and import for the local calendar.

use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use serde::Serialize;

use super::model::{Event, parse_instant, rfc3339};
use crate::ai::ics;

fn esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => o.push_str("\\\\"),
            ';' => o.push_str("\\;"),
            ',' => o.push_str("\\,"),
            '\n' => o.push_str("\\n"),
            '\r' => {}
            c => o.push(c),
        }
    }
    o
}

/// Fold a content line at 75 octets, never inside a character.
fn fold(line: &str) -> String {
    let mut out = String::new();
    let mut len = 0;
    for c in line.chars() {
        let w = c.len_utf8();
        if len + w > 75 {
            out.push_str("\r\n ");
            len = 1;
        }
        out.push(c);
        len += w;
    }
    out.push_str("\r\n");
    out
}

fn utc_stamp(t: DateTime<Utc>) -> String {
    t.format("%Y%m%dT%H%M%SZ").to_string()
}

fn local_stamp(t: DateTime<Utc>, tz: Tz) -> String {
    t.with_timezone(&tz).format("%Y%m%dT%H%M%S").to_string()
}

fn date_stamp(t: DateTime<Utc>, tz: Tz) -> String {
    t.with_timezone(&tz).format("%Y%m%d").to_string()
}

pub fn export(name: &str, events: &[Event]) -> String {
    let mut out = String::new();
    let mut push = |l: String| out.push_str(&fold(&l));
    push("BEGIN:VCALENDAR".into());
    push("VERSION:2.0".into());
    push("PRODID:-//mailcore//AI mail client//EN".into());
    push("CALSCALE:GREGORIAN".into());
    push(format!("X-WR-CALNAME:{}", esc(name)));
    for ev in events {
        let (Ok(start), Ok(end)) = (parse_instant(&ev.start_utc), parse_instant(&ev.end_utc)) else { continue };
        let tz: Tz = ev.tz.as_deref().and_then(|t| t.parse().ok()).unwrap_or(chrono_tz::UTC);
        push("BEGIN:VEVENT".into());
        push(format!("UID:{}", ev.uid));
        push(format!("DTSTAMP:{}", parse_instant(&ev.updated_at).map(utc_stamp).unwrap_or_else(|_| utc_stamp(Utc::now()))));
        push(format!("SEQUENCE:{}", ev.sequence));
        if ev.all_day {
            push(format!("DTSTART;VALUE=DATE:{}", date_stamp(start, tz)));
            push(format!("DTEND;VALUE=DATE:{}", date_stamp(end, tz)));
        } else if ev.rrule.is_some() && tz != chrono_tz::UTC {
            // A series keeps its wall-clock time across Daylight Saving, so it needs its zone.
            push(format!("DTSTART;TZID={}:{}", tz.name(), local_stamp(start, tz)));
            push(format!("DTEND;TZID={}:{}", tz.name(), local_stamp(end, tz)));
        } else {
            push(format!("DTSTART:{}", utc_stamp(start)));
            push(format!("DTEND:{}", utc_stamp(end)));
        }
        if let Some(r) = &ev.rrule {
            push(format!("RRULE:{r}"));
            for ex in ev.exdates.iter().filter_map(|x| parse_instant(x).ok()) {
                if ev.all_day {
                    push(format!("EXDATE;VALUE=DATE:{}", date_stamp(ex, tz)));
                } else if tz != chrono_tz::UTC {
                    push(format!("EXDATE;TZID={}:{}", tz.name(), local_stamp(ex, tz)));
                } else {
                    push(format!("EXDATE:{}", utc_stamp(ex)));
                }
            }
        }
        push(format!("SUMMARY:{}", esc(&ev.title)));
        if let Some(l) = &ev.location {
            push(format!("LOCATION:{}", esc(l)));
        }
        if let Some(d) = &ev.description {
            push(format!("DESCRIPTION:{}", esc(d)));
        }
        push(format!("STATUS:{}", ev.status.to_ascii_uppercase()));
        if let Some(o) = &ev.organizer {
            push(format!("ORGANIZER:mailto:{o}"));
        }
        for a in &ev.attendees {
            push(format!("ATTENDEE:mailto:{a}"));
        }
        push("END:VEVENT".into());
    }
    push("END:VCALENDAR".into());
    out
}

#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct ImportReport {
    pub added: usize,
    pub updated: usize,
    /// Why each skipped item was not imported.
    pub skipped: Vec<String>,
}

pub const MAX_IMPORT_BYTES: usize = 5 * 1024 * 1024;
pub const MAX_IMPORT_EVENTS: usize = 5000;

/// An event parsed from a file, before it is placed in a calendar.
pub struct Imported {
    pub uid: Option<String>,
    pub title: String,
    pub description: Option<String>,
    pub location: Option<String>,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub all_day: bool,
    pub tz: Option<String>,
    pub rrule: Option<String>,
    pub exdates: Vec<String>,
    pub status: String,
    pub organizer: Option<String>,
    pub attendees: Vec<String>,
    pub sequence: i64,
}

pub fn parse_import(text: &str, default_tz: Tz, skipped: &mut Vec<String>) -> Vec<Imported> {
    let mut out = Vec::new();
    for e in ics::parse(text).into_iter().take(MAX_IMPORT_EVENTS) {
        let name = if e.summary.trim().is_empty() { "(untitled)".to_string() } else { e.summary.clone() };
        if e.recurrence_id.is_some() {
            skipped.push(format!("{name}: a changed single occurrence of a series is not supported yet"));
            continue;
        }
        let Some(r) = e.resolve(default_tz) else {
            skipped.push(format!("{name}: no usable start time"));
            continue;
        };
        let end = r.end_utc.unwrap_or(r.start_utc + chrono::Duration::hours(1));
        let tz_name = r.tz.clone().unwrap_or_else(|| default_tz.name().to_string());
        let tz: Tz = tz_name.parse().unwrap_or(default_tz);
        let rrule = match e.rrule.as_deref() {
            Some(rule) => match super::recur::normalize_rule(rule, r.start_utc, tz) {
                Ok(rule) => Some(rule),
                Err(err) => {
                    skipped.push(format!("{name}: {err}"));
                    continue;
                }
            },
            None => None,
        };
        let exdates = e.exdates.iter().filter_map(|x| x.resolve(default_tz)).map(|(t, _, _)| rfc3339(t)).collect();
        let status = match e.status.as_deref() {
            Some("TENTATIVE") => "tentative",
            Some("CANCELLED") => "cancelled",
            _ => "confirmed",
        };
        out.push(Imported {
            uid: e.uid.clone().filter(|u| !u.trim().is_empty()),
            title: name,
            description: e.description.clone(),
            location: e.location.clone(),
            start: r.start_utc,
            end,
            all_day: r.all_day,
            tz: Some(tz_name),
            rrule,
            exdates,
            status: status.into(),
            organizer: e.organizer.clone(),
            attendees: e.attendees.clone(),
            sequence: e.sequence.unwrap_or(0),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono_tz::America::Chicago;

    fn ev(title: &str, start: &str, end: &str, tz: &str, rrule: Option<&str>, all_day: bool) -> Event {
        Event {
            id: "i".into(), calendar_id: "c".into(), uid: "uid-1@test".into(), title: title.into(), description: Some("Line one\nLine, two; three".into()),
            location: Some("Café Zoë, 12 Main St".into()), start_utc: start.into(), end_utc: end.into(), all_day, tz: Some(tz.into()),
            rrule: rrule.map(String::from), exdates: vec![], status: "confirmed".into(), organizer: Some("me@example.com".into()),
            attendees: vec!["a@example.com".into(), "b@example.com".into()], source_message_id: None, sequence: 2, updated_at: "2026-10-28T12:00:00Z".into(),
        }
    }

    fn roundtrip(e: &Event) -> Imported {
        let text = export("Test", std::slice::from_ref(e));
        let mut skipped = vec![];
        let mut v = parse_import(&text, Chicago, &mut skipped);
        assert!(skipped.is_empty(), "{skipped:?}");
        assert_eq!(v.len(), 1);
        v.remove(0)
    }

    #[test]
    fn a_timed_event_round_trips_with_escaping_and_folding() {
        let long = "A very long title, with commas; semicolons and \u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9} accents that must fold across several physical lines without splitting a character";
        let e = ev(long, "2026-10-29T19:30:00Z", "2026-10-29T20:30:00Z", "America/Chicago", None, false);
        let text = export("Test", std::slice::from_ref(&e));
        assert!(text.lines().all(|l| l.len() <= 76), "lines are folded at 75 octets");
        assert!(text.contains("\r\n"));
        let i = roundtrip(&e);
        assert_eq!(i.title, long);
        assert_eq!(i.description.as_deref(), Some("Line one\nLine, two; three"));
        assert_eq!(i.location.as_deref(), Some("Café Zoë, 12 Main St"));
        assert_eq!((rfc3339(i.start).as_str(), rfc3339(i.end).as_str()), ("2026-10-29T19:30:00Z", "2026-10-29T20:30:00Z"));
        assert_eq!((i.uid.as_deref(), i.sequence, i.attendees.len(), i.organizer.as_deref()), (Some("uid-1@test"), 2, 2, Some("me@example.com")));
    }

    #[test]
    fn a_series_keeps_its_zone_exceptions_and_rule() {
        let mut e = ev("Standup", "2026-10-29T14:00:00Z", "2026-10-29T14:30:00Z", "America/Chicago", Some("FREQ=WEEKLY;BYDAY=TH"), false);
        e.exdates = vec!["2026-11-05T15:00:00Z".into()];
        let text = export("Test", std::slice::from_ref(&e));
        assert!(text.contains("DTSTART;TZID=America/Chicago:20261029T090000"), "{text}");
        assert!(text.contains("EXDATE;TZID=America/Chicago:20261105T090000"));
        let i = roundtrip(&e);
        assert_eq!(i.rrule.as_deref(), Some("FREQ=WEEKLY;BYDAY=TH"));
        assert_eq!(i.exdates, vec!["2026-11-05T15:00:00Z"]);
        assert_eq!((rfc3339(i.start).as_str(), i.tz.as_deref()), ("2026-10-29T14:00:00Z", Some("America/Chicago")));
    }

    #[test]
    fn all_day_events_use_dates() {
        let e = ev("Trip", "2026-10-31T05:00:00Z", "2026-11-03T06:00:00Z", "America/Chicago", None, true);
        let text = export("Test", std::slice::from_ref(&e));
        assert!(text.contains("DTSTART;VALUE=DATE:20261031") && text.contains("DTEND;VALUE=DATE:20261103"), "{text}");
        let i = roundtrip(&e);
        assert!(i.all_day);
        assert_eq!((rfc3339(i.start).as_str(), rfc3339(i.end).as_str()), ("2026-10-31T05:00:00Z", "2026-11-03T06:00:00Z"));
    }

    #[test]
    fn unsupported_or_broken_items_are_reported_not_fatal() {
        let text = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:a\r\nSUMMARY:No start\r\nEND:VEVENT\r\n\
BEGIN:VEVENT\r\nUID:b\r\nSUMMARY:Bad rule\r\nDTSTART:20261029T150000Z\r\nRRULE:FREQ=NEVER\r\nEND:VEVENT\r\n\
BEGIN:VEVENT\r\nUID:c\r\nRECURRENCE-ID:20261105T150000Z\r\nSUMMARY:Override\r\nDTSTART:20261105T160000Z\r\nEND:VEVENT\r\n\
BEGIN:VEVENT\r\nUID:d\r\nSUMMARY:Fine\r\nDTSTART:20261029T150000Z\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let mut skipped = vec![];
        let v = parse_import(text, Chicago, &mut skipped);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].title, "Fine");
        assert_eq!(v[0].end - v[0].start, chrono::Duration::hours(1), "no end means an hour");
        assert_eq!(skipped.len(), 3, "{skipped:?}");
    }
}
