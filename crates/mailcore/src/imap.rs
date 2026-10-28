//! IMAP client layer on top of async-imap (tokio runtime, rustls TLS).
//!
//! Functions are generic over the stream so the same code runs against a TLS socket in
//! production and a plaintext in-process fake server in tests.

use std::sync::Arc;

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

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct MailboxState {
    pub exists: u32,
    pub uidvalidity: Option<u32>,
    pub uidnext: Option<u32>,
}

const HEADER_FIELDS: &str = "FROM TO CC SUBJECT DATE MESSAGE-ID IN-REPLY-TO";

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
        flags: f.flags().map(|fl| flag_to_string(&fl)).collect::<Vec<_>>().join(" "),
        ..Default::default()
    };
    if let Some(raw) = f.header() {
        parse_header_fields(raw, &mut h);
    }
    Some(h)
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
    if let Some(to) = msg.to() {
        let list: Vec<String> = to
            .mailboxes()
            .filter_map(|a| a.address().map(|addr| match a.name() {
                Some(n) => format!("{n} <{addr}>"),
                None => addr.to_string(),
            }))
            .collect();
        if !list.is_empty() {
            h.to_addrs = Some(list.join(", "));
        }
    }
    h.date = msg.date().map(|d| d.to_rfc3339());
    h.message_id = msg.message_id().map(|s| s.to_string());
    h.in_reply_to = msg.in_reply_to().and_then(|l| l.first()).map(|s| s.to_string());
}

#[cfg(test)]
pub(crate) mod fake_server {
    //! A tiny scripted IMAP server good enough to exercise LOGIN/LIST/SELECT/FETCH/LOGOUT.
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::TcpListener;

    pub const HEADERS_1: &str = "From: Jill Book <jill@example.com>\r\nTo: Fred Book <fred@example.com>\r\nSubject: =?UTF-8?Q?Caf=C3=A9_meeting?=\r\nDate: Mon, 26 Oct 2026 09:15:00 -0400\r\nMessage-ID: <m1@example.com>\r\n\r\n";
    pub const HEADERS_2: &str = "From: calendar@example.org\r\nTo: fred@example.com, kobe@example.com\r\nSubject: Invitation: Dentist\r\nDate: Tue, 27 Oct 2026 08:00:00 +0000\r\nMessage-ID: <m2@example.org>\r\nIn-Reply-To: <m1@example.com>\r\n\r\n";

    /// Start the server on an ephemeral port; returns the port. Serves exactly one connection.
    pub async fn start() -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (sock, _) = listener.accept().await.unwrap();
            let (r, mut w) = sock.into_split();
            let mut lines = BufReader::new(r).lines();
            w.write_all(b"* OK [CAPABILITY IMAP4rev1] fake ready\r\n").await.unwrap();
            while let Ok(Some(line)) = lines.next_line().await {
                let mut parts = line.splitn(3, ' ');
                let tag = parts.next().unwrap_or("*").to_string();
                let cmd = parts.next().unwrap_or("").to_ascii_uppercase();
                let args = parts.next().unwrap_or("").to_string();
                let resp = match cmd.as_str() {
                    "LOGIN" => {
                        if args.contains("\"good-password\"") || args.contains(" good-password") {
                            format!("{tag} OK [CAPABILITY IMAP4rev1] LOGIN completed\r\n")
                        } else {
                            format!("{tag} NO [AUTHENTICATIONFAILED] bad credentials\r\n")
                        }
                    }
                    "CAPABILITY" => format!("* CAPABILITY IMAP4rev1\r\n{tag} OK CAPABILITY completed\r\n"),
                    "LIST" => format!(
                        "* LIST (\\HasNoChildren) \"/\" \"INBOX\"\r\n* LIST (\\HasNoChildren \\Sent) \"/\" \"Sent Messages\"\r\n{tag} OK LIST completed\r\n"
                    ),
                    "SELECT" | "EXAMINE" => format!(
                        "* 2 EXISTS\r\n* 0 RECENT\r\n* OK [UIDVALIDITY 42] UIDs valid\r\n* OK [UIDNEXT 1003] Predicted next UID\r\n* FLAGS (\\Seen \\Answered \\Flagged)\r\n* OK [PERMANENTFLAGS (\\Seen \\Answered \\Flagged \\*)] Limited\r\n{tag} OK [READ-WRITE] SELECT completed\r\n"
                    ),
                    "FETCH" => {
                        let section = format!("BODY[HEADER.FIELDS ({})]", super::HEADER_FIELDS);
                        format!(
                            "* 1 FETCH (UID 1001 FLAGS (\\Seen) INTERNALDATE \"26-Oct-2026 13:15:00 +0000\" RFC822.SIZE 2048 {section} {{{}}}\r\n{})\r\n\
                             * 2 FETCH (UID 1002 FLAGS () INTERNALDATE \"27-Oct-2026 08:00:01 +0000\" RFC822.SIZE 4096 {section} {{{}}}\r\n{})\r\n\
                             {tag} OK FETCH completed\r\n",
                            HEADERS_1.len(), HEADERS_1, HEADERS_2.len(), HEADERS_2
                        )
                    }
                    "LOGOUT" => {
                        w.write_all(format!("* BYE fake signing off\r\n{tag} OK LOGOUT completed\r\n").as_bytes()).await.unwrap();
                        break;
                    }
                    _ => format!("{tag} OK {cmd} completed\r\n"),
                };
                w.write_all(resp.as_bytes()).await.unwrap();
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
        assert_eq!(h1.size, Some(2048));
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
    async fn bad_password_is_an_error() {
        let port = fake_server::start().await;
        let client = connect_plain("127.0.0.1", port).await.unwrap();
        let err = login(client, "fred@example.com", "wrong").await.err().unwrap();
        assert!(err.to_string().to_lowercase().contains("bad credentials"), "{err}");
    }
}
