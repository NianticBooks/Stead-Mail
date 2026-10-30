//! Google Calendar events over REST: incremental pull (syncToken) and push, translating between
//! Google's JSON and our events.

use chrono::{DateTime, Duration, NaiveDate, Utc};
use chrono_tz::Tz;
use serde_json::{Value, json};

use super::ical::{self, Imported};
use super::model::{Event, parse_instant};
use crate::error::{Error, Result};
use crate::google::GoogleEndpoints;

/// How far back the first sync reaches. Older events stay on the server.
pub const HISTORY_DAYS: i64 = 90;

/// One event as the server has it.
#[derive(Debug, Clone)]
pub struct RemoteEvent {
    pub remote_id: String,
    pub etag: Option<String>,
    pub item: Imported,
    /// Parts we keep verbatim (CalDAV only).
    pub extra: Option<ical::Extra>,
}

#[derive(Debug, Default)]
pub struct Pull {
    pub upserts: Vec<RemoteEvent>,
    pub deleted: Vec<String>,
    /// Single occurrences removed on the server: (series remote id, start of the occurrence).
    pub removed_occurrences: Vec<(String, DateTime<Utc>)>,
    pub skipped: Vec<String>,
    pub next_token: Option<String>,
    /// The server listed everything (not a delta): anything it did not list is gone.
    pub full: bool,
}

pub struct GoogleCal<'a> {
    pub http: &'a reqwest::Client,
    pub ep: &'a GoogleEndpoints,
    pub token: &'a str,
    pub calendar_id: &'a str,
    pub default_tz: Tz,
}

fn events_url(base: &str, calendar_id: &str) -> String {
    let mut u = url::Url::parse(base).expect("calendar base url");
    u.path_segments_mut().expect("base").pop_if_empty().extend(["calendars", calendar_id, "events"]);
    u.to_string()
}

async fn check(resp: reqwest::Response, what: &str) -> Result<reqwest::Response> {
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }
    let text = resp.text().await.unwrap_or_default();
    Err(Error::Http(format!("{what}: HTTP {status}: {}", text.chars().take(300).collect::<String>())))
}

/// A Google id must be 5-1024 characters of a-v and 0-9; our event ids (hex) always are. Using
/// them means a retried create cannot make a duplicate.
pub fn google_id(event_id: &str) -> String {
    event_id.chars().filter(|c| c.is_ascii_hexdigit()).collect::<String>().to_ascii_lowercase()
}

