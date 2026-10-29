//! Two-way calendar sync. A synced calendar is a local mirror of one Google or CalDAV calendar.
//!
//! Each run pulls first, then pushes. When the same event changed in both places, the server's
//! version wins and the loss is written to the sync log, so nothing disappears silently.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use serde::Serialize;

use super::gcal::{GoogleCal, Pull, RemoteEvent};
use super::ical::{Extra, Imported};
use super::model::{Calendar, Event, clean_text, clean_title, parse_instant, rfc3339};
use super::syncdb::{self, SyncCal, SyncEvent, SyncLogEntry};
use super::dav;
use crate::account::Provider;
use crate::api::Api;
use crate::error::{Error, Result};

#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct CalSyncReport {
    pub calendar: String,
    /// Events added or changed here because the server had newer versions.
    pub pulled: usize,
    /// Events removed here because they were deleted on the server.
    pub removed: usize,
    /// Local changes sent to the server (creates, edits, deletes).
    pub pushed: usize,
    /// Places where both sides had changed and the server's version won.
    pub conflicts: usize,
    pub skipped: Vec<String>,
    pub errors: Vec<String>,
}

/// A calendar on a server that can be mirrored here.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RemoteCalendar {
    /// google | caldav
    pub source: String,
    /// Display name of the account (an email address).
    pub account: String,
    pub account_id: String,
    pub remote_id: String,
    pub name: String,
    pub color: Option<String>,
    pub read_only: bool,
    /// The local calendar mirroring it, if linked already.
    pub linked_to: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct RemoteList {
    pub calendars: Vec<RemoteCalendar>,
    /// One entry per account that could not be listed; the others still appear.
    pub errors: Vec<String>,
}

const PALETTE: &[&str] = &["#7aa2f7", "#9ece6a", "#e0af68", "#f7768e", "#bb9af7", "#7dcfff", "#ff9e64", "#73daca"];

fn tidy_color(c: Option<&str>, fallback_index: usize) -> String {
    // Apple sends #RRGGBBAA; Google sends #rrggbb.
    let c = c.map(|c| c.trim().chars().take(7).collect::<String>().to_ascii_lowercase());
    match c {
        Some(c) if c.len() == 7 && c.starts_with('#') && c[1..].chars().all(|h| h.is_ascii_hexdigit()) => c,
        _ => PALETTE[fallback_index % PALETTE.len()].to_string(),
    }
}

fn is_conflict(e: &Error) -> bool {
    e.to_string().contains("412")
}

/// A short, readable account of an event for the log.
fn describe(ev: &Event) -> String {
    let tz: Tz = ev.tz.as_deref().and_then(|t| t.parse().ok()).unwrap_or(chrono_tz::UTC);
    let when = match parse_instant(&ev.start_utc) {
        Ok(t) if ev.all_day => t.with_timezone(&tz).format("%a %-d %b %Y, all day").to_string(),
        Ok(t) => format!("{} {}", t.with_timezone(&tz).format("%a %-d %b %Y %H:%M"), tz.name()),
        Err(_) => ev.start_utc.clone(),
    };
    let mut s = format!("“{}” on {when}", ev.title);
    if let Some(l) = &ev.location {
        s.push_str(&format!(" at {l}"));
    }
    if ev.rrule.is_some() {
        s.push_str(" (repeats)");
    }
    s
}

fn same_content(a: &Event, b: &Event) -> bool {
    fn norm(v: &[String]) -> Vec<String> {
        let mut v: Vec<String> = v.iter().map(|s| s.to_ascii_lowercase()).collect();
        v.sort();
        v
    }
    a.title == b.title
        && a.description == b.description
        && a.location == b.location
        && a.start_utc == b.start_utc
        && a.end_utc == b.end_utc
        && a.all_day == b.all_day
        && a.rrule == b.rrule
        && a.status == b.status
        && norm(&a.exdates) == norm(&b.exdates)
        && norm(&a.attendees) == norm(&b.attendees)
}

fn event_from(imp: &Imported, calendar_id: &str, id: String, fallback_uid: &str) -> Event {
    Event {
        id,
        calendar_id: calendar_id.to_string(),
        uid: imp.uid.clone().unwrap_or_else(|| fallback_uid.to_string()),
        title: clean_title(&imp.title).unwrap_or_else(|_| "(untitled)".into()),
        description: imp.description.as_deref().map(|d| clean_text(d, 5000)).filter(|d| !d.is_empty()),
        location: imp.location.as_deref().map(|d| clean_text(d, 300).replace('\n', " ")).filter(|d| !d.is_empty()),
        start_utc: rfc3339(imp.start),
        end_utc: rfc3339(imp.end),
        all_day: imp.all_day,
        tz: imp.tz.clone(),
        rrule: imp.rrule.clone(),
        exdates: {
            let mut x = imp.exdates.clone();
            x.sort();
            x.dedup();
            x
        },
        status: imp.status.clone(),
        organizer: imp.organizer.clone(),
        attendees: imp.attendees.iter().take(50).cloned().collect(),
        source_message_id: None,
        sequence: imp.sequence,
        updated_at: String::new(),
    }
}

