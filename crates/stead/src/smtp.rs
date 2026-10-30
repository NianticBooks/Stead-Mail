//! SMTP submission: building RFC 5322 messages and delivering them via mail-send (STARTTLS on 587
//! or implicit TLS on 465). Error classification lives here so the outbox can decide whether a
//! failure is worth retrying.

use std::time::Duration;

use mail_send::mail_builder::MessageBuilder;
use mail_send::mail_builder::headers::address::Address;
use mail_send::smtp::message::Message;
use mail_send::SmtpClientBuilder;

use crate::account::ServerConfig;
use crate::error::{Error, Result};

/// Gmail's limit is 25 MB and most providers are similar; refuse early with a clear message.
pub const MAX_MESSAGE_BYTES: usize = 25 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Transport {
    #[default]
    Tls,
    /// Clear text. Only for loopback test servers.
    Plain,
}

/// 4xx replies and any network-level failure are temporary; 5xx replies, rejected credentials and
/// local misuse are permanent.
pub fn classify(e: mail_send::Error) -> Error {
    use mail_send::Error as E;
    match e {
        E::Io(_) | E::Timeout | E::UnparseableReply | E::Tls(_) | E::MissingStartTls => Error::SmtpTransient(e.to_string()),
        E::UnexpectedReply(ref r) if r.code >= 400 && r.code < 500 => Error::SmtpTransient(e.to_string()),
        other => Error::Smtp(other.to_string()),
    }
}

fn builder(server: &ServerConfig, password: &str) -> Result<SmtpClientBuilder<String>> {
    if server.host.is_empty() {
        return Err(Error::Invalid("smtp host is not configured".into()));
    }
    Ok(SmtpClientBuilder::new(server.host.clone(), server.port)
        .map_err(Error::Smtp)?
        .implicit_tls(!server.starttls)
        .credentials((server.username.clone(), password.to_string()))
        .timeout(Duration::from_secs(30)))
}

/// EHLO, STARTTLS (or implicit TLS), AUTH. Proves credentials without sending anything.
pub async fn verify_login(server: &ServerConfig, password: &str, transport: Transport) -> Result<()> {
    let b = builder(server, password)?;
    match transport {
        Transport::Tls => drop(b.connect().await.map_err(classify)?),
        Transport::Plain => drop(b.connect_plain().await.map_err(classify)?),
    }
    Ok(())
}

/// Hand one finished message to the server. `rcpts` is the envelope (To + Cc + Bcc).
pub async fn deliver(server: &ServerConfig, password: &str, from: &str, rcpts: &[String], raw: &[u8], transport: Transport) -> Result<()> {
    let msg = Message::new(from.to_string(), rcpts.to_vec(), raw.to_vec());
    let b = builder(server, password)?;
    match transport {
        Transport::Tls => b.connect().await.map_err(classify)?.send(msg).await.map_err(classify),
        Transport::Plain => b.connect_plain().await.map_err(classify)?.send(msg).await.map_err(classify),
    }
}

#[derive(Debug, Clone)]
pub struct AttachmentIn {
    pub filename: String,
    pub content_type: String,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, Default)]
pub struct Compose {
    pub from_name: Option<String>,
    pub from_email: String,
    pub to: Vec<String>,
    pub cc: Vec<String>,
    /// Envelope only: never written into the message headers.
    pub bcc: Vec<String>,
    pub subject: String,
    pub body: String,
    /// Message-ID of the message being answered, with or without angle brackets.
    pub in_reply_to: Option<String>,
    /// Full References chain of the original, oldest first.
    pub references: Vec<String>,
    pub attachments: Vec<AttachmentIn>,
}

#[derive(Debug, Clone)]
pub struct Built {
    pub message_id: String,
    pub raw: Vec<u8>,
    /// Envelope recipients: To, Cc and Bcc, deduplicated case-insensitively.
    pub rcpts: Vec<String>,
}

/// An address must be a single token with one `@` and no whitespace or control characters. This
/// is what stops header injection through the recipient list.
pub fn validate_address(a: &str) -> Result<String> {
    let a = a.trim();
    let ok = !a.is_empty()
        && a.matches('@').count() == 1
        && !a.starts_with('@')
        && !a.ends_with('@')
        && !a.chars().any(|c| c.is_whitespace() || c.is_control() || matches!(c, '<' | '>' | ',' | ';' | '"'));
    if ok { Ok(a.to_string()) } else { Err(Error::Invalid(format!("'{}' is not a valid email address", a.escape_debug()))) }
}

