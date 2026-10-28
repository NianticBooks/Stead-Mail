//! OAuth 2.0 authorization-code flow for native apps (RFC 8252): PKCE (RFC 7636) plus a loopback
//! redirect on 127.0.0.1. Provider-agnostic; `google.rs` supplies the endpoints and scopes.

use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use crate::error::{Error, Result};

#[derive(Debug, Clone)]
pub struct OAuthClient {
    pub client_id: String,
    /// Google issues a "secret" even for desktop clients; it is not confidential (RFC 8252 §8.5).
    pub client_secret: Option<String>,
    pub auth_url: String,
    pub token_url: String,
    pub scopes: Vec<String>,
    /// Extra query parameters for the authorization request (e.g. Google's access_type=offline).
    pub extra_auth_params: Vec<(String, String)>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TokenSet {
    pub access_token: String,
    pub refresh_token: Option<String>,
    /// Unix seconds.
    pub expires_at: i64,
    pub scope: Option<String>,
    pub token_type: Option<String>,
}

impl TokenSet {
    pub fn is_fresh(&self, margin_secs: i64) -> bool {
        self.expires_at - margin_secs > now_unix()
    }
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<i64>,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    token_type: Option<String>,
}

#[derive(Deserialize)]
struct TokenError {
    error: String,
    #[serde(default)]
    error_description: Option<String>,
}

pub fn now_unix() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// Generate a PKCE verifier (43 chars, base64url of 32 random bytes) and its S256 challenge.
pub fn pkce_pair() -> Result<(String, String)> {
    let mut raw = [0u8; 32];
    getrandom::fill(&mut raw).map_err(|e| Error::OAuth(e.to_string()))?;
    let verifier = URL_SAFE_NO_PAD.encode(raw);
    Ok((verifier.clone(), pkce_challenge(&verifier)))
}

pub fn pkce_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

fn random_state() -> Result<String> {
    let mut raw = [0u8; 16];
    getrandom::fill(&mut raw).map_err(|e| Error::OAuth(e.to_string()))?;
    Ok(URL_SAFE_NO_PAD.encode(raw))
}

/// Build the authorization URL for a given redirect URI, PKCE challenge and state.
pub fn authorize_url(client: &OAuthClient, redirect_uri: &str, challenge: &str, state: &str) -> Result<url::Url> {
    let mut u = url::Url::parse(&client.auth_url).map_err(|e| Error::OAuth(format!("bad auth_url: {e}")))?;
    {
        let mut q = u.query_pairs_mut();
        q.append_pair("client_id", &client.client_id)
            .append_pair("redirect_uri", redirect_uri)
            .append_pair("response_type", "code")
            .append_pair("scope", &client.scopes.join(" "))
            .append_pair("code_challenge", challenge)
            .append_pair("code_challenge_method", "S256")
            .append_pair("state", state);
        for (k, v) in &client.extra_auth_params {
            q.append_pair(k, v);
        }
    }
    Ok(u)
}

/// Parse the loopback redirect request target (`/callback?code=..&state=..`).
pub fn parse_callback(target: &str) -> std::result::Result<(String, String), String> {
    let u = url::Url::parse(&format!("http://127.0.0.1{target}")).map_err(|e| e.to_string())?;
    if u.path() != "/callback" {
        return Err(format!("unexpected path {}", u.path()));
    }
    let mut code = None;
    let mut state = None;
    let mut error = None;
    for (k, v) in u.query_pairs() {
        match k.as_ref() {
            "code" => code = Some(v.into_owned()),
            "state" => state = Some(v.into_owned()),
            "error" => error = Some(v.into_owned()),
            _ => {}
        }
    }
    if let Some(e) = error {
        return Err(format!("provider returned error '{e}'"));
    }
    match (code, state) {
        (Some(c), Some(s)) => Ok((c, s)),
        _ => Err("missing code or state".into()),
    }
}

const CALLBACK_HTML: &str = "<!doctype html><html><head><meta charset=utf-8><title>Mailcore</title>\
<style>body{background:#0f1115;color:#e6e9ef;font-family:system-ui,sans-serif;display:grid;place-items:center;height:100vh;margin:0}\
div{text-align:center}h1{font-weight:600}p{color:#aab2c0}</style></head><body><div><h1>Signed in</h1>\
<p>Mailcore received the authorization. You can close this tab.</p></div></body></html>";

/// Run the whole flow: start a loopback listener, hand the authorization URL to `present`
/// (print it, open a browser), wait for the redirect, verify state, exchange the code.
pub async fn authorize(
    client: &OAuthClient,
    http: &reqwest::Client,
    timeout: Duration,
    present: impl FnOnce(&str),
) -> Result<TokenSet> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    let redirect_uri = format!("http://127.0.0.1:{port}/callback");
    let (verifier, challenge) = pkce_pair()?;
    let state = random_state()?;
    let url = authorize_url(client, &redirect_uri, &challenge, &state)?;
    present(url.as_str());