/// Apply what the server reported to the local mirror. Remote wins; every time that discards a
/// local change it is logged. `forget_unlisted_before`: after a full listing, rows the server did
/// not list are gone (limited to events that end after that instant, since older ones were never asked for).
pub fn apply_pull(conn: &rusqlite::Connection, cal: &SyncCal, pull: &Pull, forget_unlisted_before: Option<DateTime<Utc>>, report: &mut CalSyncReport) -> Result<()> {
    let rows = syncdb::sync_events(conn, &cal.id)?;
    let by_remote: HashMap<&str, &SyncEvent> = rows.iter().filter_map(|r| r.remote_id.as_deref().map(|id| (id, r))).collect();
    let by_uid: HashMap<&str, &SyncEvent> = rows.iter().filter(|r| r.remote_id.is_none() && !r.deleted).map(|r| (r.event.uid.as_str(), r)).collect();
    let mut gone: Vec<String> = pull.deleted.clone();
    if let Some(cutoff) = forget_unlisted_before {
        let listed: HashSet<&str> = pull.upserts.iter().map(|u| u.remote_id.as_str()).chain(pull.deleted.iter().map(String::as_str)).collect();
        for r in &rows {
            let (Some(rid), false) = (r.remote_id.as_deref(), r.event.rrule.is_none() && parse_instant(&r.event.end_utc).is_ok_and(|e| e < cutoff)) else { continue };
            if !listed.contains(rid) {
                gone.push(rid.to_string());
            }
        }
    }

    let mut taken: HashSet<String> = rows.iter().filter(|r| !r.deleted).map(|r| r.event.uid.clone()).collect();
    for RemoteEvent { remote_id, etag, item, extra } in &pull.upserts {
        let local = by_remote.get(remote_id.as_str()).copied().or_else(|| item.uid.as_deref().and_then(|u| by_uid.get(u).copied()));
        let id = local.map(|l| l.event.id.clone()).unwrap_or_else(|| uuid::Uuid::new_v4().simple().to_string());
        let mut ev = event_from(item, &cal.id, id, remote_id);
        let extra_json = extra.as_ref().map(|x| serde_json::to_string(x).unwrap_or_default());
        // The unique (calendar, uid) index would reject a second live event with the same uid.
        if local.is_none_or(|l| l.event.uid != ev.uid) && !taken.insert(ev.uid.clone()) {
            report.skipped.push(format!("{}: another event in this calendar already has the same id", ev.title));
            continue;
        }
        if let Some(l) = local {
            ev.source_message_id = l.event.source_message_id;
            if l.deleted && !same_content(&l.event, &ev) {
                syncdb::log(conn, cal, "conflict", &ev.title, &format!("You deleted this here, but it changed on the server, so it is back. {}", describe(&ev)))?;
                report.conflicts += 1;
            } else if l.deleted {
                // Deleted here, unchanged there: our deletion is still waiting to be pushed.
                continue;
            } else if l.dirty && !same_content(&l.event, &ev) {
                syncdb::log(conn, cal, "conflict", &ev.title, &format!("Changed here and on the server. The server's version was kept.\nYours: {}\nServer: {}", describe(&l.event), describe(&ev)))?;
                report.conflicts += 1;
            } else if !l.dirty && l.etag.as_deref() == etag.as_deref() && etag.is_some() {
                continue;
            }
        }
        syncdb::put_remote(conn, &ev, remote_id, etag.as_deref(), extra_json.as_deref())?;
        report.pulled += 1;
    }

    for rid in &gone {
        let Some(l) = by_remote.get(rid.as_str()) else { continue };
        if l.dirty && !l.deleted {
            syncdb::log(conn, cal, "conflict", &l.event.title, &format!("Changed here, but deleted on the server, so it was removed here too. {}", describe(&l.event)))?;
            report.conflicts += 1;
        }
        syncdb::hard_delete(conn, &l.event.id)?;
        if !l.deleted {
            report.removed += 1;
        }
    }

    for (master, at) in &pull.removed_occurrences {
        let Some(l) = by_remote.get(master.as_str()) else { continue };
        let stamp = rfc3339(*at);
        if l.event.rrule.is_none() || l.deleted || l.event.exdates.contains(&stamp) {
            continue;
        }
        let mut ex = l.event.exdates.clone();
        ex.push(stamp);
        ex.sort();
        conn.execute("UPDATE events SET exdates = ?2 WHERE id = ?1", rusqlite::params![l.event.id, serde_json::to_string(&ex).unwrap_or_else(|_| "[]".into())])?;
        report.pulled += 1;
    }
    for s in &pull.skipped {
        if !report.skipped.contains(s) {
            report.skipped.push(s.clone());
        }
    }
    Ok(())
}

enum Remote<'a> {
    Google(GoogleCal<'a>),
    Dav { client: crate::caldav::CalDavClient, collection: String },
}

impl Remote<'_> {
    async fn create(&self, se: &SyncEvent) -> Result<(String, Option<String>)> {
        match self {
            Remote::Google(g) => g.create(&se.event).await,
            Remote::Dav { client, collection } => {
                let href = dav::href_for(collection, &se.event);
                let etag = client.put(&href, &dav::body_for(&se.event, parsed_extra(se).as_ref()), None).await?;
                Ok((href, etag))
            }
        }
    }

    async fn update(&self, remote_id: &str, se: &SyncEvent) -> Result<(String, Option<String>)> {
        match self {
            Remote::Google(g) => g.update(remote_id, se.etag.as_deref(), &se.event).await,
            Remote::Dav { client, .. } => {
                let etag = client.put(remote_id, &dav::body_for(&se.event, parsed_extra(se).as_ref()), se.etag.as_deref()).await?;
                Ok((remote_id.to_string(), etag))
            }
        }
    }

    async fn delete(&self, remote_id: &str, etag: Option<&str>) -> Result<()> {
        match self {
            Remote::Google(g) => g.delete(remote_id, etag).await,
            Remote::Dav { client, .. } => client.remove(remote_id, etag).await,
        }
    }
}

fn parsed_extra(se: &SyncEvent) -> Option<Extra> {
    se.extra.as_deref().and_then(|j| serde_json::from_str(j).ok())
}

impl Api {
    /// What `calendar_sync` records about the last run, newest first.
    pub fn sync_log(&self, limit: usize) -> Result<Vec<SyncLogEntry>> {
        syncdb::read_log(&self.store().conn, limit.clamp(1, 500))
    }

    pub fn sync_log_clear(&self) -> Result<()> {
        syncdb::clear_log(&self.store().conn)
    }

    /// Sync one calendar, or every synced calendar. A failing calendar reports its error and the
    /// others still run.
    pub async fn calendar_sync(&self, calendar: Option<&str>) -> Result<Vec<CalSyncReport>> {
        let _gate = self.sync_gate.try_lock().map_err(|_| Error::Invalid("a calendar sync is already running".into()))?;
        let cals: Vec<SyncCal> = {
            let store = self.store();
            match calendar {
                Some(id) => vec![syncdb::sync_calendar(&store.conn, id)?.ok_or_else(|| Error::Invalid("that calendar is not linked to Google or iCloud".into()))?],
                None => syncdb::sync_calendars(&store.conn)?,
            }
        };
        let mut reports = Vec::new();
        for cal in cals {
            let mut report = CalSyncReport { calendar: cal.name.clone(), ..Default::default() };
            match self.sync_one(&cal, &mut report).await {
                Ok(()) => {
                    let store = self.store();
                    syncdb::set_sync_state(&store.conn, &cal.id, None, None)?;
                }
                Err(e) => {
                    let msg = e.to_string();
                    let store = self.store();
                    syncdb::set_sync_state(&store.conn, &cal.id, None, Some(&msg))?;
                    syncdb::log(&store.conn, &cal, "error", "", &msg)?;
                    report.errors.push(msg);
                }
            }
            reports.push(report);
        }
        Ok(reports)
    }