impl GoogleCal<'_> {
    pub async fn pull(&self, sync_token: Option<&str>) -> Result<Pull> {
        let mut pull = Pull { full: sync_token.is_none(), ..Default::default() };
        let mut page: Option<String> = None;
        let base = events_url(&self.ep.calendar_base, self.calendar_id);
        loop {
            let mut q: Vec<(String, String)> = vec![("maxResults".into(), "250".into()), ("showDeleted".into(), "true".into()), ("singleEvents".into(), "false".into())];
            match sync_token {
                Some(t) => q.push(("syncToken".into(), t.into())),
                None => q.push(("timeMin".into(), super::model::rfc3339(Utc::now() - Duration::days(HISTORY_DAYS)))),
            }
            if let Some(p) = &page {
                q.push(("pageToken".into(), p.clone()));
            }
            let resp = self.http.get(&base).bearer_auth(self.token).query(&q).send().await?;
            if resp.status().as_u16() == 410 {
                return Err(Error::Http("sync token expired".into()));
            }
            let body: Value = check(resp, "events.list").await?.json().await?;
            for item in body["items"].as_array().cloned().unwrap_or_default() {
                self.take(&item, &mut pull);
            }
            if let Some(t) = body["nextSyncToken"].as_str() {
                pull.next_token = Some(t.to_string());
            }
            match body["nextPageToken"].as_str() {
                Some(p) => page = Some(p.to_string()),
                None => break,
            }
        }
        Ok(pull)
    }

    fn take(&self, item: &Value, pull: &mut Pull) {
        let Some(id) = item["id"].as_str() else { return };
        let title = item["summary"].as_str().unwrap_or("(untitled)").to_string();
        let cancelled = item["status"].as_str() == Some("cancelled");
        // One occurrence of a series, changed or removed.
        if let Some(master) = item["recurringEventId"].as_str() {
            if cancelled {
                if let Some(at) = original_start(item, self.default_tz) {
                    pull.removed_occurrences.push((master.to_string(), at));
                }
            } else {
                pull.skipped.push(format!("{title}: a changed single occurrence of a series is not supported yet"));
            }
            return;
        }
        if cancelled {
            pull.deleted.push(id.to_string());
            return;
        }
        match self.to_imported(item) {
            Ok(it) => pull.upserts.push(RemoteEvent { remote_id: id.to_string(), etag: item["etag"].as_str().map(String::from), item: it, extra: None }),
            Err(e) => pull.skipped.push(format!("{title}: {e}")),
        }
    }

    fn to_imported(&self, item: &Value) -> Result<Imported> {
        let uid = item["iCalUID"].as_str().or(item["id"].as_str()).unwrap_or_default();
        let mut lines = vec!["BEGIN:VCALENDAR".to_string(), "BEGIN:VEVENT".into(), format!("UID:{uid}")];
        lines.push(time_line("DTSTART", &item["start"]).ok_or_else(|| Error::Invalid("no start time".into()))?);
        if let Some(l) = time_line("DTEND", &item["end"]) {
            lines.push(l);
        }
        lines.push(format!("SUMMARY:{}", ical::esc(item["summary"].as_str().unwrap_or("(untitled)"))));
        if let Some(l) = item["location"].as_str() {
            lines.push(format!("LOCATION:{}", ical::esc(l)));
        }
        if let Some(d) = item["description"].as_str() {
            lines.push(format!("DESCRIPTION:{}", ical::esc(d)));
        }
        lines.push(format!("STATUS:{}", item["status"].as_str().unwrap_or("confirmed").to_ascii_uppercase()));
        if let Some(n) = item["sequence"].as_i64() {
            lines.push(format!("SEQUENCE:{n}"));
        }
        if let Some(o) = item["organizer"]["email"].as_str() {
            lines.push(format!("ORGANIZER:mailto:{o}"));
        }
        for a in item["attendees"].as_array().into_iter().flatten() {
            if a["resource"].as_bool() != Some(true)
                && let Some(e) = a["email"].as_str()
            {
                lines.push(format!("ATTENDEE:mailto:{e}"));
            }
        }
        for r in item["recurrence"].as_array().into_iter().flatten().filter_map(|r| r.as_str()) {
            // Recurrence lines carry their own parameters; they must not contain a line break.
            if !r.contains(['\r', '\n']) {
                lines.push(r.to_string());
            }
        }
        lines.push("END:VEVENT".into());
        lines.push("END:VCALENDAR".into());
        let mut skipped = Vec::new();
        let parsed = crate::ai::ics::parse(&lines.join("\r\n"));
        let one = parsed.first().ok_or_else(|| Error::Invalid("unreadable event".into()))?;
        ical::import_one(one, self.default_tz, &mut skipped).ok_or_else(|| Error::Invalid(skipped.join("; ")))
    }

    /// Create the event on the server; returns (remote id, etag). A repeated create of the same
    /// event finds it already there and adopts it.
    pub async fn create(&self, ev: &Event) -> Result<(String, Option<String>)> {
        let mut body = to_google(ev)?;
        let id = google_id(&ev.id);
        body["id"] = json!(id);
        let url = events_url(&self.ep.calendar_base, self.calendar_id);
        let resp = self.http.post(&url).bearer_auth(self.token).query(&[("sendUpdates", "none")]).json(&body).send().await?;
        if resp.status().as_u16() == 409 {
            // Already created by an earlier attempt that we did not hear back from.
            return self.update(&id, None, ev).await;
        }
        let v: Value = check(resp, "events.insert").await?.json().await?;
        Ok((v["id"].as_str().unwrap_or(&id).to_string(), v["etag"].as_str().map(String::from)))
    }

    /// Replace the event; `etag` makes the server refuse if it changed since we last saw it.
    pub async fn update(&self, remote_id: &str, etag: Option<&str>, ev: &Event) -> Result<(String, Option<String>)> {
        let mut url = url::Url::parse(&events_url(&self.ep.calendar_base, self.calendar_id)).map_err(|e| Error::Http(e.to_string()))?;
        url.path_segments_mut().map_err(|_| Error::Http("bad url".into()))?.push(remote_id);
        let mut req = self.http.put(url).bearer_auth(self.token).query(&[("sendUpdates", "none")]).json(&to_google(ev)?);
        if let Some(e) = etag {
            req = req.header("If-Match", e);
        }
        let resp = req.send().await?;
        if resp.status().as_u16() == 412 {
            return Err(Error::Http("412 changed on the server".into()));
        }
        let v: Value = check(resp, "events.update").await?.json().await?;
        Ok((v["id"].as_str().unwrap_or(remote_id).to_string(), v["etag"].as_str().map(String::from)))
    }

    /// Delete; already gone counts as done.
    pub async fn delete(&self, remote_id: &str, etag: Option<&str>) -> Result<()> {
        let mut url = url::Url::parse(&events_url(&self.ep.calendar_base, self.calendar_id)).map_err(|e| Error::Http(e.to_string()))?;
        url.path_segments_mut().map_err(|_| Error::Http("bad url".into()))?.push(remote_id);
        let mut req = self.http.delete(url).bearer_auth(self.token).query(&[("sendUpdates", "none")]);
        if let Some(e) = etag {
            req = req.header("If-Match", e);
        }
        let resp = req.send().await?;
        let code = resp.status().as_u16();
        if code == 404 || code == 410 {
            return Ok(());
        }
        if code == 412 {
            return Err(Error::Http("412 changed on the server".into()));
        }
        check(resp, "events.delete").await?;
        Ok(())
    }
}

