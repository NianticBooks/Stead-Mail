//! IMAP client layer on top of async-imap (tokio runtime, rustls TLS).
//!
//! Functions are generic over the stream so the same code runs against a TLS socket in
//! production and a plaintext in-process fake server in tests.

use std::sync::Arc;

use std::collections::HashMap;
use std::time::Duration;

use async_imap::types::{Fetch, Name};
use async_imap::{Client, Session};
use futures_util::TryStreamExt;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;
use tokio_rustls::rustls::pki_types::ServerName;
use tokio_rustls::rustls::{ClientConfig, RootCertStore};

use crate::error::{Error, Result};
use crate::store::MessageHeader;

pub type TlsSession = Session<TlsStream<TcpStream>>;

/// Trait alias for anything async-imap can drive.
pub trait ImapStream: AsyncRead + AsyncWrite + Unpin + std::fmt::Debug + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + std::fmt::Debug + Send> ImapStream for T {}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MailboxInfo {
    pub name: String,
    pub delimiter: Option<String>,
    pub attributes: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct MailboxState {
    pub exists: u32,
    pub uidvalidity: Option<u32>,
    pub uidnext: Option<u32>,
}

const HEADER_FIELDS: &str = "FROM TO CC SUBJECT DATE MESSAGE-ID IN-REPLY-TO REFERENCES";

/// Open an implicit-TLS connection (port 993 style) and consume the server greeting.
pub async fn connect_tls(host: &str, port: u16) -> Result<Client<TlsStream<TcpStream>>> {
    let tcp = TcpStream::connect((host, port)).await?;
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let config = ClientConfig::builder().with_root_certificates(roots).with_no_client_auth();
    let connector = TlsConnector::from(Arc::new(config));
    let name = ServerName::try_from(host.to_string()).map_err(|e| Error::Tls(e.to_string()))?;
    let tls = connector.connect(name, tcp).await.map_err(|e| Error::Tls(e.to_string()))?;
    let mut client = Client::new(tls);
    let _greeting = client.read_response().await;
    Ok(client)
}

/// Plaintext connection. Only for tests and loopback development servers.
pub async fn connect_plain(host: &str, port: u16) -> Result<Client<TcpStream>> {
    let tcp = TcpStream::connect((host, port)).await?;
    let mut client = Client::new(tcp);
    let _greeting = client.read_response().await;
    Ok(client)
}

pub async fn login<T: ImapStream>(client: Client<T>, user: &str, password: &str) -> Result<Session<T>> {
    client.login(user, password).await.map_err(|(e, _client)| Error::Imap(e))
}

pub async fn list_mailboxes<T: ImapStream>(session: &mut Session<T>) -> Result<Vec<MailboxInfo>> {
    let names: Vec<Name> = session.list(Some(""), Some("*")).await?.try_collect().await?;
    Ok(names
        .iter()
        .map(|n| MailboxInfo {
            name: n.name().to_string(),
            delimiter: n.delimiter().map(|d| d.to_string()),
            attributes: n.attributes().iter().map(|a| format!("{a:?}")).collect(),
        })
        .collect())
}

/// SELECT a mailbox and fetch headers for the newest `limit` messages by sequence number.
pub async fn fetch_recent_headers<T: ImapStream>(
    session: &mut Session<T>,
    mailbox: &str,
    limit: u32,
) -> Result<(MailboxState, Vec<MessageHeader>)> {
    let mb = session.select(mailbox).await?;
    let state = MailboxState { exists: mb.exists, uidvalidity: mb.uid_validity, uidnext: mb.uid_next };
    if mb.exists == 0 || limit == 0 {
        return Ok((state, Vec::new()));
    }
    let start = mb.exists.saturating_sub(limit.saturating_sub(1)).max(1);
    let seq = format!("{start}:{}", mb.exists);
    let query = format!("(UID FLAGS INTERNALDATE RFC822.SIZE BODY.PEEK[HEADER.FIELDS ({HEADER_FIELDS})])");
    let fetches: Vec<Fetch> = session.fetch(seq, query).await?.try_collect().await?;
    let mut headers: Vec<MessageHeader> = fetches.iter().filter_map(header_from_fetch).collect();
    headers.sort_by_key(|h| h.uid);
    Ok((state, headers))
}

/// File a finished message in `mailbox` (the Sent copy, or a draft). `flags` like `\\Seen` or `\\Seen \\Draft`.
pub async fn append<T: ImapStream>(session: &mut Session<T>, mailbox: &str, flags: &str, raw: &[u8]) -> Result<()> {
    let flags = format!("({flags})");
    session.append(mailbox, Some(flags.as_str()), None, raw).await?;
    Ok(())
}

pub async fn logout<T: ImapStream>(mut session: Session<T>) -> Result<()> {
    session.logout().await?;
    Ok(())
}

fn header_from_fetch(f: &Fetch) -> Option<MessageHeader> {
    let uid = f.uid?;
    let mut h = MessageHeader {
        uid,
        size: f.size,
        internal_date: f.internal_date().map(|d| d.to_rfc3339()),
        flags: canonical_flags(f.flags().map(|fl| flag_to_string(&fl))),
        ..Default::default()
    };
    if let Some(raw) = f.header() {
        parse_header_fields(raw, &mut h);
    }
    Some(h)
}

/// Sorted, space-joined flags. Servers may reorder flags between responses; a canonical form
/// keeps "did the flags change" a plain string comparison.
pub fn canonical_flags(flags: impl Iterator<Item = String>) -> String {
    let mut v: Vec<String> = flags.collect();
    v.sort();
    v.dedup();
    v.join(" ")
}

/// Render an IMAP flag in wire form (`\\Seen`, `\\Flagged`, `$Label1`).
pub fn flag_to_string(flag: &async_imap::types::Flag<'_>) -> String {
    use async_imap::types::Flag::*;
    match flag {
        Seen => "\\Seen".into(),
        Answered => "\\Answered".into(),
        Flagged => "\\Flagged".into(),
        Deleted => "\\Deleted".into(),
        Draft => "\\Draft".into(),
        Recent => "\\Recent".into(),
        MayCreate => "\\*".into(),
        Custom(c) => c.to_string(),
    }
}

/// Fill the RFC 5322 header fields from raw header bytes using mail-parser (handles RFC 2047
/// encoded words, folded headers, and date normalisation).
pub fn parse_header_fields(raw: &[u8], h: &mut MessageHeader) {
    let Some(msg) = mail_parser::MessageParser::default().parse_headers(raw) else {
        return;
    };
    h.subject = msg.subject().map(|s| s.to_string());
    if let Some(addr) = msg.from().and_then(|a| a.first()) {
        h.from_name = addr.name().map(|s| s.to_string());
        h.from_addr = addr.address().map(|s| s.to_string());
    }
    let fmt = |a: Option<mail_parser::AddressList<'_>>| -> Option<String> {
        let list: Vec<String> = a?
            .mailboxes()
            .filter_map(|a| a.address().map(|addr| match a.name() {
                Some(n) => format!("{n} <{addr}>"),
                None => addr.to_string(),
            }))
            .collect();
        Some(list.join(", ")).filter(|s| !s.is_empty())
    };
    h.to_addrs = fmt(msg.to());
    // Empty (not NULL) records that the header was read and there is no Cc.
    h.cc_addrs = Some(fmt(msg.cc()).unwrap_or_default());
    h.date = msg.date().map(|d| d.to_rfc3339());
    h.message_id = msg.message_id().map(|s| s.to_string());
    h.in_reply_to = msg.in_reply_to().and_then(|l| l.first()).map(|s| s.to_string());
    h.references = msg.references().map(|l| l.iter().collect::<Vec<_>>().join(" ")).filter(|s| !s.is_empty());
}