fn strip_brackets(id: &str) -> String {
    id.trim().trim_start_matches('<').trim_end_matches('>').to_string()
}

pub fn guess_content_type(filename: &str) -> &'static str {
    match filename.rsplit('.').next().map(|e| e.to_ascii_lowercase()).as_deref() {
        Some("pdf") => "application/pdf",
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("txt") | Some("log") => "text/plain",
        Some("md") => "text/markdown",
        Some("csv") => "text/csv",
        Some("html") | Some("htm") => "text/html",
        Some("json") => "application/json",
        Some("zip") => "application/zip",
        Some("ics") => "text/calendar",
        Some("stl") | Some("3mf") | Some("step") | Some("stp") => "model/stl",
        Some("doc") => "application/msword",
        Some("docx") => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        Some("xlsx") => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        _ => "application/octet-stream",
    }
}

pub fn build_message(c: &Compose) -> Result<Built> {
    let from_email = validate_address(&c.from_email)?;
    let to: Vec<String> = c.to.iter().map(|a| validate_address(a)).collect::<Result<_>>()?;
    let cc: Vec<String> = c.cc.iter().map(|a| validate_address(a)).collect::<Result<_>>()?;
    let bcc: Vec<String> = c.bcc.iter().map(|a| validate_address(a)).collect::<Result<_>>()?;
    if to.is_empty() && cc.is_empty() && bcc.is_empty() {
        return Err(Error::Invalid("no recipients".into()));
    }
    let mut seen = std::collections::HashSet::new();
    let rcpts: Vec<String> = to.iter().chain(&cc).chain(&bcc).filter(|a| seen.insert(a.to_ascii_lowercase())).cloned().collect();

    // A subject is one line; fold stray line breaks into spaces rather than letting them through.
    let subject: String = c.subject.chars().map(|ch| if ch == '\r' || ch == '\n' { ' ' } else { ch }).collect();
    let domain = from_email.rsplit('@').next().unwrap_or("localhost");
    let message_id = format!("{}@{}", uuid::Uuid::new_v4(), domain);

    let from = Address::new_address(c.from_name.clone().filter(|n| !n.trim().is_empty() && !n.contains(['\r', '\n'])), from_email.clone());
    let list = |v: &[String]| Address::new_list(v.iter().map(|a| Address::new_address(None::<String>, a.clone())).collect());
    let mut b = MessageBuilder::new().from(from).subject(subject).message_id(message_id.clone()).text_body(c.body.clone());
    if !to.is_empty() {
        b = b.to(list(&to));
    }
    if !cc.is_empty() {
        b = b.cc(list(&cc));
    }
    if let Some(irt) = c.in_reply_to.as_deref().map(strip_brackets).filter(|s| !s.is_empty()) {
        b = b.in_reply_to(irt);
    }
    let refs: Vec<String> = c.references.iter().map(|r| strip_brackets(r)).filter(|r| !r.is_empty()).collect();
    if !refs.is_empty() {
        b = b.references(refs);
    }
    let mut total = c.body.len();
    for a in &c.attachments {
        total += a.data.len();
        b = b.attachment(a.content_type.clone(), a.filename.clone(), a.data.clone());
    }
    if total > MAX_MESSAGE_BYTES {
        return Err(Error::Invalid(format!("message is {} MB; the limit is {} MB", total / (1024 * 1024), MAX_MESSAGE_BYTES / (1024 * 1024))));
    }
    let raw = b.write_to_vec()?;
    Ok(Built { message_id, raw, rcpts })
}

#[cfg(test)]
pub(crate) mod fake_smtp {
    //! Scripted plain-text SMTP server. Each accepted connection takes the next scripted behaviour
    //! (default: accept everything), so tests can make the first attempt fail and the second work.
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::TcpListener;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Behavior {
        Accept,
        /// 451 after DATA: temporary.
        TempFailData,
        /// 550 on RCPT TO: permanent.
        RejectRcpt,
        /// Close the connection right after the greeting: a network failure.
        Drop,
    }

    #[derive(Debug, Clone)]
    pub struct Received {
        pub from: String,
        pub rcpts: Vec<String>,
        pub data: String,
    }

    #[derive(Default)]
    pub struct State {
        pub script: VecDeque<Behavior>,
        pub received: Vec<Received>,
        pub connections: usize,
    }

