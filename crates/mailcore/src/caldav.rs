//! CalDAV discovery (RFC 4791 / RFC 6764): principal → calendar-home-set → calendars. Phase 1
//! proves the credentials and finds the collections; event sync is Phase 5/6.

use serde::{Deserialize, Serialize};
use url::Url;

use crate::error::{Error, Result};

const NS_DAV: &str = "DAV:";
const NS_CALDAV: &str = "urn:ietf:params:xml:ns:caldav";
const NS_CS: &str = "http://calendarserver.org/ns/";
const NS_APPLE: &str = "http://apple.com/ns/ical/";

pub const ICLOUD_CALDAV: &str = "https://caldav.icloud.com/";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CalDavHome {
    pub principal: String,
    pub home: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CalDavCalendar {
    pub href: String,
    pub display_name: String,
    pub ctag: Option<String>,
    pub color: Option<String>,
    pub supports_events: bool,
    pub supports_todos: bool,
}

pub struct CalDavClient {
    http: reqwest::Client,
    base: Url,
    username: String,
    password: String,
}

impl CalDavClient {
    pub fn new(base: &str, username: &str, password: &str) -> Result<CalDavClient> {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(30))
            .build()?;
        Ok(CalDavClient {
            http,
            base: Url::parse(base).map_err(|e| Error::CalDav(format!("bad base url: {e}")))?,
            username: username.to_string(),
            password: password.to_string(),
        })
    }

    /// PROPFIND with manual redirect handling (servers 301 PROPFIND; reqwest would turn it into GET).
    async fn propfind(&self, url: &Url, depth: u8, body: &str) -> Result<String> {
        let mut url = url.clone();
        for _ in 0..5 {
            let resp = self
                .http
                .request(reqwest::Method::from_bytes(b"PROPFIND").unwrap(), url.clone())
                .basic_auth(&self.username, Some(&self.password))
                .header("Depth", depth.to_string())
                .header("Content-Type", "application/xml; charset=utf-8")
                .body(body.to_string())
                .send()
                .await?;
            let status = resp.status();
            if status.is_redirection() {
                let loc = resp
                    .headers()
                    .get("location")
                    .and_then(|v| v.to_str().ok())
                    .ok_or_else(|| Error::CalDav(format!("redirect {status} without Location")))?;
                url = url.join(loc).map_err(|e| Error::CalDav(e.to_string()))?;
                continue;
            }
            if status.as_u16() == 401 || status.as_u16() == 403 {
                return Err(Error::CalDav(format!("authentication failed ({status}) for {}", self.username)));
            }
            if !(status.is_success() || status.as_u16() == 207) {
                let text = resp.text().await.unwrap_or_default();
                return Err(Error::CalDav(format!("PROPFIND {url} returned {status}: {}", text.chars().take(200).collect::<String>())));
            }
            return Ok(resp.text().await?);
        }
        Err(Error::CalDav("too many redirects".into()))
    }

    pub async fn discover(&self) -> Result<CalDavHome> {
        let body = r#"<?xml version="1.0" encoding="utf-8"?><d:propfind xmlns:d="DAV:"><d:prop><d:current-user-principal/></d:prop></d:propfind>"#;
        let xml = self.propfind(&self.base, 0, body).await?;
        let principal_href = parse_single_href(&xml, NS_DAV, "current-user-principal")
            .ok_or_else(|| Error::CalDav("server did not report current-user-principal".into()))?;
        let principal = self.base.join(&principal_href).map_err(|e| Error::CalDav(e.to_string()))?;

        let body = r#"<?xml version="1.0" encoding="utf-8"?><d:propfind xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav"><d:prop><c:calendar-home-set/></d:prop></d:propfind>"#;
        let xml = self.propfind(&principal, 0, body).await?;
        let home_href = parse_single_href(&xml, NS_CALDAV, "calendar-home-set")
            .ok_or_else(|| Error::CalDav("principal has no calendar-home-set".into()))?;
        let home = principal.join(&home_href).map_err(|e| Error::CalDav(e.to_string()))?;
        Ok(CalDavHome { principal: principal.to_string(), home: home.to_string() })
    }

    pub async fn list_calendars(&self, home: &str) -> Result<Vec<CalDavCalendar>> {
        let home = Url::parse(home).map_err(|e| Error::CalDav(e.to_string()))?;
        let body = r#"<?xml version="1.0" encoding="utf-8"?><d:propfind xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav" xmlns:cs="http://calendarserver.org/ns/" xmlns:ical="http://apple.com/ns/ical/"><d:prop><d:displayname/><d:resourcetype/><cs:getctag/><c:supported-calendar-component-set/><ical:calendar-color/></d:prop></d:propfind>"#;
        let xml = self.propfind(&home, 1, body).await?;
        let mut cals = parse_calendars(&xml)?;
        for c in &mut cals {
            if let Ok(abs) = home.join(&c.href) {
                c.href = abs.to_string();
            }
        }
        Ok(cals)
    }
}

