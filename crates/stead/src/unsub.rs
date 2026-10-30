//! Unsubscribing from mailing lists: read `List-Unsubscribe` (RFC 2369) and the one-click form
//! (RFC 8058) and decide how a request may be made. The header comes from the sender, so every
//! address in it is untrusted: only https links to public hosts are ever requested.

use serde::{Deserialize, Serialize};
use std::net::IpAddr;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Targets {
    pub https: Option<String>,
    pub mailto: Option<String>,
    /// The sender accepts a bare POST to the https link (`List-Unsubscribe-Post: List-Unsubscribe=One-Click`).
    pub one_click: bool,
}

impl Targets {
    pub fn is_empty(&self) -> bool {
        self.https.is_none() && self.mailto.is_none()
    }
}

/// Header values from the raw header block of one message: `(List-Unsubscribe, List-Unsubscribe-Post)`.
/// Folded lines are joined. A missing header is an empty string.
pub fn extract_headers(raw: &[u8]) -> (String, String) {
    let text = String::from_utf8_lossy(raw);
    let (mut list, mut post) = (String::new(), String::new());
    let mut current: Option<&mut String> = None;
    for line in text.split("\r\n").flat_map(|l| l.split('\n')) {
        if line.is_empty() {
            break;
        }
        if line.starts_with(' ') || line.starts_with('\t') {
            if let Some(c) = current.as_deref_mut() {
                c.push(' ');
                c.push_str(line.trim());
            }
            continue;
        }
        current = None;
        if let Some((name, value)) = line.split_once(':') {
            let slot = match name.trim().to_ascii_lowercase().as_str() {
                "list-unsubscribe" => Some(&mut list),
                "list-unsubscribe-post" => Some(&mut post),
                _ => None,
            };
            if let Some(slot) = slot {
                if slot.is_empty() {
                    slot.push_str(value.trim());
                }
                current = Some(slot);
            }
        }
    }
    (list, post)
}

/// The usable targets in a `List-Unsubscribe` value such as `<mailto:u@x.com?subject=out>, <https://x.com/u/1>`.
pub fn parse_targets(list: &str, post: &str) -> Targets {
    let mut t = Targets::default();
    for part in list.split('<').skip(1) {
        let Some((inner, _)) = part.split_once('>') else { continue };
        let inner = inner.trim();
        let lower = inner.to_ascii_lowercase();
        if lower.starts_with("https://") && t.https.is_none() && public_https(inner).is_some() {
            t.https = Some(inner.to_string());
        } else if lower.starts_with("mailto:") && t.mailto.is_none() && mailto_parts(inner).is_some() {
            t.mailto = Some(inner.to_string());
        }
    }
    t.one_click = t.https.is_some() && post.to_ascii_lowercase().replace(' ', "").contains("list-unsubscribe=one-click");
    t
}

/// The parsed link when it is https to a host that is not on this machine or a private network.
/// Only literal addresses and local names can be judged here; the request itself also refuses redirects.
pub fn public_https(link: &str) -> Option<url::Url> {
    let u = url::Url::parse(link).ok()?;
    if u.scheme() != "https" || !u.username().is_empty() || u.password().is_some() {
        return None;
    }
    match u.host()? {
        url::Host::Domain(d) => {
            let d = d.trim_end_matches('.').to_ascii_lowercase();
            let local = d == "localhost" || !d.contains('.') || d.ends_with(".local") || d.ends_with(".localhost") || d.ends_with(".internal") || d.ends_with(".lan") || d.ends_with(".home");
            (!local).then_some(u)
        }
        url::Host::Ipv4(ip) => (!private_ip(IpAddr::V4(ip))).then_some(u),
        url::Host::Ipv6(ip) => (!private_ip(IpAddr::V6(ip))).then_some(u),
    }
}

fn private_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => v.is_private() || v.is_loopback() || v.is_link_local() || v.is_unspecified() || v.is_broadcast() || v.octets()[0] == 100 && (64..128).contains(&v.octets()[1]),
        IpAddr::V6(v) => v.is_loopback() || v.is_unspecified() || (v.segments()[0] & 0xfe00) == 0xfc00 || (v.segments()[0] & 0xffc0) == 0xfe80,
    }
}