    async fn sync_one(&self, cal: &SyncCal, report: &mut CalSyncReport) -> Result<()> {
        let tz = self.calendar_tz();
        match cal.kind.as_str() {
            "google" => {
                let (_identity, token) = self.google_access_token(&cal.account_id).await?;
                let remote = GoogleCal { http: &self.http, ep: &self.google_endpoints, token: &token, calendar_id: &cal.remote_id, default_tz: tz };
                let (pull, cutoff) = match remote.pull(cal.sync_token.as_deref()).await {
                    Ok(p) => {
                        let full = p.full;
                        (p, full.then(|| Utc::now() - chrono::Duration::days(super::gcal::HISTORY_DAYS)))
                    }
                    Err(e) if e.to_string().contains("sync token expired") => {
                        let p = remote.pull(None).await?;
                        (p, Some(Utc::now() - chrono::Duration::days(super::gcal::HISTORY_DAYS)))
                    }
                    Err(e) => return Err(e),
                };
                {
                    let mut store = self.store();
                    let tx = store.conn.transaction()?;
                    apply_pull(&tx, cal, &pull, cutoff, report)?;
                    tx.commit()?;
                }
                self.push_all(cal, &Remote::Google(remote), report).await?;
                // The token is saved last: if anything above failed, the next run replays the same delta.
                if let Some(t) = &pull.next_token {
                    syncdb::set_sync_state(&self.store().conn, &cal.id, Some(t), None)?;
                }
                Ok(())
            }
            "caldav" => {
                let account = self.store().find_account(&cal.account_id)?;
                let password = self.password_for(&account)?;
                let client = self.caldav_client(&account, &password)?;
                let known: dav::Known = {
                    let store = self.store();
                    syncdb::sync_events(&store.conn, &cal.id)?.into_iter().filter_map(|r| Some((r.remote_id?, r.etag?))).collect()
                };
                let pull = dav::pull(&client, &cal.remote_id, cal.sync_token.as_deref(), &known, tz, false).await?;
                {
                    let mut store = self.store();
                    let tx = store.conn.transaction()?;
                    apply_pull(&tx, cal, &pull, None, report)?;
                    tx.commit()?;
                }
                self.push_all(cal, &Remote::Dav { client, collection: cal.remote_id.clone() }, report).await?;
                if let Some(t) = &pull.next_token {
                    syncdb::set_sync_state(&self.store().conn, &cal.id, Some(t), None)?;
                }
                Ok(())
            }
            other => Err(Error::Invalid(format!("don't know how to sync a '{other}' calendar"))),
        }
    }

    async fn push_all(&self, cal: &SyncCal, remote: &Remote<'_>, report: &mut CalSyncReport) -> Result<()> {
        if cal.read_only {
            return Ok(());
        }
        let dirty = syncdb::dirty_events(&self.store().conn, &cal.id)?;
        for se in dirty {
            let title = se.event.title.clone();
            let outcome: Result<()> = async {
                if se.deleted {
                    if let Some(rid) = &se.remote_id {
                        remote.delete(rid, se.etag.as_deref()).await?;
                    }
                    syncdb::hard_delete(&self.store().conn, &se.event.id)?;
                } else {
                    let (rid, etag) = match &se.remote_id {
                        Some(rid) => remote.update(rid, &se).await?,
                        None => remote.create(&se).await?,
                    };
                    syncdb::mark_pushed(&self.store().conn, &se.event, &rid, etag.as_deref())?;
                }
                Ok(())
            }
            .await;
            match outcome {
                Ok(()) => report.pushed += 1,
                Err(e) if is_conflict(&e) => {
                    let store = self.store();
                    syncdb::log(&store.conn, cal, "info", &title, "Changed on the server while syncing. It will be reconciled on the next sync, and the server's version wins.")?;
                    report.errors.push(format!("{title}: changed on the server while syncing"));
                }
                Err(e) => {
                    let store = self.store();
                    syncdb::log(&store.conn, cal, "error", &title, &format!("Could not send this change: {e}"))?;
                    report.errors.push(format!("{title}: {e}"));
                }
            }
        }
        Ok(())
    }

    // ----- linking -----

    /// Every calendar the signed-in Google accounts and CalDAV (iCloud) accounts offer.
    pub async fn calendar_remote_list(&self) -> Result<RemoteList> {
        let mut out = RemoteList::default();
        let (identities, accounts) = {
            let store = self.store();
            (store.list_oauth_identities("google")?, store.list_accounts()?)
        };
        let linked = |kind: &str, account: &str, remote: &str| -> Option<String> { syncdb::linked(&self.store().conn, kind, account, remote).ok().flatten() };
        for idn in identities {
            match self.google_calendars(&idn.id).await {
                Ok(list) => out.calendars.extend(list.into_iter().map(|c| RemoteCalendar {
                    linked_to: linked("google", &idn.id, &c.id),
                    source: "google".into(),
                    account: idn.email.clone(),
                    account_id: idn.id.clone(),
                    name: c.summary.clone(),
                    color: c.background_color.clone(),
                    read_only: matches!(c.access_role.as_str(), "reader" | "freeBusyReader"),
                    remote_id: c.id,
                })),
                Err(e) => out.errors.push(format!("Google {}: {e}", idn.email)),
            }
        }
        for acc in accounts.into_iter().filter(|a| a.provider == Provider::Icloud || self.caldav_base.is_some()) {
            match self.caldav_calendars(&acc.id).await {
                Ok(rep) => out.calendars.extend(rep.calendars.into_iter().filter(|c| c.supports_events).map(|c| RemoteCalendar {
                    linked_to: linked("caldav", &acc.id, &c.href),
                    source: "caldav".into(),
                    account: acc.email.clone(),
                    account_id: acc.id.clone(),
                    name: c.display_name,
                    color: c.color,
                    read_only: false,
                    remote_id: c.href,
                })),
                Err(e) => out.errors.push(format!("{}: {e}", acc.email)),
            }
        }
        Ok(out)
    }

    /// Mirror a server calendar here. The first `calendar_sync` fills it. The server is not touched.
    pub async fn calendar_link(&self, source: &str, account_id: &str, remote_id: &str) -> Result<Calendar> {
        let list = self.calendar_remote_list().await?;
        let found = list
            .calendars
            .into_iter()
            .find(|c| c.source == source && c.account_id == account_id && c.remote_id == remote_id)
            .ok_or_else(|| Error::Invalid(format!("the server offers no such calendar{}", if list.errors.is_empty() { String::new() } else { format!(" ({})", list.errors.join("; ")) })))?;
        let store = self.store();
        if let Some(existing) = &found.linked_to {
            return store.calendar(existing);
        }
        let count = store.calendars()?.len();
        let suffix = if source == "google" { "Google" } else { "iCloud" };
        let mut name = clean_text(&found.name, 60).replace('\n', " ");
        if name.is_empty() {
            name = suffix.to_string();
        }
        let taken = |n: &str| store.calendars().map(|c| c.iter().any(|x| x.name.eq_ignore_ascii_case(n))).unwrap_or(false);
        if taken(&name) {
            name = format!("{name} ({suffix})");
        }
        let mut n = 2;
        while taken(&name) {
            name = format!("{} {n}", name.split(" (").next().unwrap_or(&name));
            n += 1;
        }
        let id = uuid::Uuid::new_v4().simple().to_string();
        store.conn.execute(
            "INSERT INTO calendars (id, name, color, kind, read_only, account_id, remote_id) VALUES (?1,?2,?3,?4,?5,?6,?7)",
            rusqlite::params![id, name, tidy_color(found.color.as_deref(), count), source, found.read_only as i64, account_id, remote_id],
        )?;
        store.calendar(&id)
    }

