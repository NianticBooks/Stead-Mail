//! A small iCalendar (RFC 5545) reader for invitation emails. Deterministic: no model involved.
//! It handles what real senders produce: folded lines, escaped text, UTC / zoned / floating times,
//! all-day dates, DURATION, Windows time zone names, and METHOD (request, cancel, reply).

use chrono::{DateTime, Duration, NaiveDate, NaiveDateTime, TimeZone, Utc};
use chrono_tz::Tz;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IcsTime {
    Utc(DateTime<Utc>),
    Zoned(NaiveDateTime, Tz),
    /// No zone given: interpreted in the user's zone.
    Floating(NaiveDateTime),
    Date(NaiveDate),
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct IcsEvent {
    pub method: Option<String>,
    pub uid: Option<String>,
    pub summary: String,
    pub start: Option<IcsTime>,
    pub end: Option<IcsTime>,
    pub duration: Option<Duration>,
    pub location: Option<String>,
    pub description: Option<String>,
    pub organizer: Option<String>,
    pub attendees: Vec<String>,
    pub rrule: Option<String>,
    pub status: Option<String>,
    /// Occurrences removed from a recurring event.
    pub exdates: Vec<IcsTime>,
    /// Set on an override of one occurrence of a recurring event (not supported by the calendar yet).
    pub recurrence_id: Option<IcsTime>,
    pub sequence: Option<i64>,
}

/// A concrete instant range, ready to store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub start_utc: DateTime<Utc>,
    pub end_utc: Option<DateTime<Utc>>,
    pub all_day: bool,
    /// The zone the sender meant, when they named one.
    pub tz: Option<String>,
}

const WINDOWS_ZONES: &[(&str, &str)] = &[
    ("Eastern Standard Time", "America/New_York"),
    ("US Eastern Standard Time", "America/Indiana/Indianapolis"),
    ("Central Standard Time", "America/Chicago"),
    ("Mountain Standard Time", "America/Denver"),
    ("US Mountain Standard Time", "America/Phoenix"),
    ("Pacific Standard Time", "America/Los_Angeles"),
    ("Alaskan Standard Time", "America/Anchorage"),
    ("Hawaiian Standard Time", "Pacific/Honolulu"),
    ("GMT Standard Time", "Europe/London"),
    ("W. Europe Standard Time", "Europe/Berlin"),
    ("Romance Standard Time", "Europe/Paris"),
    ("Central Europe Standard Time", "Europe/Budapest"),
    ("India Standard Time", "Asia/Kolkata"),
    ("China Standard Time", "Asia/Shanghai"),
    ("Tokyo Standard Time", "Asia/Tokyo"),
    ("AUS Eastern Standard Time", "Australia/Sydney"),
    ("UTC", "UTC"),
];

fn parse_tz(name: &str) -> Option<Tz> {
    let n = name.trim().trim_matches('"');
    n.parse::<Tz>().ok().or_else(|| WINDOWS_ZONES.iter().find(|(w, _)| w.eq_ignore_ascii_case(n)).and_then(|(_, i)| i.parse().ok()))
}

/// Unfold continuation lines and split into logical lines.
pub(crate) fn unfold(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in text.replace("\r\n", "\n").replace('\r', "\n").split('\n') {
        if (line.starts_with(' ') || line.starts_with('\t')) && !out.is_empty() {
            out.last_mut().unwrap().push_str(&line[1..]);
        } else {
            out.push(line.to_string());
        }
    }
    out
}

fn unescape(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    let mut chars = v.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') | Some('N') => out.push('\n'),
                Some(other) => out.push(other),
                None => {}
            }
        } else {
            out.push(c);
        }
    }
    out
}

struct Prop {
    name: String,
    params: Vec<(String, String)>,
    value: String,
}