fn original_start(item: &Value, default_tz: Tz) -> Option<DateTime<Utc>> {
    let t = &item["originalStartTime"];
    if let Some(dt) = t["dateTime"].as_str() {
        return DateTime::parse_from_rfc3339(dt).ok().map(|d| d.with_timezone(&Utc));
    }
    let d = NaiveDate::parse_from_str(t["date"].as_str()?, "%Y-%m-%d").ok()?;
    crate::ai::ics::local_to_utc(d.and_hms_opt(0, 0, 0)?, default_tz)
}

/// An iCalendar time line for one of Google's start/end objects.
fn time_line(name: &str, t: &Value) -> Option<String> {
    if let Some(d) = t["date"].as_str() {
        return Some(format!("{name};VALUE=DATE:{}", d.replace('-', "")));
    }
    let dt = DateTime::parse_from_rfc3339(t["dateTime"].as_str()?).ok()?.with_timezone(&Utc);
    match t["timeZone"].as_str().and_then(|z| z.parse::<Tz>().ok()) {
        Some(tz) => Some(format!("{name};TZID={}:{}", tz.name(), dt.with_timezone(&tz).format("%Y%m%dT%H%M%S"))),
        None => Some(format!("{name}:{}", dt.format("%Y%m%dT%H%M%SZ"))),
    }
}

/// Our event as Google's JSON.
pub fn to_google(ev: &Event) -> Result<Value> {
    let (start, end) = (parse_instant(&ev.start_utc)?, parse_instant(&ev.end_utc)?);
    let tz: Tz = ev.tz.as_deref().and_then(|t| t.parse().ok()).unwrap_or(chrono_tz::UTC);
    let (s, e) = if ev.all_day {
        (json!({"date": start.with_timezone(&tz).format("%Y-%m-%d").to_string()}), json!({"date": end.with_timezone(&tz).format("%Y-%m-%d").to_string()}))
    } else {
        (
            json!({"dateTime": super::model::rfc3339(start), "timeZone": tz.name()}),
            json!({"dateTime": super::model::rfc3339(end), "timeZone": tz.name()}),
        )
    };
    let mut v = json!({
        "iCalUID": ev.uid,
        "summary": ev.title,
        "start": s,
        "end": e,
        "status": ev.status,
        "sequence": ev.sequence,
        "description": ev.description.clone().unwrap_or_default(),
        "location": ev.location.clone().unwrap_or_default(),
    });
    if !ev.attendees.is_empty() {
        v["attendees"] = Value::Array(ev.attendees.iter().map(|a| json!({"email": a})).collect());
    }
    if let Some(r) = &ev.rrule {
        let mut rec = vec![format!("RRULE:{r}")];
        let ex: Vec<String> = ev
            .exdates
            .iter()
            .filter_map(|x| parse_instant(x).ok())
            .map(|x| if ev.all_day { x.with_timezone(&tz).format("%Y%m%d").to_string() } else { x.format("%Y%m%dT%H%M%SZ").to_string() })
            .collect();
        if !ex.is_empty() {
            rec.push(if ev.all_day { format!("EXDATE;VALUE=DATE:{}", ex.join(",")) } else { format!("EXDATE:{}", ex.join(",")) });
        }
        v["recurrence"] = json!(rec);
    }
    Ok(v)
}
