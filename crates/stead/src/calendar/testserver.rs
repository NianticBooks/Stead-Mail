//! A tiny programmable HTTP server for sync tests: the handler sees every request and decides
//! the answer, so tests can model a Google or CalDAV server with state.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[derive(Debug, Clone)]
pub struct Req {
    pub method: String,
    /// Path and query as sent.
    pub target: String,
    /// Lower-cased header names.
    pub headers: HashMap<String, String>,
    pub body: String,
}

impl Req {
    pub fn path(&self) -> &str {
        self.target.split('?').next().unwrap_or("")
    }

    pub fn query(&self, key: &str) -> Option<String> {
        let q = self.target.split_once('?')?.1;
        url::form_urlencoded::parse(q.as_bytes()).find(|(k, _)| k == key).map(|(_, v)| v.into_owned())
    }
}

pub struct Resp {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl Resp {
    pub fn json(status: u16, body: serde_json::Value) -> Resp {
        Resp { status, headers: vec![("Content-Type".into(), "application/json".into())], body: body.to_string() }
    }

    pub fn xml(status: u16, body: &str) -> Resp {
        Resp { status, headers: vec![("Content-Type".into(), "application/xml".into())], body: body.to_string() }
    }

    pub fn empty(status: u16) -> Resp {
        Resp { status, headers: vec![], body: String::new() }
    }

    pub fn with(mut self, k: &str, v: &str) -> Resp {
        self.headers.push((k.into(), v.into()));
        self
    }
}

pub type Seen = Arc<Mutex<Vec<Req>>>;

pub async fn serve(handler: impl Fn(&Req) -> Resp + Send + Sync + 'static) -> (u16, Seen) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen: Seen = Arc::new(Mutex::new(Vec::new()));
    let handler = Arc::new(handler);
    let seen2 = seen.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut s, _)) = listener.accept().await else { return };
            let (handler, seen) = (handler.clone(), seen2.clone());
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut tmp = [0u8; 8192];
                let (head_end, content_length) = loop {
                    let n = s.read(&mut tmp).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    buf.extend_from_slice(&tmp[..n]);
                    if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&buf[..pos]).to_string();
                        let cl = head.lines().filter_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap_or(0))).next().unwrap_or(0);
                        break (pos + 4, cl);
                    }
                };
                while buf.len() < head_end + content_length {
                    let n = s.read(&mut tmp).await.unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&tmp[..n]);
                }
                let head = String::from_utf8_lossy(&buf[..head_end - 4]).to_string();
                let mut lines = head.lines();
                let mut first = lines.next().unwrap_or("").split_whitespace();
                let (method, target) = (first.next().unwrap_or("").to_string(), first.next().unwrap_or("").to_string());
                let headers = lines.filter_map(|l| l.split_once(':')).map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string())).collect();
                let body = String::from_utf8_lossy(&buf[head_end..]).to_string();
                let req = Req { method, target, headers, body };
                seen.lock().unwrap().push(req.clone());
                let resp = handler(&req);
                let mut out = format!("HTTP/1.1 {} X\r\n", resp.status);
                for (k, v) in &resp.headers {
                    out.push_str(&format!("{k}: {v}\r\n"));
                }
                out.push_str(&format!("Content-Length: {}\r\nConnection: close\r\n\r\n{}", resp.body.len(), resp.body));
                let _ = s.write_all(out.as_bytes()).await;
                let _ = s.shutdown().await;
            });
        }
    });
    (port, seen)
}
