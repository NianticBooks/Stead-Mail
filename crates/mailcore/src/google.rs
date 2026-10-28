//! Google: OAuth client definition (Calendar scopes only, per the 2026-10-27 decision) and the
//! two REST calls Phase 1 needs: who am I, and which calendars do I have.

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::oauth::OAuthClient;

/// Sensitive (not restricted) scopes: no CASA assessment required. Gmail scopes are deliberately
/// absent; mail uses an app password over IMAP/SMTP.
pub const SCOPES: &[&str] = &[
    "https://www.googleapis.com/auth/calendar.events",
    "https://www.googleapis.com/auth/calendar.calendarlist.readonly",
    "https://www.googleapis.com/auth/userinfo.email",
];

#[derive(Debug, Clone)]
pub struct GoogleEndpoints {
    pub auth_url: String,
    pub token_url: String,
    pub userinfo_url: String,
    pub calendar_base: String,
}

impl Default for GoogleEndpoints {
    fn default() -> Self {
        GoogleEndpoints {
            auth_url: "https://accounts.google.com/o/oauth2/v2/auth".into(),
            token_url: "https://oauth2.googleapis.com/token".into(),
            userinfo_url: "https://www.googleapis.com/oauth2/v3/userinfo".into(),
            calendar_base: "https://www.googleapis.com/calendar/v3".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ClientConfig {
    pub client_id: String,
    pub client_secret: Option<String>,
}

pub fn oauth_client(cfg: &ClientConfig, ep: &GoogleEndpoints) -> OAuthClient {
    OAuthClient {
        client_id: cfg.client_id.clone(),
        client_secret: cfg.client_secret.clone(),
        auth_url: ep.auth_url.clone(),
        token_url: ep.token_url.clone(),
        scopes: SCOPES.iter().map(|s| s.to_string()).collect(),
        // offline + consent guarantee a refresh token on every login.
        extra_auth_params: vec![("access_type".into(), "offline".into()), ("prompt".into(), "consent".into())],
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CalendarSummary {
    pub id: String,
    pub summary: String,
    #[serde(default)]
    pub primary: bool,
    #[serde(default)]
    pub access_role: String,
    #[serde(default)]
    pub time_zone: Option<String>,
    #[serde(default)]
    pub background_color: Option<String>,
}

#[derive(Deserialize)]
struct UserInfo {
    email: String,
}

#[derive(Deserialize)]
struct CalendarListItem {
    id: String,
    #[serde(default)]
    summary: String,
    #[serde(default)]
    primary: bool,
    #[serde(default, rename = "accessRole")]
    access_role: String,
    #[serde(default, rename = "timeZone")]
    time_zone: Option<String>,
    #[serde(default, rename = "backgroundColor")]
    background_color: Option<String>,
}

#[derive(Deserialize)]
struct CalendarList {
    #[serde(default)]
    items: Vec<CalendarListItem>,
    #[serde(default, rename = "nextPageToken")]
    next_page_token: Option<String>,
}

async fn check(resp: reqwest::Response, what: &str) -> Result<reqwest::Response> {
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }
    let text = resp.text().await.unwrap_or_default();
    Err(Error::Http(format!("{what}: HTTP {status}: {}", text.chars().take(300).collect::<String>())))
}

pub async fn userinfo_email(http: &reqwest::Client, ep: &GoogleEndpoints, access_token: &str) -> Result<String> {
    let resp = http.get(&ep.userinfo_url).bearer_auth(access_token).send().await?;
    let info: UserInfo = check(resp, "userinfo").await?.json().await?;
    Ok(info.email)
}

pub async fn list_calendars(http: &reqwest::Client, ep: &GoogleEndpoints, access_token: &str) -> Result<Vec<CalendarSummary>> {
    let mut out = Vec::new();
    let mut page: Option<String> = None;
    loop {
        let mut req = http.get(format!("{}/users/me/calendarList", ep.calendar_base)).bearer_auth(access_token);
        if let Some(p) = &page {
            req = req.query(&[("pageToken", p)]);
        }
        let list: CalendarList = check(req.send().await?, "calendarList").await?.json().await?;
        out.extend(list.items.into_iter().map(|i| CalendarSummary {
            id: i.id,
            summary: i.summary,
            primary: i.primary,
            access_role: i.access_role,
            time_zone: i.time_zone,
            background_color: i.background_color,
        }));
        match list.next_page_token {
            Some(t) => page = Some(t),
            None => break,
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oauth::fake_http;

    #[tokio::test]
    async fn parses_calendar_list_and_userinfo() {
        let (p, seen) = fake_http::start(
            200,
            r#"{"items":[{"id":"fred@gmail.com","summary":"Fred","primary":true,"accessRole":"owner","timeZone":"America/New_York"},{"id":"abc@group.calendar.google.com","summary":"Family","accessRole":"writer"}]}"#,
            1,
        )
        .await;
        let ep = GoogleEndpoints { calendar_base: format!("http://127.0.0.1:{p}/calendar/v3"), ..Default::default() };
        let cals = list_calendars(&crate::http_client(), &ep, "TOKEN").await.unwrap();
        assert_eq!(cals.len(), 2);
        assert!(cals[0].primary);
        assert_eq!(cals[1].access_role, "writer");
        assert_eq!(cals[1].time_zone, None);
        assert_eq!(seen.lock().unwrap().requests[0].0, "/calendar/v3/users/me/calendarList");

        let (p, _) = fake_http::start(200, r#"{"sub":"1","email":"fred@gmail.com","email_verified":true}"#, 1).await;
        let ep = GoogleEndpoints { userinfo_url: format!("http://127.0.0.1:{p}/userinfo"), ..Default::default() };
        assert_eq!(userinfo_email(&crate::http_client(), &ep, "T").await.unwrap(), "fred@gmail.com");

        let (p, _) = fake_http::start(401, r#"{"error":{"code":401,"message":"Invalid Credentials"}}"#, 1).await;
        let ep = GoogleEndpoints { calendar_base: format!("http://127.0.0.1:{p}/v3"), ..Default::default() };
        let err = list_calendars(&crate::http_client(), &ep, "bad").await.unwrap_err().to_string();
        assert!(err.contains("401"), "{err}");
    }

    #[test]
    fn google_client_requests_offline_refresh_tokens() {
        let c = oauth_client(&ClientConfig { client_id: "x".into(), client_secret: None }, &GoogleEndpoints::default());
        assert!(c.extra_auth_params.iter().any(|(k, v)| k == "access_type" && v == "offline"));
        assert!(c.scopes.iter().all(|s| !s.contains("gmail")), "no Gmail scopes allowed");
    }
}
