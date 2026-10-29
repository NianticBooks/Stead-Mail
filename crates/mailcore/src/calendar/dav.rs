//! CalDAV event sync: diff the server's ETags against ours, fetch what changed, and translate
//! between iCalendar resources and our events.

use std::collections::HashMap;

use chrono_tz::Tz;

use super::gcal::{Pull, RemoteEvent};
use super::ical;
use super::model::Event;
use crate::caldav::CalDavClient;
use crate::error::Result;

/// What we know about the resources already mirrored: remote id (URL) → etag.
pub type Known = HashMap<String, String>;

/// Pull the changes of one collection. `ctag` is the collection's change tag from last time; when
/// it has not moved, nothing changed on the server and no listing is needed.
pub async fn pull(client: &CalDavClient, collection: &str, ctag_before: Option<&str>, known: &Known, default_tz: Tz, force: bool) -> Result<Pull> {
    let ctag = client.ctag(collection).await?;
    let mut pull = Pull { full: true, next_token: ctag.clone(), ..Default::default() };
    if !force && ctag.is_some() && ctag.as_deref() == ctag_before {
        // Unchanged: report no deltas, and do not let "absent from the listing" delete anything.
        pull.full = false;
        return Ok(pull);
    }
    let listing = client.list_etags(collection).await?;
    let seen: std::collections::HashSet<&String> = listing.iter().map(|(u, _)| u).collect();
    pull.deleted = known.keys().filter(|k| !seen.contains(k)).cloned().collect();
    let changed: Vec<String> = listing.iter().filter(|(u, e)| known.get(u) != Some(e)).map(|(u, _)| u.clone()).collect();
    if changed.is_empty() {
        return Ok(pull);
    }
    for (url, etag, text) in client.multiget(collection, &changed).await? {
        let mut skipped = Vec::new();
        let events = crate::ai::ics::parse(&text);
        let Some(master) = events.iter().find(|e| e.recurrence_id.is_none()) else {
            pull.skipped.push(format!("{url}: no event in this resource"));
            continue;
        };
        match ical::import_one(master, default_tz, &mut skipped) {
            Some(item) => {
                let uid = item.uid.clone().unwrap_or_default();
                let extra = ical::extract_extra(&text, &uid);
                pull.upserts.push(RemoteEvent { remote_id: url, etag: Some(etag), item, extra: Some(extra).filter(|x| !x.is_empty()) });
            }
            None => pull.skipped.extend(skipped),
        }
    }
    Ok(pull)
}

/// File name for a new resource: the event id is already URL-safe.
pub fn href_for(collection: &str, ev: &Event) -> String {
    format!("{}/{}.ics", collection.trim_end_matches('/'), ev.id)
}

pub fn body_for(ev: &Event, extra: Option<&ical::Extra>) -> String {
    ical::export_resource(ev, extra)
}