    pub type Shared = Arc<Mutex<State>>;

    pub async fn start(state: Shared) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            loop {
                let Ok((sock, _)) = listener.accept().await else { return };
                let state = state.clone();
                tokio::spawn(async move {
                    let behavior = {
                        let mut st = state.lock().unwrap();
                        st.connections += 1;
                        st.script.pop_front().unwrap_or(Behavior::Accept)
                    };
                    let (r, mut w) = sock.into_split();
                    let mut lines = BufReader::new(r).lines();
                    let _ = w.write_all(b"220 fake ESMTP ready\r\n").await;
                    if behavior == Behavior::Drop {
                        return;
                    }
                    let (mut from, mut rcpts) = (String::new(), Vec::<String>::new());
                    while let Ok(Some(line)) = lines.next_line().await {
                        let up = line.to_ascii_uppercase();
                        let reply: String = if up.starts_with("EHLO") || up.starts_with("HELO") {
                            "250-fake\r\n250-8BITMIME\r\n250 AUTH PLAIN\r\n".into()
                        } else if up.starts_with("AUTH PLAIN") {
                            let payload = match line.split_whitespace().nth(2) {
                                Some(p) => p.to_string(),
                                None => {
                                    let _ = w.write_all(b"334 \r\n").await;
                                    lines.next_line().await.ok().flatten().unwrap_or_default()
                                }
                            };
                            use base64::Engine;
                            let decoded = base64::engine::general_purpose::STANDARD.decode(payload.trim()).unwrap_or_default();
                            if String::from_utf8_lossy(&decoded).ends_with("good-password") {
                                "235 2.7.0 Authentication successful\r\n".into()
                            } else {
                                "535 5.7.8 Authentication credentials invalid\r\n".into()
                            }
                        } else if up.starts_with("MAIL FROM") {
                            from = line[line.find('<').map(|i| i + 1).unwrap_or(10)..line.find('>').unwrap_or(line.len())].to_string();
                            rcpts.clear();
                            "250 ok\r\n".into()
                        } else if up.starts_with("RCPT TO") {
                            if behavior == Behavior::RejectRcpt {
                                "550 5.1.1 mailbox unavailable\r\n".into()
                            } else {
                                rcpts.push(line[line.find('<').map(|i| i + 1).unwrap_or(8)..line.find('>').unwrap_or(line.len())].to_string());
                                "250 ok\r\n".into()
                            }
                        } else if up.starts_with("DATA") {
                            let _ = w.write_all(b"354 go ahead\r\n").await;
                            let mut data = String::new();
                            while let Ok(Some(l)) = lines.next_line().await {
                                if l == "." {
                                    break;
                                }
                                data.push_str(&l);
                                data.push('\n');
                            }
                            if behavior == Behavior::TempFailData {
                                "451 4.3.0 try again later\r\n".into()
                            } else {
                                state.lock().unwrap().received.push(Received { from: from.clone(), rcpts: rcpts.clone(), data });
                                "250 2.0.0 queued\r\n".into()
                            }
                        } else if up.starts_with("QUIT") {
                            let _ = w.write_all(b"221 bye\r\n").await;
                            break;
                        } else {
                            "250 ok\r\n".into()
                        };
                        if w.write_all(reply.as_bytes()).await.is_err() {
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

    fn compose() -> Compose {
        Compose {
            from_name: Some("Fred Book".into()),
            from_email: "fred@example.com".into(),
            to: vec!["jill@example.com".into()],
            cc: vec!["kobe@example.com".into()],
            bcc: vec!["secret@example.com".into(), "JILL@example.com".into()],
            subject: "Hello\r\nBcc: injected@evil.test".into(),
            body: "Dinner at six?".into(),
            ..Default::default()
        }
    }

    #[test]
    fn builds_a_clean_message_and_keeps_bcc_out_of_the_headers() {
        let b = build_message(&compose()).unwrap();
        let text = String::from_utf8_lossy(&b.raw).to_string();
        assert!(text.contains("Dinner at six?"));
        assert!(!text.to_ascii_lowercase().contains("secret@example.com"), "Bcc must never appear in the message");
        assert!(!text.contains("\r\nBcc: injected"), "line breaks in the subject must not create headers");
        // Envelope has everyone once, case-insensitively.
        assert_eq!(b.rcpts, vec!["jill@example.com", "kobe@example.com", "secret@example.com"]);
        let parsed = mail_parser::MessageParser::default().parse(&b.raw).unwrap();
        assert_eq!(parsed.message_id(), Some(b.message_id.as_str()));
        assert_eq!(parsed.subject(), Some("Hello  Bcc: injected@evil.test"));
        assert_eq!(parsed.from().unwrap().first().unwrap().address(), Some("fred@example.com"));
        assert!(parsed.date().is_some());
    }

    #[test]
    fn reply_headers_and_attachments_round_trip() {
        let mut c = compose();
        c.bcc.clear();
        c.in_reply_to = Some("<orig@example.org>".into());
        c.references = vec!["<root@example.org>".into(), "<orig@example.org>".into()];
        c.attachments = vec![AttachmentIn { filename: "plan.pdf".into(), content_type: "application/pdf".into(), data: b"%PDF-1.4 fake".to_vec() }];
        let b = build_message(&c).unwrap();
        let p = mail_parser::MessageParser::default().parse(&b.raw).unwrap();
        assert_eq!(p.in_reply_to().and_then(|l| l.first()), Some("orig@example.org"));
        let refs: Vec<&str> = p.references().map(|l| l.iter().collect()).unwrap_or_default();
        assert_eq!(refs, vec!["root@example.org", "orig@example.org"]);
        let att: Vec<_> = p.attachments().collect();
        assert_eq!(att.len(), 1);
        assert_eq!(att[0].attachment_name(), Some("plan.pdf"));
        assert_eq!(att[0].decoded().as_ref(), b"%PDF-1.4 fake");
    }

    #[test]
    fn rejects_bad_addresses_and_oversize_messages() {
        for bad in ["", "no-at-sign", "a@b@c", "@x.com", "x@", "a b@c.com", "a@c.com\nBcc: z@z.com", "<a@c.com>", "a@c.com,b@c.com"] {
            assert!(validate_address(bad).is_err(), "{bad:?} should be rejected");
        }
        assert!(validate_address("  ok.name+tag@sub.example.com ").is_ok());
        let mut c = compose();
        c.to = vec!["bad address".into()];
        assert!(matches!(build_message(&c), Err(Error::Invalid(_))));
        let mut c = compose();
        c.attachments = vec![AttachmentIn { filename: "big.bin".into(), content_type: "application/octet-stream".into(), data: vec![0; MAX_MESSAGE_BYTES + 1] }];
        assert!(build_message(&c).unwrap_err().to_string().contains("limit"));
        let mut c = compose();
        c.to.clear(); c.cc.clear(); c.bcc.clear();
        assert!(build_message(&c).is_err());
    }

    #[test]
    fn content_types_by_extension() {
        assert_eq!(guess_content_type("Plan.PDF"), "application/pdf");
        assert_eq!(guess_content_type("noext"), "application/octet-stream");
        assert_eq!(guess_content_type("part.3mf"), "model/stl");
    }

    #[tokio::test]
    async fn delivers_to_a_plain_server_and_classifies_failures() {
        use fake_smtp::Behavior::*;
        let state: fake_smtp::Shared = Default::default();
        state.lock().unwrap().script.extend([Accept, TempFailData, RejectRcpt, Drop, Accept]);
        let port = fake_smtp::start(state.clone()).await;
        let server = ServerConfig { host: "127.0.0.1".into(), port, starttls: true, username: "fred".into() };
        let built = build_message(&compose()).unwrap();
        let send = |pw: &'static str| {
            let (server, built) = (server.clone(), built.clone());
            async move { deliver(&server, pw, "fred@example.com", &built.rcpts, &built.raw, Transport::Plain).await }
        };

        send("good-password").await.unwrap();
        {
            let st = state.lock().unwrap();
            assert_eq!(st.received.len(), 1);
            assert_eq!(st.received[0].from, "fred@example.com");
            assert_eq!(st.received[0].rcpts, built.rcpts);
            assert!(st.received[0].data.contains("Dinner at six?"));
        }
        assert!(matches!(send("good-password").await, Err(Error::SmtpTransient(_))), "451 is temporary");
        assert!(matches!(send("good-password").await, Err(Error::Smtp(_))), "550 is permanent");
        assert!(matches!(send("good-password").await, Err(Error::SmtpTransient(_))), "a dropped connection is temporary");
        assert!(matches!(send("wrong").await, Err(Error::Smtp(_))), "bad credentials are permanent");
    }
}