    let (code, returned_state) = tokio::time::timeout(timeout, wait_for_callback(&listener))
        .await
        .map_err(|_| Error::OAuth(format!("no authorization callback within {}s", timeout.as_secs())))??;
    if returned_state != state {
        return Err(Error::OAuth("state mismatch in callback (possible CSRF); aborting".into()));
    }
    exchange_code(client, http, &code, &verifier, &redirect_uri).await
}

async fn wait_for_callback(listener: &TcpListener) -> Result<(String, String)> {
    loop {
        let (mut sock, _) = listener.accept().await?;
        let mut buf = Vec::with_capacity(2048);
        let mut tmp = [0u8; 1024];
        // Read until end of headers; the callback is a bare GET so there is no body.
        loop {
            let n = sock.read(&mut tmp).await?;
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&tmp[..n]);
            if buf.windows(4).any(|w| w == b"\r\n\r\n") || buf.len() > 16 * 1024 {
                break;
            }
        }
        let request_line = String::from_utf8_lossy(&buf).lines().next().unwrap_or_default().to_string();
        let target = request_line.split_whitespace().nth(1).unwrap_or("/").to_string();
        match parse_callback(&target) {
            Ok(pair) => {
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    CALLBACK_HTML.len(),
                    CALLBACK_HTML
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.shutdown().await;
                return Ok(pair);
            }
            Err(e) if target.starts_with("/callback") => {
                let body = format!("Authorization failed: {e}");
                let resp = format!("HTTP/1.1 400 Bad Request\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body);
                let _ = sock.write_all(resp.as_bytes()).await;
                return Err(Error::OAuth(e));
            }
            Err(_) => {
                // favicon.ico and friends: answer and keep waiting.
                let _ = sock.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
            }
        }
    }
}

pub async fn exchange_code(
    client: &OAuthClient,
    http: &reqwest::Client,
    code: &str,
    verifier: &str,
    redirect_uri: &str,
) -> Result<TokenSet> {
    let mut form = vec![
        ("grant_type", "authorization_code".to_string()),
        ("code", code.to_string()),
        ("code_verifier", verifier.to_string()),
        ("redirect_uri", redirect_uri.to_string()),
        ("client_id", client.client_id.clone()),
    ];
    if let Some(s) = &client.client_secret {
        form.push(("client_secret", s.clone()));
    }
    token_request(client, http, &form).await
}

pub async fn refresh(client: &OAuthClient, http: &reqwest::Client, refresh_token: &str) -> Result<TokenSet> {
    let mut form = vec![
        ("grant_type", "refresh_token".to_string()),
        ("refresh_token", refresh_token.to_string()),
        ("client_id", client.client_id.clone()),
    ];
    if let Some(s) = &client.client_secret {
        form.push(("client_secret", s.clone()));
    }
    let mut set = token_request(client, http, &form).await?;
    // Providers usually omit the refresh token on refresh; keep the one we have.
    if set.refresh_token.is_none() {
        set.refresh_token = Some(refresh_token.to_string());
    }
    Ok(set)
}

async fn token_request(client: &OAuthClient, http: &reqwest::Client, form: &[(&str, String)]) -> Result<TokenSet> {
    let body: String = form
        .iter()
        .map(|(k, v)| format!("{}={}", k, url::form_urlencoded::byte_serialize(v.as_bytes()).collect::<String>()))
        .collect::<Vec<_>>()
        .join("&");
    let resp = http
        .post(&client.token_url)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("Accept", "application/json")
        .body(body)
        .send()
        .await?;
    let status = resp.status();
    let text = resp.text().await?;
    if !status.is_success() {
        let msg = serde_json::from_str::<TokenError>(&text)
            .map(|e| format!("{}: {}", e.error, e.error_description.unwrap_or_default()))
            .unwrap_or_else(|_| text.chars().take(300).collect());
        return Err(Error::OAuth(format!("token endpoint returned {status}: {msg}")));
    }
    let tr: TokenResponse = serde_json::from_str(&text).map_err(|e| Error::OAuth(format!("bad token response: {e}")))?;
    Ok(TokenSet {
        access_token: tr.access_token,
        refresh_token: tr.refresh_token,
        expires_at: now_unix() + tr.expires_in.unwrap_or(3600),
        scope: tr.scope,
        token_type: tr.token_type,
    })
}

#[cfg(test)]
pub(crate) mod fake_http {
    //! Minimal HTTP/1.1 responder for tests: one canned JSON response per accepted connection,
    //! recording each request's target and body.
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[derive(Default)]
    pub struct Seen {
        pub requests: Vec<(String, String)>,
    }

