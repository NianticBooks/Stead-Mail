//! Recurrence: validating rules and expanding them over a time window, in the event's own zone.

use chrono::{DateTime, Duration, TimeZone, Utc};
use chrono_tz::Tz;
use rrule::{RRule, RRuleSet, Unvalidated};

use super::model::{Event, Occurrence, parse_instant, rfc3339};
use crate::ai::ics::local_to_utc;
use crate::error::{Error, Result};

/// Hard cap on occurrences produced for one event in one query.
pub const MAX_OCCURRENCES: u16 = 3000;

fn zone(tz: Tz) -> rrule::Tz {
    rrule::Tz::Tz(tz)
}

/// Check a rule and return it in canonical form (no `RRULE:` prefix, upper case). Rules that never
/// end are allowed; expansion is bounded by the window.
pub fn normalize_rule(rule: &str, start: DateTime<Utc>, tz: Tz) -> Result<String> {
    let body = rule.trim().trim_start_matches("RRULE:").trim_start_matches("rrule:").trim();
    if body.is_empty() || body.contains('\n') || body.contains('\r') || body.to_ascii_uppercase().contains("DTSTART") {
        return Err(Error::Invalid("a repeat rule is like FREQ=WEEKLY;BYDAY=TH".into()));
    }
    let upper = body.to_ascii_uppercase();
    let dt_start = zone(tz).from_utc_datetime(&start.naive_utc());
    let parsed: RRule<Unvalidated> = upper.parse().map_err(|e| Error::Invalid(format!("invalid repeat rule: {e}")))?;
    parsed.validate(dt_start).map_err(|e| Error::Invalid(format!("invalid repeat rule: {e}")))?;
    Ok(upper)
}

/// Every appearance of `ev` that overlaps `[from, to)`, as (start, end) instants.
pub fn expand(ev: &Event, from: DateTime<Utc>, to: DateTime<Utc>) -> Result<Vec<(DateTime<Utc>, DateTime<Utc>)>> {
    let start = parse_instant(&ev.start_utc)?;
    let end = parse_instant(&ev.end_utc)?;
    let overlaps = |s: DateTime<Utc>, e: DateTime<Utc>| s < to && e > from;
    let Some(rule) = ev.rrule.as_deref() else {
        return Ok(if overlaps(start, end) { vec![(start, end)] } else { vec![] });
    };
    let tz: Tz = ev.tz.as_deref().and_then(|t| t.parse().ok()).unwrap_or(chrono_tz::UTC);
    let dt_start = zone(tz).from_utc_datetime(&start.naive_utc());
    let parsed: RRule<Unvalidated> = rule.parse().map_err(|e| Error::Invalid(format!("invalid repeat rule: {e}")))?;
    let mut set = RRuleSet::new(dt_start).rrule(parsed.validate(dt_start).map_err(|e| Error::Invalid(format!("invalid repeat rule: {e}")))?);
    for ex in &ev.exdates {
        if let Ok(t) = parse_instant(ex) {
            set = set.exdate(t.with_timezone(&zone(tz)));
        }
    }
    let span = end - start;
    // An occurrence that began before the window can still run into it.
    let after = from - span - Duration::seconds(1);
    set = set.after(zone(tz).from_utc_datetime(&after.naive_utc())).before(zone(tz).from_utc_datetime(&to.naive_utc()));
    let result = set.all(MAX_OCCURRENCES);

    // All-day events last whole local days, so measure the span in days, not seconds.
    let span_days = ev.all_day.then(|| {
        let first = start.with_timezone(&tz).date_naive();
        let last = end.with_timezone(&tz).date_naive();
        (last - first).num_days().max(1)
    });
    let mut out = Vec::new();
    for occ in result.dates {
        let s = occ.with_timezone(&Utc);
        let e = match span_days {
            Some(days) => local_to_utc((occ.with_timezone(&tz).date_naive() + Duration::days(days)).and_hms_opt(0, 0, 0).unwrap(), tz).unwrap_or(s + span),
            None => s + span,
        };
        if overlaps(s, e) {
            out.push((s, e));
        }
    }
    Ok(out)
}

