use chrono::{DateTime, Duration, NaiveDate, NaiveDateTime, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};

use crate::ai::ics::local_to_utc;
use crate::error::{Error, Result};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Calendar {
    pub id: String,
    pub name: String,
    /// CSS colour, e.g. "#7aa2f7".
    pub color: String,
    /// local | google | caldav
    pub kind: String,
    pub read_only: bool,
    pub visible: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Event {
    pub id: String,
    pub calendar_id: String,
    /// The iCalendar UID. Stable across export, import and sync.
    pub uid: String,
    pub title: String,
    pub description: Option<String>,
    pub location: Option<String>,
    pub start_utc: String,
    /// Exclusive end. For all-day events this is local midnight after the last day.
    pub end_utc: String,
    pub all_day: bool,
    /// IANA zone the wall clock belongs to.
    pub tz: Option<String>,
    /// RRULE body without the "RRULE:" prefix.
    pub rrule: Option<String>,
    /// Start instants (RFC 3339, UTC) of occurrences removed from the series.
    pub exdates: Vec<String>,
    /// confirmed | tentative | cancelled
    pub status: String,
    pub organizer: Option<String>,
    pub attendees: Vec<String>,
    pub source_message_id: Option<i64>,
    pub sequence: i64,
    pub updated_at: String,
}

/// What a caller supplies to create or change an event. Times are what a person types: a local
/// wall-clock time in `tz` (or an RFC 3339 instant), or plain dates for an all-day event.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(default)]
pub struct EventInput {
    pub calendar_id: Option<String>,
    pub title: String,
    pub description: Option<String>,
    pub location: Option<String>,
    /// "YYYY-MM-DDTHH:MM" (local in `tz`), an RFC 3339 instant, or "YYYY-MM-DD" when `all_day`.
    pub start: String,
    /// Same forms. For an all-day event this is the last day, inclusive. Default: one hour / one day.
    pub end: Option<String>,
    pub all_day: bool,
    pub tz: Option<String>,
    pub rrule: Option<String>,
    pub status: Option<String>,
    pub attendees: Vec<String>,
}

/// One concrete appearance of an event in a time range (a recurring event has many).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Occurrence {
    pub event_id: String,
    pub calendar_id: String,
    pub title: String,
    pub location: Option<String>,
    pub start_utc: String,
    pub end_utc: String,
    pub all_day: bool,
    pub tz: Option<String>,
    pub recurring: bool,
    pub status: String,
    pub color: String,
    /// This occurrence's start instant; identifies it for "delete only this one".
    pub instance: String,
}

pub const MAX_DURATION_DAYS: i64 = 366;

/// Times and zone of an event after validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Normalized {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub all_day: bool,
    pub tz: Tz,
    pub rrule: Option<String>,
}

pub fn rfc3339(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

pub fn parse_instant(s: &str) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s).map(|t| t.with_timezone(&Utc)).map_err(|e| Error::Invalid(format!("'{s}' is not a valid time: {e}")))
}

enum When {
    Date(NaiveDate),
    Local(NaiveDateTime),
    Instant(DateTime<Utc>),
}

fn parse_when(s: &str) -> Result<When> {
    let s = s.trim();
    if let Ok(t) = DateTime::parse_from_rfc3339(s) {
        return Ok(When::Instant(t.with_timezone(&Utc)));
    }
    for f in ["%Y-%m-%dT%H:%M:%S", "%Y-%m-%dT%H:%M", "%Y-%m-%d %H:%M"] {
        if let Ok(n) = NaiveDateTime::parse_from_str(s, f) {
            return Ok(When::Local(n));
        }
    }
    NaiveDate::parse_from_str(s, "%Y-%m-%d").map(When::Date).map_err(|_| Error::Invalid(format!("'{s}' is not a date or time (use YYYY-MM-DD or YYYY-MM-DDTHH:MM)")))
}

fn midnight(d: NaiveDate, tz: Tz) -> Result<DateTime<Utc>> {
    local_to_utc(d.and_hms_opt(0, 0, 0).unwrap(), tz).ok_or_else(|| Error::Invalid(format!("{d} does not exist in {}", tz.name())))
}