fn header_query() -> String {
    format!("(UID FLAGS INTERNALDATE RFC822.SIZE BODY.PEEK[HEADER.FIELDS ({HEADER_FIELDS})])")
}

/// SELECT and report the server-side state without fetching anything.
pub async fn select_state<T: ImapStream>(session: &mut Session<T>, mailbox: &str) -> Result<MailboxState> {
    let mb = session.select(mailbox).await?;
    Ok(MailboxState { exists: mb.exists, uidvalidity: mb.uid_validity, uidnext: mb.uid_next })
}

/// Headers of the newest `limit` messages by sequence number (used for the first sync only).
pub async fn fetch_newest_headers<T: ImapStream>(session: &mut Session<T>, exists: u32, limit: u32) -> Result<Vec<MessageHeader>> {
    if exists == 0 || limit == 0 {
        return Ok(Vec::new());
    }
    let start = exists.saturating_sub(limit.saturating_sub(1)).max(1);
    let fetches: Vec<Fetch> = session.fetch(format!("{start}:{exists}"), header_query()).await?.try_collect().await?;
    let mut headers: Vec<MessageHeader> = fetches.iter().filter_map(header_from_fetch).collect();
    headers.sort_by_key(|h| h.uid);
    Ok(headers)
}

/// Headers for a UID set such as `1001:*` or `5,9,12`.
pub async fn fetch_headers_by_uidset<T: ImapStream>(session: &mut Session<T>, uid_set: &str) -> Result<Vec<MessageHeader>> {
    let fetches: Vec<Fetch> = session.uid_fetch(uid_set, header_query()).await?.try_collect().await?;
    let mut headers: Vec<MessageHeader> = fetches.iter().filter_map(header_from_fetch).collect();
    headers.sort_by_key(|h| h.uid);
    Ok(headers)
}