    /// Stop mirroring a server calendar and drop the local copy. Nothing is deleted on the server.
    /// Changes made here and not yet synced are lost.
    pub fn calendar_unlink(&self, id: &str) -> Result<()> {
        let store = self.store();
        let cal = store.calendar(id)?;
        if cal.kind == "local" {
            return Err(Error::Invalid("this calendar is not linked to a server; use delete instead".into()));
        }
        store.conn.execute("DELETE FROM calendars WHERE id = ?1", [id])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use serde_json::{Value, json};

    use super::*;
    use crate::calendar::model::EventInput;
    use crate::calendar::testserver::{Req, Resp, serve};
    use crate::google::GoogleEndpoints;
    use crate::oauth::TokenSet;
    use crate::account::Provider;
    use crate::secrets::EncryptedFileStore;
    use crate::store::{OAuthIdentity, Store};

    fn test_api() -> Api {
        let dir = std::env::temp_dir().join(format!("mailcore-sync-{}", uuid::Uuid::new_v4()));
        let secrets = EncryptedFileStore::open_with_passphrase(&dir.join("secrets.enc.json"), "pw").unwrap();
        Api::new(Store::open_in_memory().unwrap(), Box::new(secrets), dir)
    }

    // ----- Google -----

    #[derive(Default)]
    struct G {
        /// Events the next full listing returns.
        all: Vec<Value>,
        /// Events the next delta returns.
        delta: Vec<Value>,
        /// When set, every sync token is refused with 410 (the server forgot its history).
        expired: bool,
        inserted: Vec<Value>,
        updated: Vec<(String, Option<String>, Value)>,
        deleted: Vec<String>,
        fail_update_with_412: bool,
    }

    fn g_event(id: &str, etag: &str, title: &str, start: &str) -> Value {
        json!({"id": id, "etag": etag, "iCalUID": format!("{id}@google.com"), "summary": title, "status": "confirmed",
               "start": {"dateTime": start, "timeZone": "America/Chicago"},
               "end": {"dateTime": start.replace("T09", "T10"), "timeZone": "America/Chicago"}})
    }

    async fn google_api(state: Arc<Mutex<G>>) -> (Api, crate::calendar::testserver::Seen, Calendar) {
        let st = state.clone();
        let (port, seen) = serve(move |r: &Req| {
            let mut g = st.lock().unwrap();
            let p = r.path().to_string();
            if p == "/calendar/v3/users/me/calendarList" {
                return Resp::json(200, json!({"items": [
                    {"id": "primary@gmail.com", "summary": "Fred", "primary": true, "accessRole": "owner", "backgroundColor": "#9fe1e7"},
                    {"id": "ro@group.calendar.google.com", "summary": "Holidays", "accessRole": "reader"}]}));
            }
            let Some(rest) = p.strip_prefix("/calendar/v3/calendars/") else { return Resp::empty(404) };
            let (_cal, tail) = rest.split_once("/events").unwrap();
            let id = tail.trim_start_matches('/').to_string();
            match (r.method.as_str(), id.is_empty()) {
                ("GET", true) => match r.query("syncToken") {
                    Some(t) if g.expired || !(t == "T-full" || t == "T-next") => Resp::json(410, json!({"error": {"code": 410}})),
                    Some(_) => {
                        let items = std::mem::take(&mut g.delta);
                        Resp::json(200, json!({"items": items, "nextSyncToken": "T-next"}))
                    }
                    None => Resp::json(200, json!({"items": g.all.clone(), "nextSyncToken": "T-full"})),
                },
                ("POST", true) => {
                    let body: Value = serde_json::from_str(&r.body).unwrap();
                    let id = body["id"].as_str().unwrap().to_string();
                    g.inserted.push(body);
                    Resp::json(200, json!({"id": id, "etag": "\"created\""}))
                }
                ("PUT", false) => {
                    if g.fail_update_with_412 {
                        return Resp::json(412, json!({"error": {"code": 412}}));
                    }
                    let body: Value = serde_json::from_str(&r.body).unwrap();
                    g.updated.push((id.clone(), r.headers.get("if-match").cloned(), body));
                    Resp::json(200, json!({"id": id, "etag": "\"updated\""}))
                }
                ("DELETE", false) => {
                    g.deleted.push(id);
                    Resp::empty(204)
                }
                _ => Resp::empty(400),
            }
        })
        .await;
        let mut api = test_api();
        api.google_endpoints = GoogleEndpoints { calendar_base: format!("http://127.0.0.1:{port}/calendar/v3"), ..Default::default() };
        let ident = api
            .store()
            .upsert_oauth_identity(&OAuthIdentity { id: "gid".into(), provider: "google".into(), email: "fred@gmail.com".into(), scopes: vec![], client_id: "x".into(), created_at: String::new() })
            .unwrap();
        let tokens = TokenSet { access_token: "TOKEN".into(), refresh_token: None, expires_at: crate::oauth::now_unix() + 3600, scope: None, token_type: None };
        api.secrets.set(&ident.access_secret_key(), &serde_json::to_string(&tokens).unwrap()).unwrap();
        let cal = api.calendar_link("google", "gid", "primary@gmail.com").await.unwrap();
        (api, seen, cal)
    }

    fn titles(api: &Api, cal: &str) -> Vec<String> {
        let mut t: Vec<String> = api.event_list(Some(cal), 100).unwrap().into_iter().map(|e| e.title).collect();
        t.sort();
        t
    }

    #[tokio::test]
    async fn linking_lists_calendars_names_them_and_marks_read_only() {
        let (api, _, cal) = google_api(Arc::new(Mutex::new(G::default()))).await;
        assert_eq!(cal.name, "Fred");
        assert_eq!(cal.kind, "google");
        assert_eq!(cal.color, "#9fe1e7");
        assert!(!cal.read_only);
        let list = api.calendar_remote_list().await.unwrap();
        assert_eq!(list.calendars.len(), 2);
        assert_eq!(list.calendars[0].linked_to.as_deref(), Some(cal.id.as_str()));
        assert!(list.calendars[1].read_only && list.calendars[1].linked_to.is_none());
        // Linking twice returns the same calendar.
        assert_eq!(api.calendar_link("google", "gid", "primary@gmail.com").await.unwrap().id, cal.id);
        let ro = api.calendar_link("google", "gid", "ro@group.calendar.google.com").await.unwrap();
        assert!(ro.read_only);
        assert!(api.event_create(EventInput { calendar_id: Some(ro.id.clone()), title: "x".into(), start: "2026-11-05T09:00".into(), ..Default::default() }).is_err());
        assert!(api.calendar_link("google", "gid", "nope").await.is_err());
        // Unlinking drops the local mirror only.
        api.calendar_unlink(&cal.id).unwrap();
        assert!(api.calendars().unwrap().iter().all(|c| c.id != cal.id));
        assert!(api.calendar_unlink("personal").is_err());
    }

    #[tokio::test]
    async fn first_sync_pulls_everything_and_later_syncs_use_the_token() {
        let st = Arc::new(Mutex::new(G::default()));
        {
            let mut g = st.lock().unwrap();
            g.all.push(g_event("a1", "\"1\"", "Dentist", "2026-11-05T09:00:00-06:00"));
            g.all.push(json!({"id": "a2", "etag": "\"2\"", "iCalUID": "a2@google.com", "summary": "Birthday", "start": {"date": "2026-11-07"}, "end": {"date": "2026-11-08"}}));
            g.all.push(json!({"id": "a3", "etag": "\"3\"", "iCalUID": "a3@google.com", "summary": "Standup",
                "start": {"dateTime": "2026-11-02T09:00:00-06:00", "timeZone": "America/Chicago"}, "end": {"dateTime": "2026-11-02T09:15:00-06:00", "timeZone": "America/Chicago"},
                "recurrence": ["RRULE:FREQ=WEEKLY;BYDAY=MO", "EXDATE;TZID=America/Chicago:20261109T090000"]}));
        }
        let (api, seen, cal) = google_api(st.clone()).await;
        let reports = api.calendar_sync(None).await.unwrap();
        assert_eq!(reports.len(), 1);
        assert_eq!((reports[0].pulled, reports[0].pushed, reports[0].conflicts), (3, 0, 0), "{:?}", reports[0]);
        assert_eq!(titles(&api, &cal.id), ["Birthday", "Dentist", "Standup"]);
        let standup = api.event_list(Some(&cal.id), 10).unwrap().into_iter().find(|e| e.title == "Standup").unwrap();
        assert_eq!(standup.tz.as_deref(), Some("America/Chicago"));
        assert_eq!(standup.rrule.as_deref().map(|r| r.contains("WEEKLY")), Some(true));
        assert_eq!(standup.exdates, ["2026-11-09T15:00:00Z"]);
        let birthday = api.event_list(Some(&cal.id), 10).unwrap().into_iter().find(|e| e.title == "Birthday").unwrap();
        assert!(birthday.all_day);
        // The first listing asks for a bounded history; the second uses the saved token.
        assert!(seen.lock().unwrap().iter().any(|r| r.method == "GET" && r.query("timeMin").is_some() && r.query("syncToken").is_none()));
        let again = api.calendar_sync(None).await.unwrap();
        assert_eq!((again[0].pulled, again[0].pushed), (0, 0));
        assert!(seen.lock().unwrap().iter().any(|r| r.query("syncToken").as_deref() == Some("T-full")));
        let stored = api.calendars().unwrap().into_iter().find(|c| c.id == cal.id).unwrap();
        assert!(stored.last_sync_at.is_some() && stored.last_sync_error.is_none());
    }

    #[tokio::test]
    async fn local_changes_are_pushed_with_the_etag() {
        let st = Arc::new(Mutex::new(G::default()));
        let (api, _, cal) = google_api(st.clone()).await;
        api.calendar_sync(None).await.unwrap();
        let ev = api.event_create(EventInput { calendar_id: Some(cal.id.clone()), title: "Lunch".into(), start: "2026-11-05T12:00".into(), tz: Some("America/Chicago".into()), attendees: vec!["a@example.com".into()], ..Default::default() }).unwrap();
        let r = api.calendar_sync(None).await.unwrap();
        assert_eq!((r[0].pushed, r[0].errors.len()), (1, 0), "{:?}", r[0]);
        {
            let g = st.lock().unwrap();
            assert_eq!(g.inserted.len(), 1);
            assert_eq!(g.inserted[0]["id"], crate::calendar::gcal::google_id(&ev.id));
            assert_eq!(g.inserted[0]["summary"], "Lunch");
            assert_eq!(g.inserted[0]["start"]["dateTime"], "2026-11-05T18:00:00Z");
            assert_eq!(g.inserted[0]["start"]["timeZone"], "America/Chicago");
            assert_eq!(g.inserted[0]["attendees"][0]["email"], "a@example.com");
        }
        // Nothing left to push; an edit sends If-Match with the etag the server gave.
        assert_eq!(api.calendar_sync(None).await.unwrap()[0].pushed, 0);
        api.event_update(&ev.id, EventInput { title: "Late lunch".into(), start: "2026-11-05T13:00".into(), tz: Some("America/Chicago".into()), ..Default::default() }).unwrap();
        assert_eq!(api.calendar_sync(None).await.unwrap()[0].pushed, 1);
        {
            let g = st.lock().unwrap();
            assert_eq!(g.updated.len(), 1);
            assert_eq!(g.updated[0].1.as_deref(), Some("\"created\""));
            assert_eq!(g.updated[0].2["summary"], "Late lunch");
        }
        api.event_delete(&ev.id, None).unwrap();
        assert_eq!(titles(&api, &cal.id), Vec::<String>::new(), "tombstones are hidden");
        assert_eq!(api.calendar_sync(None).await.unwrap()[0].pushed, 1);
        assert_eq!(st.lock().unwrap().deleted, [crate::calendar::gcal::google_id(&ev.id)]);
        assert!(api.store().list_events(Some(&cal.id), 10).unwrap().is_empty());
        assert!(api.sync_log(10).unwrap().is_empty(), "a clean run logs nothing");
    }

    #[tokio::test]
    async fn conflict_keeps_the_servers_version_and_logs_both() {
        let st = Arc::new(Mutex::new(G::default()));
        st.lock().unwrap().all.push(g_event("a1", "\"1\"", "Dentist", "2026-11-05T09:00:00-06:00"));
        let (api, _, cal) = google_api(st.clone()).await;
        api.calendar_sync(None).await.unwrap();
        let id = api.event_list(Some(&cal.id), 10).unwrap()[0].id.clone();
        api.event_update(&id, EventInput { title: "Dentist (mine)".into(), start: "2026-11-05T11:00".into(), tz: Some("America/Chicago".into()), ..Default::default() }).unwrap();
        st.lock().unwrap().delta.push(g_event("a1", "\"2\"", "Dentist (theirs)", "2026-11-05T14:00:00-06:00"));
        let r = api.calendar_sync(None).await.unwrap();
        assert_eq!((r[0].conflicts, r[0].pushed), (1, 0), "{:?}", r[0]);
        assert_eq!(api.event_get(&id).unwrap().title, "Dentist (theirs)");
        let log = api.sync_log(10).unwrap();
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].kind, "conflict");
        assert!(log[0].detail.contains("Dentist (mine)") && log[0].detail.contains("Dentist (theirs)"), "{}", log[0].detail);
        assert!(st.lock().unwrap().updated.is_empty(), "the server's version must not be overwritten");
        // An identical change on both sides is not a conflict.
        api.event_update(&id, EventInput { title: "Same".into(), start: "2026-11-05T14:00".into(), tz: Some("America/Chicago".into()), ..Default::default() }).unwrap();
        st.lock().unwrap().delta.push(g_event("a1", "\"3\"", "Same", "2026-11-05T14:00:00-06:00"));
        assert_eq!(api.calendar_sync(None).await.unwrap()[0].conflicts, 0);
        assert_eq!(api.sync_log(10).unwrap().len(), 1);
    }

    #[tokio::test]
    async fn deletions_on_either_side_are_reconciled() {
        let st = Arc::new(Mutex::new(G::default()));
        {
            let mut g = st.lock().unwrap();
            for (i, t) in ["One", "Two", "Three"].iter().enumerate() {
                g.all.push(g_event(&format!("e{i}"), "\"1\"", t, &format!("2026-11-0{}T09:00:00-06:00", i + 2)));
            }
        }
        let (api, _, cal) = google_api(st.clone()).await;
        api.calendar_sync(None).await.unwrap();
        let ev = |t: &str| api.event_list(Some(&cal.id), 10).unwrap().into_iter().find(|e| e.title == t).unwrap();
        // One: untouched here, deleted there -> quietly removed.
        // Two: edited here, deleted there -> removed, logged.
        // Three: deleted here, changed there -> back, logged.
        api.event_update(&ev("Two").id, EventInput { title: "Two!".into(), start: "2026-11-03T10:00".into(), tz: Some("America/Chicago".into()), ..Default::default() }).unwrap();
        api.event_delete(&ev("Three").id, None).unwrap();
        st.lock().unwrap().delta.extend([json!({"id": "e0", "status": "cancelled"}), json!({"id": "e1", "status": "cancelled"}), g_event("e2", "\"9\"", "Three (changed)", "2026-11-04T09:00:00-06:00")]);
        let r = api.calendar_sync(None).await.unwrap();
        assert_eq!(r[0].conflicts, 2, "{:?}", r[0]);
        assert_eq!(titles(&api, &cal.id), ["Three (changed)"]);
        let log = api.sync_log(10).unwrap();
        assert_eq!(log.len(), 2);
        assert!(log.iter().any(|l| l.title == "Two!" && l.detail.contains("deleted on the server")));
        assert!(log.iter().any(|l| l.title == "Three (changed)" && l.detail.contains("You deleted this here")));
        assert!(st.lock().unwrap().deleted.is_empty(), "nothing is deleted on the server when it disagrees");
    }

    #[tokio::test]
    async fn an_expired_token_triggers_a_full_resync_that_notices_deletions() {
        let st = Arc::new(Mutex::new(G::default()));
        {
            let mut g = st.lock().unwrap();
            g.all.push(g_event("x1", "\"1\"", "Keep", "2026-11-05T09:00:00-06:00"));
            g.all.push(g_event("x2", "\"1\"", "Vanishes", "2026-11-06T09:00:00-06:00"));
        }
        let (api, _, cal) = google_api(st.clone()).await;
        api.calendar_sync(None).await.unwrap();
        {
            let mut g = st.lock().unwrap();
            g.expired = true;
            g.all.retain(|e| e["id"] == "x1");
        }
        let r = api.calendar_sync(None).await.unwrap();
        assert!(r[0].errors.is_empty(), "{:?}", r[0]);
        assert_eq!(titles(&api, &cal.id), ["Keep"]);
        assert_eq!(r[0].removed, 1);
    }

    #[tokio::test]
    async fn one_occurrence_removed_on_the_server_becomes_an_exception() {
        let st = Arc::new(Mutex::new(G::default()));
        st.lock().unwrap().all.push(json!({"id": "s1", "etag": "\"1\"", "iCalUID": "s1@google.com", "summary": "Standup",
            "start": {"dateTime": "2026-11-02T09:00:00-06:00", "timeZone": "America/Chicago"}, "end": {"dateTime": "2026-11-02T09:15:00-06:00", "timeZone": "America/Chicago"},
            "recurrence": ["RRULE:FREQ=WEEKLY;BYDAY=MO"]}));
        let (api, _, cal) = google_api(st.clone()).await;
        api.calendar_sync(None).await.unwrap();
        st.lock().unwrap().delta.extend([
            json!({"id": "s1_20261109T150000Z", "recurringEventId": "s1", "status": "cancelled", "originalStartTime": {"dateTime": "2026-11-09T09:00:00-06:00", "timeZone": "America/Chicago"}}),
            json!({"id": "s1_20261116T150000Z", "recurringEventId": "s1", "status": "confirmed", "summary": "Moved", "originalStartTime": {"dateTime": "2026-11-16T09:00:00-06:00"}}),
        ]);
        let r = api.calendar_sync(None).await.unwrap();
        assert_eq!(r[0].skipped.len(), 1, "{:?}", r[0]);
        let s = api.event_list(Some(&cal.id), 10).unwrap().remove(0);
        assert_eq!(s.exdates, ["2026-11-09T15:00:00Z"]);
        assert!(!s.exdates.is_empty() && api.sync_log(5).unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_conflicting_push_is_reported_and_the_next_sync_lets_the_server_win() {
        let st = Arc::new(Mutex::new(G::default()));
        st.lock().unwrap().all.push(g_event("a1", "\"1\"", "Dentist", "2026-11-05T09:00:00-06:00"));
        let (api, _, cal) = google_api(st.clone()).await;
        api.calendar_sync(None).await.unwrap();
        let id = api.event_list(Some(&cal.id), 10).unwrap()[0].id.clone();
        api.event_update(&id, EventInput { title: "Mine".into(), start: "2026-11-05T11:00".into(), tz: Some("America/Chicago".into()), ..Default::default() }).unwrap();
        st.lock().unwrap().fail_update_with_412 = true;
        let r = api.calendar_sync(None).await.unwrap();
        assert_eq!((r[0].pushed, r[0].errors.len()), (0, 1));
        assert_eq!(api.event_get(&id).unwrap().title, "Mine", "kept until the server's version arrives");
        {
            let mut g = st.lock().unwrap();
            g.fail_update_with_412 = false;
            g.delta.push(g_event("a1", "\"5\"", "Theirs", "2026-11-05T09:00:00-06:00"));
        }
        let r = api.calendar_sync(None).await.unwrap();
        assert_eq!(r[0].conflicts, 1);
        assert_eq!(api.event_get(&id).unwrap().title, "Theirs");
    }

    #[tokio::test]
    async fn a_server_error_is_recorded_on_the_calendar_and_others_still_sync() {
        let st = Arc::new(Mutex::new(G::default()));
        let (api, _, cal) = google_api(st.clone()).await;
        // Break the endpoint after linking.
        let mut api = api;
        api.google_endpoints.calendar_base = "http://127.0.0.1:1/calendar/v3".into();
        let r = api.calendar_sync(None).await.unwrap();
        assert_eq!(r[0].errors.len(), 1);
        let c = api.calendars().unwrap().into_iter().find(|c| c.id == cal.id).unwrap();
        assert!(c.last_sync_error.is_some());
        assert_eq!(api.sync_log(5).unwrap()[0].kind, "error");
        assert!(api.calendar_sync(Some("personal")).await.is_err(), "a local calendar cannot be synced");
    }

    // ----- CalDAV -----

    #[derive(Default)]
    struct D {
        ctag: u32,
        /// file name -> (etag, ics)
        res: std::collections::BTreeMap<String, (String, String)>,
        puts: Vec<(String, Option<String>, Option<String>, String)>,
        deleted: Vec<(String, Option<String>)>,
        /// PUT answers without an ETag header (some servers do).
        quiet_puts: bool,
        etag_n: u32,
    }

    const COLL: &str = "/123/calendars/home/";

    fn multistatus(inner: &str) -> Resp {
        Resp::xml(207, &format!(r#"<?xml version="1.0"?><d:multistatus xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav" xmlns:cs="http://calendarserver.org/ns/" xmlns:a="http://apple.com/ns/ical/">{inner}</d:multistatus>"#))
    }

    fn piano_ics() -> String {
        [
            "BEGIN:VCALENDAR", "VERSION:2.0", "BEGIN:VEVENT", "UID:ev1@icloud", "DTSTART;TZID=America/Chicago:20261105T090000", "DTEND;TZID=America/Chicago:20261105T100000",
            "RRULE:FREQ=WEEKLY", "SUMMARY:Piano", "BEGIN:VALARM", "TRIGGER:-PT15M", "ACTION:DISPLAY", "DESCRIPTION:Reminder", "END:VALARM", "END:VEVENT",
            "BEGIN:VEVENT", "UID:ev1@icloud", "RECURRENCE-ID;TZID=America/Chicago:20261112T090000", "DTSTART;TZID=America/Chicago:20261112T100000",
            "DTEND;TZID=America/Chicago:20261112T110000", "SUMMARY:Piano (moved)", "END:VEVENT", "END:VCALENDAR", "",
        ]
        .join("\r\n")
    }

    fn simple_ics(uid: &str, title: &str, start: &str) -> String {
        format!("BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:{uid}\r\nDTSTART:{start}\r\nDTEND:{start}\r\nSUMMARY:{title}\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n")
    }

    async fn dav_api(state: Arc<Mutex<D>>) -> (Api, crate::calendar::testserver::Seen, Calendar) {
        let st = state.clone();
        let (port, seen) = serve(move |r: &Req| {
            let mut d = st.lock().unwrap();
            let p = r.path().to_string();
            let depth = r.headers.get("depth").cloned().unwrap_or_default();
            match r.method.as_str() {
                "PROPFIND" if r.body.contains("current-user-principal") => multistatus("<d:response><d:href>/</d:href><d:propstat><d:prop><d:current-user-principal><d:href>/123/principal/</d:href></d:current-user-principal></d:prop></d:propstat></d:response>"),
                "PROPFIND" if r.body.contains("calendar-home-set") => multistatus("<d:response><d:href>/123/principal/</d:href><d:propstat><d:prop><c:calendar-home-set><d:href>/123/calendars/</d:href></c:calendar-home-set></d:prop></d:propstat></d:response>"),
                "PROPFIND" if r.body.contains("displayname") => multistatus(&format!("<d:response><d:href>{COLL}</d:href><d:propstat><d:prop><d:displayname>Home</d:displayname><d:resourcetype><d:collection/><c:calendar/></d:resourcetype><c:supported-calendar-component-set><c:comp name=\"VEVENT\"/></c:supported-calendar-component-set><a:calendar-color>#FF2968FF</a:calendar-color></d:prop></d:propstat></d:response>")),
                "PROPFIND" if r.body.contains("getctag") => multistatus(&format!("<d:response><d:href>{COLL}</d:href><d:propstat><d:prop><cs:getctag>ctag-{}</cs:getctag></d:prop></d:propstat></d:response>", d.ctag)),
                "PROPFIND" if depth == "1" => {
                    let mut x = format!("<d:response><d:href>{COLL}</d:href><d:propstat><d:prop><d:resourcetype><d:collection/><c:calendar/></d:resourcetype></d:prop></d:propstat></d:response>");
                    for (name, (etag, _)) in &d.res {
                        x.push_str(&format!("<d:response><d:href>{COLL}{name}</d:href><d:propstat><d:prop><d:getetag>{etag}</d:getetag><d:resourcetype/></d:prop></d:propstat></d:response>"));
                    }
                    multistatus(&x)
                }
                "PROPFIND" => {
                    let name = p.rsplit('/').next().unwrap_or("");
                    match d.res.get(name) {
                        Some((etag, _)) => multistatus(&format!("<d:response><d:href>{p}</d:href><d:propstat><d:prop><d:getetag>{etag}</d:getetag></d:prop></d:propstat></d:response>")),
                        None => Resp::empty(404),
                    }
                }
                "REPORT" => {
                    let mut x = String::new();
                    for part in r.body.split("<d:href>").skip(1) {
                        let href = part.split("</d:href>").next().unwrap();
                        let name = href.rsplit('/').next().unwrap();
                        if let Some((etag, ics)) = d.res.get(name) {
                            x.push_str(&format!("<d:response><d:href>{href}</d:href><d:propstat><d:prop><d:getetag>{etag}</d:getetag><c:calendar-data>{}</c:calendar-data></d:prop></d:propstat></d:response>", ics.replace('&', "&amp;")));
                        }
                    }
                    multistatus(&x)
                }
                "PUT" => {
                    let name = p.rsplit('/').next().unwrap().to_string();
                    let (inm, im) = (r.headers.get("if-none-match").cloned(), r.headers.get("if-match").cloned());
                    d.puts.push((name.clone(), im.clone(), inm.clone(), r.body.clone()));
                    let existing = d.res.get(&name).map(|(e, _)| e.clone());
                    if inm.as_deref() == Some("*") && existing.is_some() || im.is_some() && im != existing {
                        return Resp::empty(412);
                    }
                    d.etag_n += 1;
                    let etag = format!("\"p{}\"", d.etag_n);
                    d.res.insert(name, (etag.clone(), r.body.clone()));
                    d.ctag += 1;
                    let resp = Resp::empty(201);
                    if d.quiet_puts { resp } else { resp.with("ETag", &etag) }
                }
                "DELETE" => {
                    let name = p.rsplit('/').next().unwrap().to_string();
                    d.deleted.push((name.clone(), r.headers.get("if-match").cloned()));
                    d.res.remove(&name);
                    d.ctag += 1;
                    Resp::empty(204)
                }
                _ => Resp::empty(400),
            }
        })
        .await;
        let api = test_api().with_caldav_base(&format!("http://127.0.0.1:{port}/"));
        api.add_account(crate::api::AddAccountRequest {
            name: "Fake".into(), replace: false, email: "fred@example.com".into(), provider: Provider::Custom, password: "pw".into(),
            imap_host: Some("127.0.0.1".into()), imap_port: Some(1), smtp_host: None, smtp_port: None,
        })
        .unwrap();
        let account = api.store().list_accounts().unwrap().remove(0);
        let cal = api.calendar_link("caldav", &account.id, &format!("http://127.0.0.1:{port}{COLL}")).await.unwrap();
        (api, seen, cal)
    }

    #[tokio::test]
    async fn caldav_pull_keeps_alarms_and_overrides_through_a_local_edit() {
        let st = Arc::new(Mutex::new(D::default()));
        st.lock().unwrap().res.insert("ev1.ics".into(), ("\"v1\"".into(), piano_ics()));
        let (api, _, cal) = dav_api(st.clone()).await;
        assert_eq!((cal.kind.as_str(), cal.name.as_str(), cal.color.as_str()), ("caldav", "Home", "#ff2968"));
        let r = api.calendar_sync(None).await.unwrap();
        assert_eq!((r[0].pulled, r[0].errors.len(), r[0].skipped.len()), (1, 0, 0), "{:?}", r[0]);
        let ev = api.event_list(Some(&cal.id), 10).unwrap().remove(0);
        assert_eq!((ev.title.as_str(), ev.tz.as_deref()), ("Piano", Some("America/Chicago")));
        assert!(ev.rrule.is_some());
        // Edit here: the PUT replaces the version we saw and still carries the alarm and the override.
        api.event_update(&ev.id, EventInput { title: "Piano lesson".into(), start: "2026-11-05T09:00".into(), end: Some("2026-11-05T10:00".into()), tz: Some("America/Chicago".into()), rrule: ev.rrule.clone(), ..Default::default() }).unwrap();
        let r = api.calendar_sync(None).await.unwrap();
        assert_eq!((r[0].pushed, r[0].errors.len()), (1, 0), "{:?}", r[0]);
        let g = st.lock().unwrap();
        let (name, if_match, if_none, body) = g.puts.last().unwrap();
        assert_eq!((name.as_str(), if_match.as_deref(), if_none.as_deref()), ("ev1.ics", Some("\"v1\""), None));
        assert!(body.contains("SUMMARY:Piano lesson") && body.contains("BEGIN:VALARM") && body.contains("TRIGGER:-PT15M"), "{body}");
        assert!(body.contains("RECURRENCE-ID;TZID=America/Chicago:20261112T090000") && body.contains("SUMMARY:Piano (moved)"), "{body}");
        assert_eq!(body.matches("BEGIN:VEVENT").count(), 2);
    }

    #[tokio::test]
    async fn caldav_new_events_deletes_and_server_side_changes() {
        let st = Arc::new(Mutex::new(D { quiet_puts: true, ..Default::default() }));
        {
            let mut d = st.lock().unwrap();
            d.res.insert("a.ics".into(), ("\"a1\"".into(), simple_ics("a@x", "Alpha", "20261105T150000Z")));
            d.res.insert("b.ics".into(), ("\"b1\"".into(), simple_ics("b@x", "Beta", "20261106T150000Z")));
        }
        let (api, seen, cal) = dav_api(st.clone()).await;
        assert_eq!(api.calendar_sync(None).await.unwrap()[0].pulled, 2);
        // A new local event: created with If-None-Match: * at <collection>/<id>.ics, etag fetched when the PUT gives none.
        let mine = api.event_create(EventInput { calendar_id: Some(cal.id.clone()), title: "Mine".into(), start: "2026-11-07T10:00".into(), tz: Some("America/Chicago".into()), ..Default::default() }).unwrap();
        assert_eq!(api.calendar_sync(None).await.unwrap()[0].pushed, 1);
        {
            let d = st.lock().unwrap();
            let (name, im, inm, _) = d.puts.last().unwrap();
            assert_eq!((name.clone(), im.clone(), inm.as_deref()), (format!("{}.ics", mine.id), None, Some("*")));
        }
        // Our own change must not come back as a change.
        let r = api.calendar_sync(None).await.unwrap();
        assert_eq!((r[0].pulled, r[0].pushed, r[0].conflicts), (0, 0, 0), "{:?}", r[0]);
        // Unchanged server: only the change-tag probe, no listing.
        let before = seen.lock().unwrap().len();
        api.calendar_sync(None).await.unwrap();
        let probes: Vec<String> = seen.lock().unwrap()[before..].iter().map(|r| format!("{} {}", r.method, r.headers.get("depth").cloned().unwrap_or_default())).collect();
        assert_eq!(probes, ["PROPFIND 0"], "{probes:?}");
        // The server changes Alpha and drops Beta; we delete Mine.
        {
            let mut d = st.lock().unwrap();
            d.res.insert("a.ics".into(), ("\"a2\"".into(), simple_ics("a@x", "Alpha v2", "20261105T150000Z")));
            d.res.remove("b.ics");
            d.ctag += 1;
        }
        api.event_delete(&mine.id, None).unwrap();
        let r = api.calendar_sync(None).await.unwrap();
        assert_eq!((r[0].pulled, r[0].removed, r[0].pushed, r[0].conflicts), (1, 1, 1, 0), "{:?}", r[0]);
        assert_eq!(titles(&api, &cal.id), ["Alpha v2"]);
        let d = st.lock().unwrap();
        assert_eq!(d.deleted.len(), 1);
        assert_eq!(d.deleted[0].0, format!("{}.ics", mine.id));
        assert!(d.deleted[0].1.is_some(), "delete is conditional on the version we saw");
    }

    #[tokio::test]
    async fn caldav_conflict_server_wins_and_is_logged() {
        let st = Arc::new(Mutex::new(D::default()));
        st.lock().unwrap().res.insert("a.ics".into(), ("\"a1\"".into(), simple_ics("a@x", "Alpha", "20261105T150000Z")));
        let (api, _, cal) = dav_api(st.clone()).await;
        api.calendar_sync(None).await.unwrap();
        let ev = api.event_list(Some(&cal.id), 10).unwrap().remove(0);
        api.event_update(&ev.id, EventInput { title: "Alpha (mine)".into(), start: "2026-11-05T09:00".into(), tz: Some("America/Chicago".into()), ..Default::default() }).unwrap();
        {
            let mut d = st.lock().unwrap();
            d.res.insert("a.ics".into(), ("\"a2\"".into(), simple_ics("a@x", "Alpha (theirs)", "20261105T150000Z")));
            d.ctag += 1;
        }
        let r = api.calendar_sync(None).await.unwrap();
        assert_eq!((r[0].conflicts, r[0].pushed), (1, 0), "{:?}", r[0]);
        assert_eq!(api.event_get(&ev.id).unwrap().title, "Alpha (theirs)");
        let log = api.sync_log(5).unwrap();
        assert!(log[0].detail.contains("Alpha (mine)") && log[0].detail.contains("Alpha (theirs)"));
        assert!(st.lock().unwrap().puts.is_empty());
    }

    #[test]
    fn google_json_for_all_day_and_recurring_events() {
        let mut ev = Event {
            id: "i".into(), calendar_id: "c".into(), uid: "u@mailcore".into(), title: "Trash".into(), description: None, location: None,
            start_utc: "2026-11-02T06:00:00Z".into(), end_utc: "2026-11-03T06:00:00Z".into(), all_day: true, tz: Some("America/Chicago".into()),
            rrule: Some("FREQ=WEEKLY;BYDAY=MO".into()), exdates: vec!["2026-11-09T06:00:00Z".into()], status: "confirmed".into(), organizer: None,
            attendees: vec![], source_message_id: None, sequence: 3, updated_at: String::new(),
        };
        let v = crate::calendar::gcal::to_google(&ev).unwrap();
        assert_eq!(v["start"]["date"], "2026-11-02");
        assert_eq!(v["end"]["date"], "2026-11-03");
        assert_eq!(v["recurrence"][0], "RRULE:FREQ=WEEKLY;BYDAY=MO");
        assert_eq!(v["recurrence"][1], "EXDATE;VALUE=DATE:20261109");
        ev.all_day = false;
        ev.start_utc = "2026-11-02T15:00:00Z".into();
        ev.end_utc = "2026-11-02T16:00:00Z".into();
        let v = crate::calendar::gcal::to_google(&ev).unwrap();
        assert_eq!(v["recurrence"][1], "EXDATE:20261109T060000Z");
        assert_eq!(v["start"]["timeZone"], "America/Chicago");
        assert!(crate::calendar::gcal::google_id("0123456789abcdef0123456789abcdef").len() >= 5);
        assert_eq!(crate::calendar::gcal::google_id("AB-cd"), "abcd");
    }
}