fn parse_prop(line: &str) -> Option<Prop> {
    // The value starts after the first ':' that is not inside a quoted parameter value.
    let mut in_quote = false;
    let mut colon = None;
    for (i, c) in line.char_indices() {
        match c {
            '"' => in_quote = !in_quote,
            ':' if !in_quote => {
                colon = Some(i);
                break;
            }
            _ => {}
        }
    }
    let colon = colon?;
    let (head, value) = (&line[..colon], &line[colon + 1..]);
    let mut parts = Vec::new();
    let (mut cur, mut q) = (String::new(), false);
    for c in head.chars() {
        match c {
            '"' => {
                q = !q;
                cur.push(c);
            }
            ';' if !q => parts.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    parts.push(cur);
    let name = parts.remove(0).to_ascii_uppercase();
    let params = parts
        .into_iter()
        .filter_map(|p| p.split_once('=').map(|(k, v)| (k.to_ascii_uppercase(), v.trim_matches('"').to_string())))
        .collect();
    Some(Prop { name, params, value: value.to_string() })
}

fn parse_time(p: &Prop) -> Option<IcsTime> {
    let v = p.value.trim();
    let is_date = p.params.iter().any(|(k, val)| k == "VALUE" && val.eq_ignore_ascii_case("DATE")) || (v.len() == 8 && !v.contains('T'));
    if is_date {
        return NaiveDate::parse_from_str(v, "%Y%m%d").ok().map(IcsTime::Date);
    }
    if let Some(utc) = v.strip_suffix('Z') {
        let n = NaiveDateTime::parse_from_str(utc, "%Y%m%dT%H%M%S").ok()?;
        return Some(IcsTime::Utc(Utc.from_utc_datetime(&n)));
    }
    let n = NaiveDateTime::parse_from_str(v, "%Y%m%dT%H%M%S").ok()?;
    match p.params.iter().find(|(k, _)| k == "TZID").and_then(|(_, z)| parse_tz(z)) {
        Some(tz) => Some(IcsTime::Zoned(n, tz)),
        None => Some(IcsTime::Floating(n)),
    }
}

fn parse_duration(v: &str) -> Option<Duration> {
    let v = v.trim();
    let (neg, rest) = match v.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, v.strip_prefix('+').unwrap_or(v)),
    };
    let rest = rest.strip_prefix('P')?;
    let (date_part, time_part) = rest.split_once('T').unwrap_or((rest, ""));
    let mut secs = 0i64;
    let mut num = String::new();
    for c in date_part.chars() {
        if c.is_ascii_digit() {
            num.push(c);
        } else {
            let n: i64 = num.parse().ok()?;
            num.clear();
            secs += n * match c {
                'W' => 7 * 86400,
                'D' => 86400,
                _ => return None,
            };
        }
    }
    for c in time_part.chars() {
        if c.is_ascii_digit() {
            num.push(c);
        } else {
            let n: i64 = num.parse().ok()?;
            num.clear();
            secs += n * match c {
                'H' => 3600,
                'M' => 60,
                'S' => 1,
                _ => return None,
            };
        }
    }
    if secs == 0 && !v.contains('0') {
        return None;
    }
    Some(Duration::seconds(if neg { -secs } else { secs }))
}

fn mailto(v: &str) -> String {
    v.trim().strip_prefix("mailto:").or_else(|| v.trim().strip_prefix("MAILTO:")).unwrap_or(v.trim()).to_string()
}