/// Current flags of every message with a UID in `min..=max`. Expunged UIDs are simply absent.
pub async fn fetch_flags_range<T: ImapStream>(session: &mut Session<T>, min: u32, max: u32) -> Result<HashMap<u32, String>> {
    let fetches: Vec<Fetch> = session.uid_fetch(format!("{min}:{max}"), "(UID FLAGS)").await?.try_collect().await?;
    Ok(fetches
        .iter()
        .filter_map(|f| Some((f.uid?, canonical_flags(f.flags().map(|fl| flag_to_string(&fl))))))
        .collect())
}

/// `UID SEARCH <criteria>`, ascending.
pub async fn search_uids<T: ImapStream>(session: &mut Session<T>, criteria: &str) -> Result<Vec<u32>> {
    let mut v: Vec<u32> = session.uid_search(criteria).await?.into_iter().collect();
    v.sort_unstable();
    Ok(v)
}

/// The complete RFC 822 message. Uses PEEK so reading does not set \Seen.
pub async fn fetch_raw<T: ImapStream>(session: &mut Session<T>, uid: u32) -> Result<Option<Vec<u8>>> {
    let fetches: Vec<Fetch> = session.uid_fetch(uid.to_string(), "(UID BODY.PEEK[])").await?.try_collect().await?;
    Ok(fetches.iter().find(|f| f.uid == Some(uid)).and_then(|f| f.body().map(|b| b.to_vec())))
}

/// Server-side unread count via STATUS. `None` when the server refuses it (some do for the
/// selected mailbox); the caller then simply has no count rather than a wrong one.
pub async fn unseen_count<T: ImapStream>(session: &mut Session<T>, mailbox: &str) -> Option<u32> {
    session.status(mailbox, "(UNSEEN)").await.ok().and_then(|m| m.unseen)
}

/// Add or remove one flag (`\Seen`, `\Flagged`) on a message. SILENT: no echo is wanted.
pub async fn store_flag<T: ImapStream>(session: &mut Session<T>, uid: u32, flag: &str, add: bool) -> Result<()> {
    let sign = if add { '+' } else { '-' };
    let stream = session.uid_store(uid.to_string(), format!("{sign}FLAGS.SILENT ({flag})")).await?;
    let _: Vec<Fetch> = stream.try_collect().await?;
    Ok(())
}