/// `(address, subject, body)` of a `mailto:` link. `None` unless it names exactly one plain address.
pub fn mailto_parts(link: &str) -> Option<(String, String, String)> {
    let u = url::Url::parse(link).ok()?;
    if u.scheme() != "mailto" {
        return None;
    }
    let addr = percent_decode(u.path());
    let ok = !addr.is_empty() && !addr.contains([',', ' ', '<', '>', '\r', '\n']) && addr.matches('@').count() == 1 && !addr.starts_with('@') && !addr.ends_with('@');
    if !ok {
        return None;
    }
    let (mut subject, mut body) = (String::new(), String::new());
    for (k, v) in u.query_pairs() {
        match k.to_ascii_lowercase().as_str() {
            "subject" => subject = v.replace(['\r', '\n'], " "),
            "body" => body = v.into_owned(),
            _ => {}
        }
    }
    if subject.trim().is_empty() {
        subject = "unsubscribe".into();
    }
    Some((addr, subject, body))
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = |c: u8| (c as char).to_digit(16);
        if b[i] == b'%' && i + 2 < b.len() && hex(b[i + 1]).is_some() && hex(b[i + 2]).is_some() {
            out.push((hex(b[i + 1]).unwrap() * 16 + hex(b[i + 2]).unwrap()) as u8);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}


// ---------------------------------------------------------------------------------------------
// Api methods

use crate::account::Account;
use crate::api::{Api, SendRequest};
use crate::error::{Error, Result};
use crate::imap;
use crate::store::UnsubRow;
use std::collections::HashMap;

/// A sender worth unsubscribing from, and how it can be done.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Suggestion {
    /// Lower-cased address; the key for `unsubscribe` and `unsubscribe_dismiss`.
    pub sender: String,
    pub name: Option<String>,
    /// Messages from this sender in the cache, and how many of them are unread.
    pub total: u32,
    pub unread: u32,
    pub last_date: Option<String>,
    pub latest_uid: u32,
    pub latest_subject: Option<String>,
    /// The model's usual category for this sender's mail, when any was analysed.
    pub category: Option<String>,
    pub one_click: bool,
    pub has_mailto: bool,
    pub has_link: bool,
}