/// Occurrences of `ev` in the window, ready for a view.
pub fn occurrences(ev: &Event, color: &str, from: DateTime<Utc>, to: DateTime<Utc>) -> Result<Vec<Occurrence>> {
    Ok(expand(ev, from, to)?
        .into_iter()
        .map(|(s, e)| Occurrence {
            event_id: ev.id.clone(),
            calendar_id: ev.calendar_id.clone(),
            title: ev.title.clone(),
            location: ev.location.clone(),
            start_utc: rfc3339(s),
            end_utc: rfc3339(e),
            all_day: ev.all_day,
            tz: ev.tz.clone(),
            recurring: ev.rrule.is_some(),
            status: ev.status.clone(),
            color: color.to_string(),
            instance: rfc3339(s),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(start: &str, end: &str, tz: &str, rrule: Option<&str>, all_day: bool) -> Event {
        Event {
            id: "e1".into(), calendar_id: "c".into(), uid: "u".into(), title: "T".into(), description: None, location: None,
            start_utc: start.into(), end_utc: end.into(), all_day, tz: Some(tz.into()), rrule: rrule.map(String::from), exdates: vec![],
            status: "confirmed".into(), organizer: None, attendees: vec![], source_message_id: None, sequence: 0, updated_at: String::new(),
        }
    }

    fn at(s: &str) -> DateTime<Utc> {
        parse_instant(s).unwrap()
    }

    fn starts(ev: &Event, from: &str, to: &str) -> Vec<String> {
        expand(ev, at(from), at(to)).unwrap().into_iter().map(|(s, _)| rfc3339(s)).collect()
    }

    #[test]
    fn a_single_event_appears_only_in_overlapping_windows() {
        let e = event("2026-10-29T19:30:00Z", "2026-10-29T20:30:00Z", "America/Chicago", None, false);
        assert_eq!(starts(&e, "2026-10-29T00:00:00Z", "2026-10-30T00:00:00Z").len(), 1);
        assert!(starts(&e, "2026-10-30T00:00:00Z", "2026-10-31T00:00:00Z").is_empty());
        assert_eq!(starts(&e, "2026-10-29T20:00:00Z", "2026-10-29T21:00:00Z").len(), 1, "a window that starts mid-event still sees it");
        assert!(starts(&e, "2026-10-29T20:30:00Z", "2026-10-29T21:00:00Z").is_empty(), "the end is exclusive");
    }

    #[test]
    fn weekly_events_keep_their_wall_clock_time_across_daylight_saving() {
        // Thursdays at 9:00 AM Chicago, first one Oct 29 (CDT, UTC-5); DST ends Nov 1 (then UTC-6).
        let e = event("2026-10-29T14:00:00Z", "2026-10-29T15:00:00Z", "America/Chicago", Some("FREQ=WEEKLY;BYDAY=TH"), false);
        assert_eq!(
            starts(&e, "2026-10-29T00:00:00Z", "2026-11-20T00:00:00Z"),
            vec!["2026-10-29T14:00:00Z", "2026-11-05T15:00:00Z", "2026-11-12T15:00:00Z", "2026-11-19T15:00:00Z"],
            "9 AM is 14:00Z before the change and 15:00Z after it"
        );
    }

    #[test]
    fn spring_forward_also_keeps_the_wall_clock() {
        // Mondays at 9:00 AM Chicago from Mar 2 2026; DST starts Mar 8.
        let e = event("2026-03-02T15:00:00Z", "2026-03-02T16:00:00Z", "America/Chicago", Some("FREQ=WEEKLY;BYDAY=MO"), false);
        assert_eq!(starts(&e, "2026-03-01T00:00:00Z", "2026-03-17T00:00:00Z"), vec!["2026-03-02T15:00:00Z", "2026-03-09T14:00:00Z", "2026-03-16T14:00:00Z"]);
    }

    #[test]
    fn count_until_interval_and_exdate() {
        let daily = event("2026-10-26T15:00:00Z", "2026-10-26T16:00:00Z", "UTC", Some("FREQ=DAILY;COUNT=3"), false);
        assert_eq!(starts(&daily, "2026-10-01T00:00:00Z", "2026-12-01T00:00:00Z").len(), 3);
        let until = event("2026-10-26T15:00:00Z", "2026-10-26T16:00:00Z", "UTC", Some("FREQ=DAILY;UNTIL=20261028T235959Z"), false);
        assert_eq!(starts(&until, "2026-10-01T00:00:00Z", "2026-12-01T00:00:00Z").len(), 3);
        let every_other = event("2026-10-26T15:00:00Z", "2026-10-26T16:00:00Z", "UTC", Some("FREQ=DAILY;INTERVAL=2"), false);
        assert_eq!(starts(&every_other, "2026-10-26T00:00:00Z", "2026-11-01T00:00:00Z"), vec!["2026-10-26T15:00:00Z", "2026-10-28T15:00:00Z", "2026-10-30T15:00:00Z"]);
        let mut ex = event("2026-10-26T15:00:00Z", "2026-10-26T16:00:00Z", "UTC", Some("FREQ=DAILY;COUNT=4"), false);
        ex.exdates = vec!["2026-10-27T15:00:00Z".into()];
        assert_eq!(starts(&ex, "2026-10-01T00:00:00Z", "2026-12-01T00:00:00Z").len(), 3, "a removed occurrence is skipped");
    }

    #[test]
    fn monthly_and_yearly_patterns() {
        let monthly = event("2026-01-31T15:00:00Z", "2026-01-31T16:00:00Z", "UTC", Some("FREQ=MONTHLY;BYMONTHDAY=31"), false);
        // Months without a 31st are skipped, not clamped.
        let s = starts(&monthly, "2026-01-01T00:00:00Z", "2026-06-01T00:00:00Z");
        assert_eq!(s, vec!["2026-01-31T15:00:00Z", "2026-03-31T15:00:00Z", "2026-05-31T15:00:00Z"]);
        let nth = event("2026-10-13T15:00:00Z", "2026-10-13T16:00:00Z", "UTC", Some("FREQ=MONTHLY;BYDAY=2TU"), false);
        assert_eq!(starts(&nth, "2026-10-01T00:00:00Z", "2027-01-01T00:00:00Z"), vec!["2026-10-13T15:00:00Z", "2026-11-10T15:00:00Z", "2026-12-08T15:00:00Z"]);
        let yearly = event("2026-02-14T15:00:00Z", "2026-02-14T16:00:00Z", "UTC", Some("FREQ=YEARLY"), false);
        assert_eq!(starts(&yearly, "2026-01-01T00:00:00Z", "2029-01-01T00:00:00Z").len(), 3);
    }

    #[test]
    fn all_day_series_keep_whole_local_days() {
        // A two-day all-day event every week, starting Fri Oct 30 (CDT). The second one spans the DST end.
        let e = event("2026-10-30T05:00:00Z", "2026-11-01T05:00:00Z", "America/Chicago", Some("FREQ=WEEKLY"), true);
        let occ = expand(&e, at("2026-10-25T00:00:00Z"), at("2026-11-12T00:00:00Z")).unwrap();
        let pairs: Vec<(String, String)> = occ.iter().map(|(s, e)| (rfc3339(*s), rfc3339(*e))).collect();
        assert_eq!(pairs[0], ("2026-10-30T05:00:00Z".into(), "2026-11-01T05:00:00Z".into()));
        assert_eq!(pairs[1], ("2026-11-06T06:00:00Z".into(), "2026-11-08T06:00:00Z".into()), "midnight is 06:00Z after the change");
    }

    #[test]
    fn an_occurrence_that_began_before_the_window_still_shows() {
        // A 10 PM - 2 AM event nightly; the window starts at 1 AM.
        let e = event("2026-10-26T22:00:00Z", "2026-10-27T02:00:00Z", "UTC", Some("FREQ=DAILY"), false);
        assert_eq!(starts(&e, "2026-10-27T01:00:00Z", "2026-10-27T03:00:00Z"), vec!["2026-10-26T22:00:00Z"]);
    }

    #[test]
    fn rules_are_validated_and_canonicalised() {
        let start = at("2026-10-29T14:00:00Z");
        let tz = chrono_tz::America::Chicago;
        assert_eq!(normalize_rule("rrule:freq=weekly;byday=th", start, tz).unwrap(), "FREQ=WEEKLY;BYDAY=TH");
        for bad in ["", "FREQ=SOMETIMES", "FREQ=WEEKLY;BYDAY=XX", "DTSTART:20261029T090000Z\nRRULE:FREQ=DAILY", "FREQ=DAILY\nFREQ=WEEKLY", "nonsense"] {
            assert!(normalize_rule(bad, start, tz).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn endless_rules_are_bounded_by_the_window() {
        let e = event("2020-01-01T15:00:00Z", "2020-01-01T16:00:00Z", "UTC", Some("FREQ=DAILY"), false);
        // Six years later: a one-week window yields a week of occurrences without iterating blindly forever.
        let s = starts(&e, "2026-10-26T00:00:00Z", "2026-11-02T00:00:00Z");
        assert_eq!(s.len(), 7);
    }

    #[test]
    fn occurrences_carry_what_a_view_needs() {
        let e = event("2026-10-29T14:00:00Z", "2026-10-29T15:00:00Z", "America/Chicago", Some("FREQ=WEEKLY"), false);
        let o = occurrences(&e, "#7aa2f7", at("2026-10-29T00:00:00Z"), at("2026-11-13T00:00:00Z")).unwrap();
        assert_eq!(o.len(), 3);
        assert!(o.iter().all(|x| x.recurring && x.color == "#7aa2f7" && x.event_id == "e1"));
        assert_eq!(o[1].instance, o[1].start_utc);
    }
}