    pub async fn start(status: u16, json_body: &'static str, max: usize) -> (u16, Arc<Mutex<Seen>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen = Arc::new(Mutex::new(Seen::default()));
        let seen2 = seen.clone();
        tokio::spawn(async move {
            for _ in 0..max {
                let (mut s, _) = listener.accept().await.unwrap();
                let mut buf = Vec::new();
                let mut tmp = [0u8; 2048];
                let mut content_length = 0usize;
                loop {
                    let n = s.read(&mut tmp).await.unwrap();
                    if n == 0 { break; }
                    buf.extend_from_slice(&tmp[..n]);
                    if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&buf[..pos]).to_string();
                        for line in head.lines() {
                            if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                                content_length = v.trim().parse().unwrap_or(0);
                            }
                        }
                        if buf.len() >= pos + 4 + content_length { break; }
                    }
                }
                let text = String::from_utf8_lossy(&buf).to_string();
                let target = text.lines().next().unwrap_or("").split_whitespace().nth(1).unwrap_or("").to_string();
                let body = text.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
                seen2.lock().unwrap().requests.push((target, body));
                let resp = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    json_body.len(), json_body
                );
                s.write_all(resp.as_bytes()).await.unwrap();
                let _ = s.shutdown().await;
            }
        });
        (port, seen)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_matches_rfc7636_vector() {
        assert_eq!(
            pkce_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
        let (v, c) = pkce_pair().unwrap();
        assert_eq!(v.len(), 43);
        assert_eq!(c, pkce_challenge(&v));
    }

    #[test]
    fn callback_parsing() {
        assert_eq!(parse_callback("/callback?code=abc&state=xyz").unwrap(), ("abc".into(), "xyz".into()));
        assert!(parse_callback("/favicon.ico").is_err());
        assert!(parse_callback("/callback?error=access_denied&state=x").unwrap_err().contains("access_denied"));
        assert!(parse_callback("/callback?state=x").is_err());
    }

    #[test]
    fn authorize_url_has_pkce_and_extras() {
        let c = OAuthClient {
            client_id: "cid".into(), client_secret: None, auth_url: "https://auth.example/o".into(),
            token_url: "https://auth.example/t".into(), scopes: vec!["a".into(), "b c".into()],
            extra_auth_params: vec![("access_type".into(), "offline".into())],
        };
        let u = authorize_url(&c, "http://127.0.0.1:1/callback", "CH", "ST").unwrap();
        let q: std::collections::HashMap<_, _> = u.query_pairs().into_owned().collect();
        assert_eq!(q["code_challenge_method"], "S256");
        assert_eq!(q["code_challenge"], "CH");
        assert_eq!(q["scope"], "a b c");
        assert_eq!(q["access_type"], "offline");
        assert_eq!(q["redirect_uri"], "http://127.0.0.1:1/callback");
    }

    #[tokio::test]
    async fn full_loopback_flow_against_fake_provider() {
        let (tport, seen) = fake_http::start(200, r#"{"access_token":"AT","refresh_token":"RT","expires_in":3600,"scope":"a","token_type":"Bearer"}"#, 1).await;
        let client = OAuthClient {
            client_id: "cid".into(), client_secret: Some("sec".into()),
            auth_url: "https://auth.example/o".into(), token_url: format!("http://127.0.0.1:{tport}/token"),
            scopes: vec!["a".into()], extra_auth_params: vec![],
        };
        let http = crate::http_client();
        let tokens = authorize(&client, &http, Duration::from_secs(10), |url| {
            // Play the browser: follow the redirect straight back to the loopback listener.
            let u = url::Url::parse(url).unwrap();
            let q: std::collections::HashMap<_, _> = u.query_pairs().into_owned().collect();
            let redirect = q["redirect_uri"].clone();
            let state = q["state"].clone();
            tokio::spawn(async move {
                let c = crate::http_client();
                // A stray favicon request first must not break the flow.
                let base = redirect.trim_end_matches("/callback").to_string();
                let _ = c.get(format!("{base}/favicon.ico")).send().await;
                let r = c.get(format!("{redirect}?code=THECODE&state={state}")).send().await.unwrap();
                assert_eq!(r.status(), 200);
            });
        })
        .await
        .unwrap();
        assert_eq!(tokens.access_token, "AT");
        assert_eq!(tokens.refresh_token.as_deref(), Some("RT"));
        assert!(tokens.is_fresh(60));
        let reqs = &seen.lock().unwrap().requests;
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0].0, "/token");
        assert!(reqs[0].1.contains("grant_type=authorization_code"));
        assert!(reqs[0].1.contains("code=THECODE"));
        assert!(reqs[0].1.contains("client_secret=sec"));
        assert!(reqs[0].1.contains("code_verifier="));
    }

    #[tokio::test]
    async fn refresh_keeps_refresh_token_and_reports_errors() {
        let (p, _) = fake_http::start(200, r#"{"access_token":"AT2","expires_in":100}"#, 1).await;
        let client = OAuthClient { client_id: "c".into(), client_secret: None, auth_url: String::new(), token_url: format!("http://127.0.0.1:{p}/t"), scopes: vec![], extra_auth_params: vec![] };
        let http = crate::http_client();
        let t = refresh(&client, &http, "RT").await.unwrap();
        assert_eq!(t.access_token, "AT2");
        assert_eq!(t.refresh_token.as_deref(), Some("RT"));

        let (p, _) = fake_http::start(400, r#"{"error":"invalid_grant","error_description":"Token has been expired or revoked."}"#, 1).await;
        let client = OAuthClient { token_url: format!("http://127.0.0.1:{p}/t"), ..client };
        let err = refresh(&client, &http, "RT").await.unwrap_err().to_string();
        assert!(err.contains("invalid_grant"), "{err}");
    }
}