/// Parse every VEVENT in `text`. Malformed events are skipped rather than failing the message.
pub fn parse(text: &str) -> Vec<IcsEvent> {
    let mut events = Vec::new();
    let mut method: Option<String> = None;
    let mut current: Option<IcsEvent> = None;
    let mut alarm_depth = 0;
    for line in unfold(text) {
        let Some(p) = parse_prop(&line) else {
            continue;
        };
        match (p.name.as_str(), p.value.to_ascii_uppercase().as_str()) {
            ("BEGIN", "VEVENT") => current = Some(IcsEvent { method: method.clone(), ..Default::default() }),
            ("END", "VEVENT") => {
                if let Some(e) = current.take() {
                    events.push(e);
                }
            }
            ("BEGIN", "VALARM") => alarm_depth += 1,
            ("END", "VALARM") => alarm_depth = (alarm_depth - 1).max(0),
            _ if alarm_depth > 0 => {}
            ("METHOD", _) if current.is_none() => method = Some(p.value.trim().to_ascii_uppercase()),
            _ => {
                let Some(e) = current.as_mut() else { continue };
                match p.name.as_str() {
                    "UID" => e.uid = Some(p.value.trim().to_string()),
                    "SUMMARY" => e.summary = unescape(&p.value),
                    "DTSTART" => e.start = parse_time(&p),
                    "DTEND" => e.end = parse_time(&p),
                    "DURATION" => e.duration = parse_duration(&p.value),
                    "LOCATION" => e.location = Some(unescape(&p.value)).filter(|s| !s.trim().is_empty()),
                    "DESCRIPTION" => e.description = Some(unescape(&p.value)).filter(|s| !s.trim().is_empty()),
                    "ORGANIZER" => e.organizer = Some(mailto(&p.value)),
                    "ATTENDEE" => e.attendees.push(mailto(&p.value)),
                    "RRULE" => e.rrule = Some(p.value.trim().to_string()),
                    "STATUS" => e.status = Some(p.value.trim().to_ascii_uppercase()),
                    "SEQUENCE" => e.sequence = p.value.trim().parse().ok(),
                    "RECURRENCE-ID" => e.recurrence_id = parse_time(&p),
                    // One EXDATE line may list several instants separated by commas.
                    "EXDATE" => {
                        for part in p.value.split(',') {
                            let single = Prop { name: p.name.clone(), params: p.params.clone(), value: part.to_string() };
                            e.exdates.extend(parse_time(&single));
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    events
}

fn to_utc(n: NaiveDateTime, tz: Tz) -> Option<DateTime<Utc>> {
    match tz.from_local_datetime(&n) {
        chrono::LocalResult::Single(t) => Some(t.with_timezone(&Utc)),
        // Fall-back hour: take the first occurrence.
        chrono::LocalResult::Ambiguous(first, _) => Some(first.with_timezone(&Utc)),
        // Spring-forward gap: the wall time does not exist, so use the hour after it.
        chrono::LocalResult::None => tz.from_local_datetime(&(n + Duration::hours(1))).earliest().map(|t| t.with_timezone(&Utc)),
    }
}

/// Convert a naive local time to UTC in `tz` (exposed for the extraction code).
pub fn local_to_utc(n: NaiveDateTime, tz: Tz) -> Option<DateTime<Utc>> {
    to_utc(n, tz)
}

impl IcsTime {
    pub fn resolve(&self, default_tz: Tz) -> Option<(DateTime<Utc>, bool, Option<String>)> {
        match self {
            IcsTime::Utc(t) => Some((*t, false, Some("UTC".into()))),
            IcsTime::Zoned(n, tz) => to_utc(*n, *tz).map(|t| (t, false, Some(tz.name().to_string()))),
            IcsTime::Floating(n) => to_utc(*n, default_tz).map(|t| (t, false, None)),
            // All-day events are anchored at local midnight of the user's zone.
            IcsTime::Date(d) => to_utc(d.and_hms_opt(0, 0, 0)?, default_tz).map(|t| (t, true, None)),
        }
    }
}

impl IcsEvent {
    /// Concrete UTC times. An all-day event without an end lasts one day; a timed event without an
    /// end or duration has no end.
    pub fn resolve(&self, default_tz: Tz) -> Option<Resolved> {
        let (start_utc, all_day, tz) = self.start.as_ref()?.resolve(default_tz)?;
        let end_utc = match (&self.end, self.duration) {
            (Some(e), _) => e.resolve(default_tz).map(|(t, _, _)| t),
            (None, Some(d)) => Some(start_utc + d),
            (None, None) if all_day => Some(start_utc + Duration::days(1)),
            (None, None) => None,
        }
        .filter(|e| *e > start_utc);
        Some(Resolved { start_utc, end_utc, all_day, tz })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono_tz::America::Chicago;

    const GOOGLE: &str = "BEGIN:VCALENDAR\r\nPRODID:-//Google Inc//Google Calendar 70.9054//EN\r\nVERSION:2.0\r\nMETHOD:REQUEST\r\n\
BEGIN:VEVENT\r\nDTSTART:20261029T193000Z\r\nDTEND:20261029T203000Z\r\nUID:abc123@google.com\r\n\
ATTENDEE;CUTYPE=INDIVIDUAL;ROLE=REQ-PARTICIPANT;PARTSTAT=NEEDS-ACTION;CN=Fred;X-NUM-GUESTS=0:mailto:fred@example.com\r\n\
ORGANIZER;CN=Jill Book:mailto:jill@example.com\r\nSEQUENCE:0\r\n\
SUMMARY:Dinner\\, with the Smiths\r\nLOCATION:Chez Louis\\; 12 Main St\r\nDESCRIPTION:Bring wine.\\nSee you there\r\n\
\x20and a folded continuation\r\nSTATUS:CONFIRMED\r\n\
BEGIN:VALARM\r\nTRIGGER:-PT30M\r\nACTION:DISPLAY\r\nSUMMARY:Alarm text must be ignored\r\nEND:VALARM\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

    #[test]
    fn google_invitation() {
        let ev = parse(GOOGLE);
        assert_eq!(ev.len(), 1);
        let e = &ev[0];
        assert_eq!(e.method.as_deref(), Some("REQUEST"));
        assert_eq!(e.summary, "Dinner, with the Smiths", "escaped comma is unescaped and the alarm's SUMMARY is ignored");
        assert_eq!(e.location.as_deref(), Some("Chez Louis; 12 Main St"));
        assert_eq!(e.description.as_deref(), Some("Bring wine.\nSee you thereand a folded continuation"));
        assert_eq!(e.organizer.as_deref(), Some("jill@example.com"));
        assert_eq!(e.attendees, vec!["fred@example.com"]);
        let r = e.resolve(Chicago).unwrap();
        assert_eq!(r.start_utc.to_rfc3339(), "2026-10-29T19:30:00+00:00");
        assert_eq!(r.end_utc.unwrap().to_rfc3339(), "2026-10-29T20:30:00+00:00");
        assert!(!r.all_day);
    }

    #[test]
    fn outlook_windows_zone_names() {
        let ics = "BEGIN:VCALENDAR\r\nMETHOD:REQUEST\r\nBEGIN:VTIMEZONE\r\nTZID:Central Standard Time\r\nEND:VTIMEZONE\r\n\
BEGIN:VEVENT\r\nDTSTART;TZID=Central Standard Time:20261029T143000\r\nDTEND;TZID=\"Central Standard Time\":20261029T153000\r\nSUMMARY:Sync\r\nEND:VEVENT\r\nEND:VCALENDAR";
        let r = parse(ics)[0].resolve(chrono_tz::UTC).unwrap();
        // 2:30 PM CDT (Oct 29 is before the Nov 1 change) = 19:30 UTC regardless of the default zone.
        assert_eq!(r.start_utc.to_rfc3339(), "2026-10-29T19:30:00+00:00");
        assert_eq!(r.tz.as_deref(), Some("America/Chicago"));
    }

    #[test]
    fn iana_zone_across_a_dst_change() {
        let ics = "BEGIN:VEVENT\r\nDTSTART;TZID=America/Chicago:20261103T150000\r\nSUMMARY:After the change\r\nEND:VEVENT";
        // Nov 3 is after DST ends (Nov 1): CST = UTC-6.
        assert_eq!(parse(ics)[0].resolve(Chicago).unwrap().start_utc.to_rfc3339(), "2026-11-03T21:00:00+00:00");
    }

    #[test]
    fn all_day_floating_and_duration() {
        let ics = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nDTSTART;VALUE=DATE:20261031\r\nSUMMARY:Halloween\r\nEND:VEVENT\r\n\
BEGIN:VEVENT\r\nDTSTART:20261104T090000\r\nDURATION:PT1H30M\r\nSUMMARY:Floating\r\nEND:VEVENT\r\n\
BEGIN:VEVENT\r\nDTSTART:20261105T090000Z\r\nSUMMARY:No end\r\nEND:VEVENT\r\nEND:VCALENDAR";
        let ev = parse(ics);
        let h = ev[0].resolve(Chicago).unwrap();
        assert!(h.all_day);
        assert_eq!(h.start_utc.to_rfc3339(), "2026-10-31T05:00:00+00:00", "local midnight in Chicago (CDT)");
        assert_eq!(h.end_utc.unwrap() - h.start_utc, Duration::days(1));
        let f = ev[1].resolve(Chicago).unwrap();
        assert_eq!(f.start_utc.to_rfc3339(), "2026-11-04T15:00:00+00:00", "floating time uses the user's zone (CST)");
        assert_eq!(f.end_utc.unwrap() - f.start_utc, Duration::minutes(90));
        assert_eq!(ev[2].resolve(Chicago).unwrap().end_utc, None);
    }

    #[test]
    fn cancellation_reply_and_garbage() {
        let cancel = "BEGIN:VCALENDAR\r\nMETHOD:CANCEL\r\nBEGIN:VEVENT\r\nUID:x\r\nDTSTART:20261029T193000Z\r\nSUMMARY:Gone\r\nSTATUS:CANCELLED\r\nEND:VEVENT\r\nEND:VCALENDAR";
        let e = &parse(cancel)[0];
        assert_eq!((e.method.as_deref(), e.status.as_deref()), (Some("CANCEL"), Some("CANCELLED")));
        // Events with unusable times are kept but cannot be resolved; nothing panics on junk.
        assert!(parse("BEGIN:VEVENT\r\nDTSTART:not-a-date\r\nSUMMARY:Bad\r\nEND:VEVENT")[0].resolve(Chicago).is_none());
        assert!(parse("").is_empty());
        assert!(parse("complete nonsense\r\n::::\r\nBEGIN:VEVENT").is_empty(), "an unterminated event is dropped");
        assert!(parse(&"BEGIN:VEVENT\r\n".repeat(3)).is_empty());
    }

    #[test]
    fn recurrence_exceptions_and_sequence_are_read() {
        let ics = "BEGIN:VEVENT\r\nUID:r\r\nSEQUENCE:3\r\nDTSTART;TZID=America/Chicago:20261029T090000\r\nRRULE:FREQ=WEEKLY;BYDAY=TH\r\n\
EXDATE;TZID=America/Chicago:20261105T090000,20261112T090000\r\nSUMMARY:Standup\r\nEND:VEVENT\r\n\
BEGIN:VEVENT\r\nUID:r\r\nRECURRENCE-ID;TZID=America/Chicago:20261119T090000\r\nDTSTART;TZID=America/Chicago:20261119T100000\r\nSUMMARY:Standup (moved)\r\nEND:VEVENT";
        let ev = parse(ics);
        assert_eq!(ev.len(), 2);
        assert_eq!((ev[0].sequence, ev[0].rrule.as_deref(), ev[0].exdates.len()), (Some(3), Some("FREQ=WEEKLY;BYDAY=TH"), 2));
        assert!(ev[0].recurrence_id.is_none());
        assert!(ev[1].recurrence_id.is_some(), "overrides are identified so the importer can skip or handle them");
        let first = ev[0].exdates[0].resolve(Chicago).unwrap();
        assert_eq!(first.0.to_rfc3339(), "2026-11-05T15:00:00+00:00", "9 AM CST");
    }

    #[test]
    fn dst_gap_and_overlap_do_not_fail() {
        // 2:30 AM on Mar 8 2026 does not exist in Chicago (clocks jump 2:00 -> 3:00).
        let gap = NaiveDateTime::parse_from_str("20260308T023000", "%Y%m%dT%H%M%S").unwrap();
        assert!(local_to_utc(gap, Chicago).is_some());
        // 1:30 AM on Nov 1 2026 happens twice; the first one is used.
        let overlap = NaiveDateTime::parse_from_str("20261101T013000", "%Y%m%dT%H%M%S").unwrap();
        assert_eq!(local_to_utc(overlap, Chicago).unwrap().to_rfc3339(), "2026-11-01T06:30:00+00:00");
    }

    #[test]
    fn durations() {
        assert_eq!(parse_duration("PT45M"), Some(Duration::minutes(45)));
        assert_eq!(parse_duration("P1DT2H"), Some(Duration::hours(26)));
        assert_eq!(parse_duration("P1W"), Some(Duration::days(7)));
        assert_eq!(parse_duration("-PT15M"), Some(Duration::minutes(-15)));
        assert_eq!(parse_duration("garbage"), None);
    }
}