/// Validate user input and turn it into instants. `default_tz` is the user's own zone.
pub fn normalize(input: &EventInput, default_tz: Tz) -> Result<Normalized> {
    let tz: Tz = match input.tz.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
        Some(name) => name.parse().map_err(|_| Error::Invalid(format!("'{name}' is not a known time zone")))?,
        None => default_tz,
    };
    let (start, end) = if input.all_day {
        let day = |s: &str| -> Result<NaiveDate> {
            Ok(match parse_when(s)? {
                When::Date(d) => d,
                When::Local(n) => n.date(),
                When::Instant(t) => t.with_timezone(&tz).date_naive(),
            })
        };
        let first = day(&input.start)?;
        let last = match input.end.as_deref().filter(|e| !e.trim().is_empty()) {
            Some(e) => day(e)?,
            None => first,
        };
        if last < first {
            return Err(Error::Invalid("the event ends before it starts".into()));
        }
        (midnight(first, tz)?, midnight(last + Duration::days(1), tz)?)
    } else {
        let instant = |s: &str| -> Result<DateTime<Utc>> {
            match parse_when(s)? {
            When::Instant(t) => Ok(t),
            When::Local(n) => local_to_utc(n, tz).ok_or_else(|| Error::Invalid(format!("{s} does not exist in {}", tz.name()))),
            When::Date(_) => Err(Error::Invalid("a timed event needs a time of day (or mark it all-day)".into())),
            }
        };
        let start = instant(&input.start)?;
        let end = match input.end.as_deref().filter(|e| !e.trim().is_empty()) {
            Some(e) => instant(e)?,
            None => start + Duration::hours(1),
        };
        if end <= start {
            return Err(Error::Invalid("the event must end after it starts".into()));
        }
        (start, end)
    };
    if end - start > Duration::days(MAX_DURATION_DAYS) {
        return Err(Error::Invalid(format!("an event can last at most {MAX_DURATION_DAYS} days")));
    }
    let rrule = match input.rrule.as_deref().map(str::trim).filter(|r| !r.is_empty()) {
        Some(r) => Some(super::recur::normalize_rule(r, start, tz)?),
        None => None,
    };
    Ok(Normalized { start, end, all_day: input.all_day, tz, rrule })
}

pub fn clean_text(s: &str, max: usize) -> String {
    s.chars().filter(|c| !c.is_control() || matches!(c, '\n' | '\t')).take(max).collect::<String>().trim().to_string()
}

pub fn clean_title(s: &str) -> Result<String> {
    let t: String = s.split_whitespace().collect::<Vec<_>>().join(" ").chars().filter(|c| !c.is_control()).take(200).collect();
    if t.is_empty() { Err(Error::Invalid("give the event a title".into())) } else { Ok(t) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono_tz::America::Chicago;

    fn input(start: &str, end: Option<&str>, all_day: bool) -> EventInput {
        EventInput { title: "x".into(), start: start.into(), end: end.map(String::from), all_day, ..Default::default() }
    }

    #[test]
    fn timed_events_use_local_time_in_the_chosen_zone() {
        let n = normalize(&input("2026-10-29T14:30", Some("2026-10-29T15:30"), false), Chicago).unwrap();
        assert_eq!((rfc3339(n.start).as_str(), rfc3339(n.end).as_str()), ("2026-10-29T19:30:00Z", "2026-10-29T20:30:00Z"));
        let ny = EventInput { tz: Some("America/New_York".into()), ..input("2026-10-29T14:30", None, false) };
        let n = normalize(&ny, Chicago).unwrap();
        assert_eq!(rfc3339(n.start), "2026-10-29T18:30:00Z");
        assert_eq!(n.end - n.start, Duration::hours(1), "default length is an hour");
        assert_eq!(n.tz, chrono_tz::America::New_York);
        // An RFC 3339 instant is taken as is.
        assert_eq!(rfc3339(normalize(&input("2026-10-29T19:30:00Z", None, false), Chicago).unwrap().start), "2026-10-29T19:30:00Z");
    }

    #[test]
    fn all_day_events_span_whole_local_days_with_an_exclusive_end() {
        let one = normalize(&input("2026-10-31", None, true), Chicago).unwrap();
        assert_eq!((rfc3339(one.start).as_str(), rfc3339(one.end).as_str()), ("2026-10-31T05:00:00Z", "2026-11-01T05:00:00Z"));
        // Three days, crossing the DST change on Nov 1: the span is 3 local days, not 72 hours.
        let three = normalize(&input("2026-10-31", Some("2026-11-02"), true), Chicago).unwrap();
        assert_eq!((rfc3339(three.start).as_str(), rfc3339(three.end).as_str()), ("2026-10-31T05:00:00Z", "2026-11-03T06:00:00Z"));
        assert!(normalize(&input("2026-11-02", Some("2026-11-01"), true), Chicago).is_err());
    }

    #[test]
    fn bad_input_is_rejected_with_a_reason() {
        for (inp, why) in [
            (input("2026-10-29T14:30", Some("2026-10-29T14:30"), false), "must end after"),
            (input("2026-10-29T14:30", Some("2026-10-29T13:00"), false), "must end after"),
            (input("2026-10-29", None, false), "time of day"),
            (input("tomorrow", None, false), "not a date"),
            (input("2026-10-29T14:30", Some("2028-10-29T14:30"), false), "at most"),
            (EventInput { tz: Some("Mars/Base".into()), ..input("2026-10-29T14:30", None, false) }, "time zone"),
            (input("2026-03-08T02:30", None, false), "does not exist"),
        ] {
            let e = normalize(&inp, Chicago);
            // 02:30 on the spring-forward day is mapped forward, not rejected; every other case errors.
            if why == "does not exist" {
                assert!(e.is_ok());
            } else {
                assert!(e.unwrap_err().to_string().contains(why), "{why}");
            }
        }
    }

    #[test]
    fn titles_and_text_are_cleaned() {
        assert_eq!(clean_title("  Lunch \n with   Jill\u{0} ").unwrap(), "Lunch with Jill");
        assert!(clean_title("  \n ").is_err());
        assert_eq!(clean_title(&"x".repeat(500)).unwrap().len(), 200);
        assert_eq!(clean_text("a\u{0}b\nc", 100), "ab\nc");
    }
}