/// First `<d:href>` inside the named property of a multistatus response.
pub fn parse_single_href(xml: &str, ns: &str, prop: &str) -> Option<String> {
    let doc = roxmltree::Document::parse(xml).ok()?;
    doc.descendants()
        .find(|n| n.has_tag_name((ns, prop)))
        .and_then(|p| p.descendants().find(|n| n.has_tag_name((NS_DAV, "href"))))
        .and_then(|h| h.text())
        .map(|s| s.trim().to_string())
}

/// Every `<d:response>` whose resourcetype includes `<c:calendar/>`.
pub fn parse_calendars(xml: &str) -> Result<Vec<CalDavCalendar>> {
    let doc = roxmltree::Document::parse(xml).map_err(|e| Error::CalDav(format!("bad multistatus xml: {e}")))?;
    let mut out = Vec::new();
    for resp in doc.descendants().filter(|n| n.has_tag_name((NS_DAV, "response"))) {
        let href = resp
            .children()
            .find(|n| n.has_tag_name((NS_DAV, "href")))
            .and_then(|n| n.text())
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        let is_calendar = resp
            .descendants()
            .filter(|n| n.has_tag_name((NS_DAV, "resourcetype")))
            .any(|rt| rt.children().any(|c| c.has_tag_name((NS_CALDAV, "calendar"))));
        if !is_calendar {
            continue;
        }
        let text_of = |ns: &str, name: &str| -> Option<String> {
            resp.descendants().find(|n| n.has_tag_name((ns, name))).and_then(|n| n.text()).map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
        };
        let comps: Vec<String> = resp
            .descendants()
            .filter(|n| n.has_tag_name((NS_CALDAV, "comp")))
            .filter_map(|n| n.attribute("name").map(|s| s.to_ascii_uppercase()))
            .collect();
        out.push(CalDavCalendar {
            display_name: text_of(NS_DAV, "displayname").unwrap_or_else(|| href.trim_end_matches('/').rsplit('/').next().unwrap_or("").to_string()),
            href,
            ctag: text_of(NS_CS, "getctag"),
            color: text_of(NS_APPLE, "calendar-color"),
            supports_events: comps.is_empty() || comps.iter().any(|c| c == "VEVENT"),
            supports_todos: comps.iter().any(|c| c == "VTODO"),
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PRINCIPAL: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<multistatus xmlns="DAV:"><response><href>/</href><propstat><prop>
<current-user-principal><href>/123456789/principal/</href></current-user-principal>
</prop><status>HTTP/1.1 200 OK</status></propstat></response></multistatus>"#;

    const HOME: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<multistatus xmlns="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav"><response><href>/123456789/principal/</href><propstat><prop>
<C:calendar-home-set><href>https://p99-caldav.icloud.com:443/123456789/calendars/</href></C:calendar-home-set>
</prop><status>HTTP/1.1 200 OK</status></propstat></response></multistatus>"#;

    const CALS: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<multistatus xmlns="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav" xmlns:CS="http://calendarserver.org/ns/" xmlns:ICAL="http://apple.com/ns/ical/">
<response><href>/123456789/calendars/</href><propstat><prop><displayname>Fred</displayname><resourcetype><collection/></resourcetype></prop><status>HTTP/1.1 200 OK</status></propstat></response>
<response><href>/123456789/calendars/home/</href><propstat><prop><displayname>Home</displayname><resourcetype><collection/><C:calendar/></resourcetype><CS:getctag>ctag-1</CS:getctag><C:supported-calendar-component-set><C:comp name="VEVENT"/></C:supported-calendar-component-set><ICAL:calendar-color>#FF2968FF</ICAL:calendar-color></prop><status>HTTP/1.1 200 OK</status></propstat></response>
<response><href>/123456789/calendars/tasks/</href><propstat><prop><displayname>Reminders</displayname><resourcetype><collection/><C:calendar/></resourcetype><C:supported-calendar-component-set><C:comp name="VTODO"/></C:supported-calendar-component-set></prop><status>HTTP/1.1 200 OK</status></propstat></response>
<response><href>/123456789/calendars/inbox/</href><propstat><prop><resourcetype><collection/><C:schedule-inbox/></resourcetype></prop><status>HTTP/1.1 200 OK</status></propstat></response>
</multistatus>"#;

    #[test]
    fn discovery_hrefs() {
        assert_eq!(parse_single_href(PRINCIPAL, NS_DAV, "current-user-principal").as_deref(), Some("/123456789/principal/"));
        assert_eq!(parse_single_href(HOME, NS_CALDAV, "calendar-home-set").as_deref(), Some("https://p99-caldav.icloud.com:443/123456789/calendars/"));
        assert_eq!(parse_single_href(HOME, NS_DAV, "nope"), None);
    }

    #[test]
    fn calendars_only_and_component_flags() {
        let cals = parse_calendars(CALS).unwrap();
        assert_eq!(cals.len(), 2, "collection root and schedule-inbox are skipped");
        assert_eq!(cals[0].display_name, "Home");
        assert_eq!(cals[0].ctag.as_deref(), Some("ctag-1"));
        assert_eq!(cals[0].color.as_deref(), Some("#FF2968FF"));
        assert!(cals[0].supports_events && !cals[0].supports_todos);
        assert_eq!(cals[1].display_name, "Reminders");
        assert!(!cals[1].supports_events && cals[1].supports_todos);
    }
}