/// Move a message to another mailbox of the same account. Uses `UID MOVE` (RFC 6851) and, for
/// servers without it, copy + `\\Deleted` + `UID EXPUNGE`. Returns true when the fallback was used.
/// The copy comes first, so a failure part way never loses the message.
pub async fn move_uid<T: ImapStream>(session: &mut Session<T>, uid: u32, dest: &str) -> Result<bool> {
    let uid_set = uid.to_string();
    if session.uid_mv(&uid_set, dest).await.is_ok() {
        return Ok(false);
    }
    session.uid_copy(&uid_set, dest).await?;
    store_flag(session, uid, "\\Deleted", true).await?;
    // Only this uid is removed (needs UIDPLUS). A plain EXPUNGE would also drop other messages
    // somebody else had marked \Deleted, so without UIDPLUS the flag alone has to do.
    match session.uid_expunge(&uid_set).await {
        Ok(stream) => {
            let _: std::result::Result<Vec<_>, _> = stream.try_collect::<Vec<_>>().await;
        }
        Err(e) => tracing::warn!("UID EXPUNGE refused; message stays flagged \\Deleted: {e}"),
    }
    Ok(true)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdleOutcome {
    /// The server pushed something (new mail, flag change, expunge). Re-sync.
    Changed,
    /// Nothing happened before the timeout. Re-issue IDLE (servers drop idle connections ~30 min).
    Timeout,
}

/// Enter IDLE on the currently selected mailbox and wait for a push or the timeout.
pub async fn idle_wait<T: ImapStream>(session: Session<T>, timeout: Duration) -> Result<(Session<T>, IdleOutcome)> {
    let mut handle = session.idle();
    handle.init().await?;
    let outcome = {
        let (wait, _interrupt) = handle.wait_with_timeout(timeout);
        match wait.await? {
            async_imap::extensions::idle::IdleResponse::NewData(_) => IdleOutcome::Changed,
            _ => IdleOutcome::Timeout,
        }
    };
    let session = handle.done().await?;
    Ok((session, outcome))
}

#[cfg(test)]
pub(crate) mod fake_server {
    //! A small stateful IMAP server: LOGIN, LIST, SELECT, FETCH, UID FETCH, UID SEARCH, IDLE and
    //! LOGOUT over one mailbox whose contents tests can mutate between connections.
    use std::sync::{Arc, Mutex};

    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::TcpListener;

    pub const HEADERS_1: &str = "From: Jill Book <jill@example.com>\r\nTo: Fred Book <fred@example.com>\r\nSubject: =?UTF-8?Q?Caf=C3=A9_meeting?=\r\nDate: Mon, 26 Oct 2026 09:15:00 -0400\r\nMessage-ID: <m1@example.com>\r\n\r\n";
    pub const HEADERS_2: &str = "From: calendar@example.org\r\nTo: fred@example.com, kobe@example.com\r\nSubject: Invitation: Dentist\r\nDate: Tue, 27 Oct 2026 08:00:00 +0000\r\nMessage-ID: <m2@example.org>\r\nIn-Reply-To: <m1@example.com>\r\nReferences: <m1@example.com>\r\n\r\n";

    #[derive(Clone)]
    pub struct FakeMsg {
        pub uid: u32,
        pub flags: String,
        pub raw: String,
        pub internal_date: String,
    }

    pub struct FakeState {
        pub uidvalidity: u32,
        pub uidnext: u32,
        pub msgs: Vec<FakeMsg>,
        /// When set, IDLE pushes `* N EXISTS` shortly after it starts.
        pub push_on_idle: bool,
        /// Extra raw `* LIST ...` lines (without CRLF) to advertise besides INBOX and Sent Messages.
        pub extra_list: Vec<String>,
        /// Every command line received, for assertions about what the client asked for.
        pub log: Vec<String>,
        /// Messages filed with APPEND: (mailbox, flags, raw).
        pub appended: Vec<(String, String, String)>,
        /// Refuse `UID MOVE` (a server without RFC 6851), so the copy fallback is exercised.
        pub no_move: bool,
        /// Messages moved or copied to another mailbox: (mailbox, raw).
        pub filed: Vec<(String, String)>,
    }

    impl FakeState {
        pub fn standard() -> FakeState {
            FakeState {
                uidvalidity: 42,
                uidnext: 1003,
                msgs: vec![
                    FakeMsg { uid: 1001, flags: "\\Seen".into(), raw: format!("{HEADERS_1}Hello from Jill about the meeting.\r\n"), internal_date: "26-Oct-2026 13:15:00 +0000".into() },
                    FakeMsg { uid: 1002, flags: String::new(), raw: format!("{HEADERS_2}Your dentist appointment is Thursday.\r\n"), internal_date: "27-Oct-2026 08:00:01 +0000".into() },
                ],
                push_on_idle: false,
                extra_list: Vec::new(),
                log: Vec::new(),
                appended: Vec::new(),
                no_move: false,
                filed: Vec::new(),
            }
        }

        pub fn add(&mut self, headers: &str, body: &str, flags: &str) -> u32 {
            let uid = self.uidnext;
            self.uidnext += 1;
            self.msgs.push(FakeMsg { uid, flags: flags.into(), raw: format!("{headers}{body}"), internal_date: "28-Oct-2026 10:00:00 +0000".into() });
            uid
        }
    }

    pub type Shared = Arc<Mutex<FakeState>>;

    /// Parse `1001:*`, `5,9`, `7` against the current max uid. Like real servers, `n:*` always
    /// includes the highest message even when n is above it.
    fn parse_set(set: &str, max: u32) -> Vec<(u32, u32)> {
        set.split(',')
            .filter_map(|part| {
                let num = |x: &str| if x == "*" { Some(max) } else { x.parse::<u32>().ok() };
                match part.split_once(':') {
                    Some((a, b)) => {
                        let (a, b) = (num(a)?, num(b)?);
                        Some((a.min(b), a.max(b)))
                    }
                    None => num(part).map(|n| (n, n)),
                }
            })
            .collect()
    }

    fn in_set(ranges: &[(u32, u32)], uid: u32) -> bool {
        ranges.iter().any(|(a, b)| uid >= *a && uid <= *b)
    }

    fn fetch_line(seq: usize, m: &FakeMsg, query: &str) -> String {
        let q = query.to_ascii_uppercase();
        let mut items = format!("UID {}", m.uid);
        if q.contains("FLAGS") {
            items += &format!(" FLAGS ({})", m.flags);
        }
        if q.contains("INTERNALDATE") {
            items += &format!(" INTERNALDATE \"{}\"", m.internal_date);
        }
        if q.contains("RFC822.SIZE") {
            items += &format!(" RFC822.SIZE {}", m.raw.len());
        }
        if q.contains("HEADER.FIELDS") {
            let end = m.raw.find("\r\n\r\n").map(|i| i + 4).unwrap_or(m.raw.len());
            let head = &m.raw[..end];
            items += &format!(" BODY[HEADER.FIELDS ({})] {{{}}}\r\n{}", super::HEADER_FIELDS, head.len(), head);
        } else if q.contains("BODY.PEEK[]") || q.contains("BODY[]") {
            items += &format!(" BODY[] {{{}}}\r\n{}", m.raw.len(), m.raw);
        }
        format!("* {seq} FETCH ({items})\r\n")
    }

    pub async fn start() -> u16 {
        start_with(Arc::new(Mutex::new(FakeState::standard()))).await
    }

    /// Serve any number of connections against shared state; returns the port.
    pub async fn start_with(state: Shared) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            loop {
                let Ok((sock, _)) = listener.accept().await else { return };
                let state = state.clone();
                tokio::spawn(async move {
                    let (r, mut w) = sock.into_split();
                    let mut lines = BufReader::new(r).lines();
                    let _ = w.write_all(b"* OK [CAPABILITY IMAP4rev1 IDLE UIDPLUS] fake ready\r\n").await;
                    while let Ok(Some(line)) = lines.next_line().await {
                        state.lock().unwrap().log.push(line.clone());
                        let mut parts = line.splitn(3, ' ');
                        let tag = parts.next().unwrap_or("*").to_string();
                        let mut cmd = parts.next().unwrap_or("").to_ascii_uppercase();
                        let mut args = parts.next().unwrap_or("").to_string();
                        let is_uid = cmd == "UID";
                        if is_uid {
                            let (sub, rest) = args.split_once(' ').unwrap_or((args.as_str(), ""));
                            cmd = format!("UID {}", sub.to_ascii_uppercase());
                            args = rest.to_string();
                        }
                        let resp = match cmd.as_str() {
                            "LOGIN" => {
                                if args.contains("good-password") {
                                    format!("{tag} OK [CAPABILITY IMAP4rev1] LOGIN completed\r\n")
                                } else {
                                    format!("{tag} NO [AUTHENTICATIONFAILED] bad credentials\r\n")
                                }
                            }
                            "CAPABILITY" => format!("* CAPABILITY IMAP4rev1 IDLE\r\n{tag} OK CAPABILITY completed\r\n"),
                            "LIST" => {
                                let extra: String = state.lock().unwrap().extra_list.iter().map(|l| format!("{l}\r\n")).collect();
                                format!(
                                    "* LIST (\\HasNoChildren) \"/\" \"INBOX\"\r\n* LIST (\\HasNoChildren \\Sent) \"/\" \"Sent Messages\"\r\n{extra}{tag} OK LIST completed\r\n"
                                )
                            }
                            "SELECT" | "EXAMINE" => {
                                let st = state.lock().unwrap();
                                format!(
                                    "* {} EXISTS\r\n* 0 RECENT\r\n* OK [UIDVALIDITY {}] UIDs valid\r\n* OK [UIDNEXT {}] Predicted next UID\r\n* FLAGS (\\Seen \\Answered \\Flagged)\r\n* OK [PERMANENTFLAGS (\\Seen \\Answered \\Flagged \\*)] Limited\r\n{tag} OK [READ-WRITE] SELECT completed\r\n",
                                    st.msgs.len(), st.uidvalidity, st.uidnext
                                )
                            }
                            "FETCH" | "UID FETCH" => {
                                let st = state.lock().unwrap();
                                let (set, query) = args.split_once(' ').unwrap_or((args.as_str(), ""));
                                let max = st.msgs.iter().map(|m| m.uid).max().unwrap_or(0);
                                let ranges = parse_set(set, if is_uid { max } else { st.msgs.len() as u32 });
                                let mut out = String::new();
                                for (i, m) in st.msgs.iter().enumerate() {
                                    let key = if is_uid { m.uid } else { i as u32 + 1 };
                                    if in_set(&ranges, key) {
                                        out += &fetch_line(i + 1, m, query);
                                    }
                                }
                                format!("{out}{tag} OK FETCH completed\r\n")
                            }
                            "UID STORE" => {
                                // args: "<uid> +FLAGS.SILENT (\\Seen)"
                                let mut it = args.splitn(3, ' ');
                                let uid: u32 = it.next().and_then(|x| x.parse().ok()).unwrap_or(0);
                                let op = it.next().unwrap_or("").to_ascii_uppercase();
                                let flags: Vec<String> = it.next().unwrap_or("").trim_matches(|c| c == '(' || c == ')').split_whitespace().map(String::from).collect();
                                let mut st = state.lock().unwrap();
                                if let Some(m) = st.msgs.iter_mut().find(|m| m.uid == uid) {
                                    let mut cur: Vec<String> = m.flags.split_whitespace().map(String::from).collect();
                                    for f in flags {
                                        if op.starts_with('+') {
                                            if !cur.contains(&f) { cur.push(f); }
                                        } else {
                                            cur.retain(|c| *c != f);
                                        }
                                    }
                                    m.flags = cur.join(" ");
                                }
                                format!("{tag} OK STORE completed\r\n")
                            }
                            "UID MOVE" | "UID COPY" => {
                                // args: `<uid> "Trash"`
                                let (set, dest) = args.split_once(' ').unwrap_or((args.as_str(), ""));
                                let dest = dest.trim_matches('"').to_string();
                                let mut st = state.lock().unwrap();
                                if cmd == "UID MOVE" && st.no_move {
                                    format!("{tag} BAD command not supported\r\n")
                                } else {
                                    let max = st.msgs.iter().map(|m| m.uid).max().unwrap_or(0);
                                    let ranges = parse_set(set, max);
                                    let hit: Vec<FakeMsg> = st.msgs.iter().filter(|m| in_set(&ranges, m.uid)).cloned().collect();
                                    if hit.is_empty() {
                                        format!("{tag} NO no such message\r\n")
                                    } else {
                                        for m in &hit {
                                            st.filed.push((dest.clone(), m.raw.clone()));
                                        }
                                        if cmd == "UID MOVE" {
                                            st.msgs.retain(|m| !in_set(&ranges, m.uid));
                                        }
                                        format!("{tag} OK {cmd} completed\r\n")
                                    }
                                }
                            }
                            "UID EXPUNGE" => {
                                let mut st = state.lock().unwrap();
                                let max = st.msgs.iter().map(|m| m.uid).max().unwrap_or(0);
                                let ranges = parse_set(args.trim(), max);
                                st.msgs.retain(|m| !(in_set(&ranges, m.uid) && m.flags.contains("\\Deleted")));
                                format!("{tag} OK UID EXPUNGE completed\r\n")
                            }
                            "APPEND" => {
                                // args: `"Sent Messages" (\\Seen) {123}`; send the continuation, read the literal.
                                let size: usize = args.rsplit('{').next().and_then(|x| x.trim_end_matches('}').parse().ok()).unwrap_or(0);
                                let mailbox = args.split('"').nth(1).unwrap_or("").to_string();
                                let flags = args.split('(').nth(1).and_then(|x| x.split(')').next()).unwrap_or("").to_string();
                                let _ = w.write_all(b"+ go ahead\r\n").await;
                                let mut buf = vec![0u8; size];
                                let _ = tokio::io::AsyncReadExt::read_exact(lines.get_mut(), &mut buf).await;
                                let _ = lines.next_line().await; // the CRLF that ends the command
                                state.lock().unwrap().appended.push((mailbox, flags, String::from_utf8_lossy(&buf).into_owned()));
                                format!("{tag} OK [APPENDUID 42 2000] APPEND completed\r\n")
                            }
                            "STATUS" => {
                                let st = state.lock().unwrap();
                                let unseen = st.msgs.iter().filter(|m| !m.flags.contains("Seen")).count();
                                let name = args.split_whitespace().next().unwrap_or("INBOX").trim_matches('"').to_string();
                                format!("* STATUS \"{name}\" (UNSEEN {unseen})\r\n{tag} OK STATUS completed\r\n")
                            }
                            "UID SEARCH" => {
                                let st = state.lock().unwrap();
                                let max = st.msgs.iter().map(|m| m.uid).max().unwrap_or(0);
                                let ranges = match args.trim().strip_prefix("UID ") {
                                    Some(set) => parse_set(set, max),
                                    None => vec![(0, u32::MAX)],
                                };
                                let uids: Vec<String> = st.msgs.iter().filter(|m| in_set(&ranges, m.uid)).map(|m| m.uid.to_string()).collect();
                                format!("* SEARCH {}\r\n{tag} OK SEARCH completed\r\n", uids.join(" "))
                            }
                            "IDLE" => {
                                let (push, n) = {
                                    let st = state.lock().unwrap();
                                    (st.push_on_idle, st.msgs.len() + 1)
                                };
                                let _ = w.write_all(b"+ idling\r\n").await;
                                if push {
                                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                                    let _ = w.write_all(format!("* {n} EXISTS\r\n").as_bytes()).await;
                                }
                                // Wait for DONE, then terminate the command.
                                while let Ok(Some(l)) = lines.next_line().await {
                                    if l.trim().eq_ignore_ascii_case("DONE") {
                                        break;
                                    }
                                }
                                format!("{tag} OK IDLE terminated\r\n")
                            }
                            "LOGOUT" => {
                                let _ = w.write_all(format!("* BYE fake signing off\r\n{tag} OK LOGOUT completed\r\n").as_bytes()).await;
                                break;
                            }
                            _ => format!("{tag} OK {cmd} completed\r\n"),
                        };
                        if w.write_all(resp.as_bytes()).await.is_err() {
                            break;
                        }
                    }
                });
            }
        });
        port
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn login_list_fetch_against_fake_server() {
        let port = fake_server::start().await;
        let client = connect_plain("127.0.0.1", port).await.unwrap();
        let mut session = login(client, "fred@example.com", "good-password").await.unwrap();

        let boxes = list_mailboxes(&mut session).await.unwrap();
        assert_eq!(boxes.len(), 2);
        assert_eq!(boxes[0].name, "INBOX");
        assert_eq!(boxes[0].delimiter.as_deref(), Some("/"));
        assert_eq!(boxes[1].name, "Sent Messages");

        let (state, headers) = fetch_recent_headers(&mut session, "INBOX", 50).await.unwrap();
        assert_eq!(state, MailboxState { exists: 2, uidvalidity: Some(42), uidnext: Some(1003) });
        assert_eq!(headers.len(), 2);

        let h1 = &headers[0];
        assert_eq!(h1.uid, 1001);
        assert_eq!(h1.subject.as_deref(), Some("Café meeting"));
        assert_eq!(h1.from_name.as_deref(), Some("Jill Book"));
        assert_eq!(h1.from_addr.as_deref(), Some("jill@example.com"));
        assert_eq!(h1.message_id.as_deref(), Some("m1@example.com"));
        assert_eq!(h1.flags, "\\Seen");
        assert_eq!(h1.size, Some(format!("{}Hello from Jill about the meeting.\r\n", fake_server::HEADERS_1).len() as u32));
        assert!(h1.date.as_deref().unwrap().starts_with("2026-10-26T09:15:00"));
        assert!(h1.internal_date.as_deref().unwrap().starts_with("2026-10-26T13:15:00"));

        let h2 = &headers[1];
        assert_eq!(h2.uid, 1002);
        assert_eq!(h2.in_reply_to.as_deref(), Some("m1@example.com"));
        assert_eq!(h2.to_addrs.as_deref(), Some("fred@example.com, kobe@example.com"));
        assert_eq!(h2.flags, "");

        logout(session).await.unwrap();
    }

    #[tokio::test]
    async fn idle_reports_a_push_and_a_timeout() {
        use std::sync::{Arc, Mutex};
        let state = Arc::new(Mutex::new(fake_server::FakeState::standard()));
        state.lock().unwrap().push_on_idle = true;
        let port = fake_server::start_with(state.clone()).await;
        let client = connect_plain("127.0.0.1", port).await.unwrap();
        let mut session = login(client, "u", "good-password").await.unwrap();
        select_state(&mut session, "INBOX").await.unwrap();
        let (session, outcome) = idle_wait(session, Duration::from_secs(5)).await.unwrap();
        assert_eq!(outcome, IdleOutcome::Changed);

        // Without a push the wait ends at the timeout, and the session is still usable after DONE.
        state.lock().unwrap().push_on_idle = false;
        let (mut session, outcome) = idle_wait(session, Duration::from_millis(200)).await.unwrap();
        assert_eq!(outcome, IdleOutcome::Timeout);
        assert_eq!(list_mailboxes(&mut session).await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn uid_primitives_follow_real_server_range_rules() {
        use std::sync::{Arc, Mutex};
        let state = Arc::new(Mutex::new(fake_server::FakeState::standard()));
        let port = fake_server::start_with(state).await;
        let client = connect_plain("127.0.0.1", port).await.unwrap();
        let mut session = login(client, "u", "good-password").await.unwrap();
        select_state(&mut session, "INBOX").await.unwrap();
        // `n:*` above the highest uid still returns the highest message (RFC 3501 §6.4.8).
        let h = fetch_headers_by_uidset(&mut session, "1003:*").await.unwrap();
        assert_eq!(h.iter().map(|m| m.uid).collect::<Vec<_>>(), vec![1002]);
        let flags = fetch_flags_range(&mut session, 1001, 1002).await.unwrap();
        assert_eq!(flags[&1001], "\\Seen");
        assert_eq!(flags[&1002], "");
        assert_eq!(search_uids(&mut session, "UID 1:1001").await.unwrap(), vec![1001]);
        let raw = fetch_raw(&mut session, 1002).await.unwrap().unwrap();
        assert!(String::from_utf8_lossy(&raw).contains("dentist appointment"));
        assert!(fetch_raw(&mut session, 9999).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn bad_password_is_an_error() {
        let port = fake_server::start().await;
        let client = connect_plain("127.0.0.1", port).await.unwrap();
        let err = login(client, "fred@example.com", "wrong").await.err().unwrap();
        assert!(err.to_string().to_lowercase().contains("bad credentials"), "{err}");
    }
}