/// What a sender's newest message offers, for the message that is open.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MessageUnsub {
    pub sender: String,
    pub name: Option<String>,
    pub total: u32,
    pub one_click: bool,
    pub has_mailto: bool,
    pub has_link: bool,
    /// `unsubscribed`, `opened` or `dismissed` when something was already done about this sender.
    pub state: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Method {
    /// The sender confirmed a one-click request.
    OneClick,
    /// An unsubscribe email was queued to the sender's list address.
    Mailto,
    /// Only a web page is offered: the caller opens `url`.
    Web,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Outcome {
    pub sender: String,
    pub method: Method,
    /// For `web`, the page to open.
    pub url: Option<String>,
    /// Human summary of what was done.
    pub detail: String,
}

/// Messages read per scan. Newest first, so the senders that matter now come up first.
pub const SCAN_BATCH: usize = 400;

fn targets_of(r: &UnsubRow) -> Targets {
    parse_targets(&r.list_unsubscribe, &r.list_post)
}

/// One request, no redirects (a redirect could point back into the local network), short timeout.
/// `url` must already have passed [`public_https`] unless it is a test server.
pub async fn post_one_click(url: &url::Url) -> Result<()> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(concat!("stead/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| Error::Http(e.to_string()))?;
    let resp = http
        .post(url.clone())
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body("List-Unsubscribe=One-Click")
        .send()
        .await
        .map_err(|e| Error::Http(format!("{}: {e}", url.host_str().unwrap_or("the sender"))))?;
    if resp.status().is_success() {
        Ok(())
    } else {
        Err(Error::Http(format!("{} answered {}", url.host_str().unwrap_or("the sender"), resp.status())))
    }
}

impl Api {
    /// Read the list headers of cached messages that were not looked at yet. Returns how many.
    pub async fn unsubscribe_scan(&self, key: &str, mailbox: &str, limit: usize) -> Result<usize> {
        let account = self.store().find_account(key)?;
        // Most calls find nothing new: do not open a connection for those.
        {
            let store = self.store();
            let mb = store.find_mailbox(&account.id, mailbox)?;
            if store.unscanned_for_unsub(mb.id, 1)?.is_empty() {
                return Ok(0);
            }
        }
        let password = self.password_for(&account)?;
        let client = imap::connect_tls(&account.imap.host, account.imap.port).await?;
        let session = imap::login(client, &account.imap.username, &password).await?;
        self.unsubscribe_scan_with_session(session, &account, mailbox, limit).await
    }

    pub async fn unsubscribe_scan_with_session<T: imap::ImapStream>(
        &self,
        mut session: async_imap::Session<T>,
        account: &Account,
        mailbox: &str,
        limit: usize,
    ) -> Result<usize> {
        let (mb, todo) = {
            let store = self.store();
            let mb = store.find_mailbox(&account.id, mailbox)?;
            let todo = store.unscanned_for_unsub(mb.id, limit)?;
            (mb, todo)
        };
        if todo.is_empty() {
            imap::logout(session).await?;
            return Ok(0);
        }
        imap::select_state(&mut session, &mb.name).await?;
        let ids: HashMap<u32, i64> = todo.iter().map(|(id, uid)| (*uid, *id)).collect();
        let mut rows = Vec::new();
        // Chunks keep the command line short.
        for chunk in todo.chunks(100) {
            let set = chunk.iter().map(|(_, uid)| uid.to_string()).collect::<Vec<_>>().join(",");
            for (uid, list, post) in imap::fetch_unsub_headers(&mut session, &set).await? {
                if let Some(id) = ids.get(&uid) {
                    rows.push((*id, list, post));
                }
            }
        }
        // A message the server no longer has comes back empty-handed: record "none" so it is not retried.
        let seen: std::collections::HashSet<i64> = rows.iter().map(|(id, _, _)| *id).collect();
        for (id, _) in &todo {
            if !seen.contains(id) {
                rows.push((*id, String::new(), String::new()));
            }
        }
        imap::logout(session).await?;
        let n = rows.len();
        self.store().set_unsub_headers(&rows)?;
        Ok(n)
    }

    /// Senders with at least two cached messages that offer an unsubscribe, most mail first.
    /// Local only: run [`Api::unsubscribe_scan`] first to fill in the headers.
    pub fn unsubscribe_suggestions(&self, key: &str, mailbox: &str) -> Result<Vec<Suggestion>> {
        let store = self.store();
        let account = store.find_account(key)?;
        let mb = store.find_mailbox(&account.id, mailbox)?;
        let rows = store.unsub_rows(mb.id)?;
        let counts = store.sender_counts(mb.id)?;
        let done = store.unsub_sender_states(&account.id)?;
        let own = account.email.to_ascii_lowercase();

        let mut by_sender: Vec<(String, Vec<&UnsubRow>)> = Vec::new();
        let mut index: HashMap<String, usize> = HashMap::new();
        for r in &rows {
            let addr = r.from_addr.to_ascii_lowercase();
            if addr == own || done.contains_key(&addr) {
                continue;
            }
            let i = *index.entry(addr.clone()).or_insert_with(|| {
                by_sender.push((addr, Vec::new()));
                by_sender.len() - 1
            });
            by_sender[i].1.push(r);
        }
        let mut out = Vec::new();
        for (addr, msgs) in by_sender {
            let (total, unread) = counts.get(&addr).copied().unwrap_or((msgs.len() as u32, 0));
            if total < 2 {
                continue;
            }
            let latest = msgs[0]; // rows come newest first
            let t = targets_of(latest);
            if t.is_empty() {
                continue;
            }
            let mut cats: HashMap<&str, u32> = HashMap::new();
            for m in &msgs {
                if let Some(c) = m.category.as_deref() {
                    *cats.entry(c).or_default() += 1;
                }
            }
            let category = cats.into_iter().max_by_key(|(c, n)| (*n, std::cmp::Reverse(*c))).map(|(c, _)| c.to_string());
            out.push(Suggestion {
                sender: addr,
                name: msgs.iter().find_map(|m| m.from_name.clone().filter(|n| !n.trim().is_empty())),
                total,
                unread,
                last_date: latest.date.clone(),
                latest_uid: latest.uid,
                latest_subject: latest.subject.clone(),
                category,
                one_click: t.one_click,
                has_mailto: t.mailto.is_some(),
                has_link: t.https.is_some(),
            });
        }
        // Most mail first; of equal volume, the one you read least.
        out.sort_by(|a, b| b.total.cmp(&a.total).then(b.unread.cmp(&a.unread)).then(a.sender.cmp(&b.sender)));
        out.truncate(50);
        Ok(out)
    }

    /// What the open message's sender offers. Reads the headers of this one message if needed.
    pub async fn unsubscribe_info(&self, key: &str, mailbox: &str, uid: u32) -> Result<Option<MessageUnsub>> {
        let account = self.store().find_account(key)?;
        let mb = self.store().find_mailbox(&account.id, mailbox)?;
        let row = self.store().find_message(mb.id, uid)?;
        let scanned = self.store().has_unsub_row(row.id)?;
        if !scanned {
            let password = self.password_for(&account)?;
            let client = imap::connect_tls(&account.imap.host, account.imap.port).await?;
            let mut session = imap::login(client, &account.imap.username, &password).await?;
            imap::select_state(&mut session, &mb.name).await?;
            let got = imap::fetch_unsub_headers(&mut session, &uid.to_string()).await?;
            imap::logout(session).await?;
            let (list, post) = got.into_iter().find(|(u, _, _)| *u == uid).map(|(_, l, p)| (l, p)).unwrap_or_default();
            self.store().set_unsub_headers(&[(row.id, list, post)])?;
        }
        self.unsubscribe_info_cached(&account, mb.id, row.id)
    }

    fn unsubscribe_info_cached(&self, account: &Account, mailbox_id: i64, message_id: i64) -> Result<Option<MessageUnsub>> {
        let store = self.store();
        let rows = store.unsub_rows(mailbox_id)?;
        let Some(r) = rows.iter().find(|r| r.message_id == message_id) else { return Ok(None) };
        let t = targets_of(r);
        if t.is_empty() {
            return Ok(None);
        }
        let sender = r.from_addr.to_ascii_lowercase();
        let total = store.sender_counts(mailbox_id)?.get(&sender).map(|c| c.0).unwrap_or(1);
        let state = store.unsub_sender_states(&account.id)?.remove(&sender);
        Ok(Some(MessageUnsub { sender, name: r.from_name.clone(), total, one_click: t.one_click, has_mailto: t.mailto.is_some(), has_link: t.https.is_some(), state }))
    }

    /// Unsubscribe from a sender using its newest message's header: a one-click request when the
    /// sender supports it, else an email to its list address, else the web page is handed back.
    pub async fn unsubscribe(&self, key: &str, mailbox: &str, sender: &str) -> Result<Outcome> {
        let (account, targets) = {
            let store = self.store();
            let account = store.find_account(key)?;
            let mb = store.find_mailbox(&account.id, mailbox)?;
            let sender_l = sender.to_ascii_lowercase();
            let latest = store
                .unsub_rows(mb.id)?
                .into_iter()
                .find(|r| r.from_addr.eq_ignore_ascii_case(&sender_l) && !targets_of(r).is_empty())
                .ok_or_else(|| Error::Invalid(format!("{sender} offers no way to unsubscribe")))?;
            (account, targets_of(&latest))
        };
        let sender = sender.to_ascii_lowercase();
        let outcome = if let (true, Some(link)) = (targets.one_click, targets.https.as_deref()) {
            let url = public_https(link).ok_or_else(|| Error::Invalid("the unsubscribe link is not allowed".into()))?;
            post_one_click(&url).await?;
            Outcome { sender: sender.clone(), method: Method::OneClick, url: None, detail: format!("{} confirmed the request.", url.host_str().unwrap_or("The sender")) }
        } else if let Some((to, subject, body)) = targets.mailto.as_deref().and_then(mailto_parts) {
            let sent = self
                .send(SendRequest { account: account.id.clone(), to: vec![to.clone()], cc: vec![], bcc: vec![], subject, body, in_reply_to: None, references: vec![], attachments: vec![] })
                .await?;
            let detail = if sent.delivered { format!("Sent an unsubscribe email to {to}.") } else { format!("Queued an unsubscribe email to {to}; it will go out when the server is reachable.") };
            Outcome { sender: sender.clone(), method: Method::Mailto, url: None, detail }
        } else if let Some(link) = targets.https.as_deref() {
            Outcome { sender: sender.clone(), method: Method::Web, url: Some(link.to_string()), detail: "This sender needs you to confirm on its web page.".into() }
        } else {
            return Err(Error::Invalid(format!("{sender} offers no way to unsubscribe")));
        };
        let state = if outcome.method == Method::Web { "opened" } else { "unsubscribed" };
        let method = match outcome.method {
            Method::OneClick => "one_click",
            Method::Mailto => "mailto",
            Method::Web => "web",
        };
        self.store().set_unsub_sender(&account.id, &sender, state, Some(method))?;
        Ok(outcome)
    }

    /// Stop suggesting a sender.
    pub fn unsubscribe_dismiss(&self, key: &str, sender: &str) -> Result<()> {
        let store = self.store();
        let account = store.find_account(key)?;
        store.set_unsub_sender(&account.id, sender, "dismissed", None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headers_are_read_unfolded_and_case_insensitively() {
        let raw = b"From: a@b.c\r\nlist-unsubscribe: <mailto:u@x.com?subject=out>,\r\n <https://x.com/u/1>\r\nList-Unsubscribe-Post: List-Unsubscribe=One-Click\r\nSubject: hi\r\n\r\nbody: list-unsubscribe: <https://evil>";
        let (list, post) = extract_headers(raw);
        assert_eq!(list, "<mailto:u@x.com?subject=out>, <https://x.com/u/1>");
        assert_eq!(post, "List-Unsubscribe=One-Click");
        assert_eq!(extract_headers(b"From: a@b.c\r\n\r\n"), (String::new(), String::new()));
    }

    #[test]
    fn targets_keep_only_safe_links() {
        let t = parse_targets("<mailto:u@x.com?subject=out>, <https://x.com/u/1>", "List-Unsubscribe=One-Click");
        assert_eq!(t, Targets { https: Some("https://x.com/u/1".into()), mailto: Some("mailto:u@x.com?subject=out".into()), one_click: true });
        // One-click only counts with an https link and the right Post header.
        assert!(!parse_targets("<https://x.com/u/1>", "").one_click);
        assert!(!parse_targets("<mailto:u@x.com>", "List-Unsubscribe=One-Click").one_click);
        for bad in ["<http://x.com/u>", "<https://localhost/u>", "<https://127.0.0.1/u>", "<https://10.0.0.5/u>", "<https://192.168.1.1/u>", "<https://100.64.0.4/u>", "<https://[::1]/u>", "<https://printer/u>", "<https://nas.local/u>", "<https://user:pw@x.com/u>", "<javascript:alert(1)>", "<https://x.com/u"] {
            assert!(parse_targets(bad, "").is_empty(), "{bad}");
        }
        assert!(parse_targets("", "").is_empty());
    }

    #[test]
    fn mailto_needs_one_plain_address() {
        assert_eq!(mailto_parts("mailto:unsub@x.com?subject=Remove%20me&body=please").unwrap(), ("unsub@x.com".into(), "Remove me".into(), "please".into()));
        assert_eq!(mailto_parts("mailto:unsub@x.com").unwrap().1, "unsubscribe");
        for bad in ["mailto:", "mailto:a@x.com,b@y.com", "mailto:nobody", "mailto:a@b@c.com", "mailto:%0d%0aBcc:a@x.com", "https://x.com"] {
            assert!(mailto_parts(bad).is_none(), "{bad}");
        }
    }
}
