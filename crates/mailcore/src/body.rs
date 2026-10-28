//! MIME body parsing: plain text, HTML, and attachment metadata. Attachment bytes are not kept.

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// Cap on stored body text so one pathological message cannot bloat the database or the index.
const MAX_BODY_BYTES: usize = 1_000_000;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AttachmentInfo {
    pub part_index: i64,
    pub filename: Option<String>,
    pub content_type: String,
    pub size: i64,
    pub content_id: Option<String>,
    pub inline: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct ParsedBody {
    pub text: Option<String>,
    /// Raw HTML exactly as sent. It is untrusted: the UI must sanitise it before rendering.
    pub html: Option<String>,
    pub attachments: Vec<AttachmentInfo>,
}

impl ParsedBody {
    /// First ~200 characters of text with whitespace collapsed, for list views.
    pub fn preview(&self) -> Option<String> {
        let t = self.text.as_deref()?;
        let collapsed = t.split_whitespace().collect::<Vec<_>>().join(" ");
        if collapsed.is_empty() {
            return None;
        }
        Some(collapsed.chars().take(200).collect())
    }
}

fn truncate(mut s: String) -> String {
    if s.len() > MAX_BODY_BYTES {
        let mut end = MAX_BODY_BYTES;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        s.truncate(end);
    }
    s
}

pub fn parse_body(raw: &[u8]) -> Result<ParsedBody> {
    let msg = mail_parser::MessageParser::default()
        .parse(raw)
        .ok_or_else(|| Error::Other("message could not be parsed as MIME".into()))?;
    let text = msg.body_text(0).map(|t| truncate(t.into_owned())).filter(|t| !t.trim().is_empty());
    let html = msg.body_html(0).map(|h| truncate(h.into_owned())).filter(|h| !h.trim().is_empty());
    let attachments = msg
        .attachments()
        .enumerate()
        .map(|(i, part)| {
            let content_type = part
                .content_type()
                .map(|c| match c.subtype() {
                    Some(sub) => format!("{}/{}", c.ctype(), sub),
                    None => c.ctype().to_string(),
                })
                .unwrap_or_else(|| "application/octet-stream".into());
            let inline = part.is_inline();
            AttachmentInfo {
                part_index: i as i64,
                filename: part.attachment_name().map(str::to_string),
                content_type,
                size: part.decoded_len() as i64,
                content_id: part.content_id().map(str::to_string),
                inline,
            }
        })
        .collect();
    Ok(ParsedBody { text, html, attachments })
}

/// Decoded bytes of one attachment, addressed by the `part_index` that `parse_body` reported.
pub fn extract_attachment(raw: &[u8], part_index: i64) -> Result<Option<(AttachmentInfo, Vec<u8>)>> {
    let msg = mail_parser::MessageParser::default()
        .parse(raw)
        .ok_or_else(|| Error::Other("message could not be parsed as MIME".into()))?;
    let infos = parse_body(raw)?.attachments;
    let Some(info) = infos.into_iter().find(|a| a.part_index == part_index) else { return Ok(None) };
    let part = msg.attachments().nth(part_index as usize);
    Ok(part.map(|p| (info, p.decoded().into_owned())))
}

/// Turn an attacker-controlled filename into a safe file name: no directories, no control
/// characters, no leading dots, bounded length with the extension kept.
pub fn sanitize_filename(name: Option<&str>, fallback_index: i64) -> String {
    let base = name.unwrap_or("").rsplit(['/', '\\']).next().unwrap_or("");
    let cleaned: String = base
        .chars()
        .filter(|c| !c.is_control())
        .map(|c| if matches!(c, ':' | '*' | '?' | '"' | '<' | '>' | '|') { '_' } else { c })
        .collect();
    let cleaned = cleaned.trim().trim_start_matches('.').trim().to_string();
    if cleaned.is_empty() {
        return format!("attachment-{fallback_index}");
    }
    const MAX: usize = 120;
    if cleaned.chars().count() <= MAX {
        return cleaned;
    }
    let (stem, ext) = match cleaned.rfind('.') {
        Some(i) if cleaned.len() - i <= 12 => (&cleaned[..i], &cleaned[i..]),
        _ => (cleaned.as_str(), ""),
    };
    let keep = MAX.saturating_sub(ext.chars().count());
    format!("{}{}", stem.chars().take(keep).collect::<String>(), ext)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MULTIPART: &str = "From: a@example.com\r\nTo: b@example.com\r\nSubject: Report\r\nMIME-Version: 1.0\r\n\
Content-Type: multipart/mixed; boundary=\"B\"\r\n\r\n\
--B\r\nContent-Type: multipart/alternative; boundary=\"A\"\r\n\r\n\
--A\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nHello   team,\r\n\r\nthe quarterly numbers are attached.\r\n\
--A\r\nContent-Type: text/html; charset=utf-8\r\n\r\n<p>Hello <b>team</b></p>\r\n--A--\r\n\
--B\r\nContent-Type: application/pdf; name=\"q3.pdf\"\r\nContent-Disposition: attachment; filename=\"q3.pdf\"\r\n\
Content-Transfer-Encoding: base64\r\n\r\nSGVsbG8gUERG\r\n--B--\r\n";

    #[test]
    fn multipart_alternative_with_attachment() {
        let b = parse_body(MULTIPART.as_bytes()).unwrap();
        assert!(b.text.as_deref().unwrap().contains("quarterly numbers"));
        assert!(b.html.as_deref().unwrap().contains("<b>team</b>"));
        assert_eq!(b.attachments.len(), 1);
        assert_eq!(b.attachments[0].filename.as_deref(), Some("q3.pdf"));
        assert_eq!(b.attachments[0].content_type, "application/pdf");
        assert_eq!(b.attachments[0].size, "Hello PDF".len() as i64, "size is the decoded length");
        assert!(!b.attachments[0].inline);
        assert_eq!(b.preview().unwrap(), "Hello team, the quarterly numbers are attached.");
    }

    #[test]
    fn extracts_decoded_attachment_bytes_by_index() {
        let (info, bytes) = extract_attachment(MULTIPART.as_bytes(), 0).unwrap().unwrap();
        assert_eq!(info.filename.as_deref(), Some("q3.pdf"));
        assert_eq!(bytes, b"Hello PDF");
        assert!(extract_attachment(MULTIPART.as_bytes(), 1).unwrap().is_none());
    }

    #[test]
    fn filenames_cannot_escape_the_target_directory() {
        assert_eq!(sanitize_filename(Some("../../etc/passwd"), 0), "passwd");
        assert_eq!(sanitize_filename(Some("..\\..\\evil.exe"), 0), "evil.exe");
        assert_eq!(sanitize_filename(Some(".bashrc"), 0), "bashrc");
        assert_eq!(sanitize_filename(Some("a\nb\0c.txt"), 0), "abc.txt");
        assert_eq!(sanitize_filename(Some("re:port?.pdf"), 0), "re_port_.pdf");
        assert_eq!(sanitize_filename(Some("   "), 3), "attachment-3");
        assert_eq!(sanitize_filename(None, 7), "attachment-7");
        let long = format!("{}.pdf", "x".repeat(300));
        let out = sanitize_filename(Some(&long), 0);
        assert!(out.chars().count() <= 120 && out.ends_with(".pdf"), "{out}");
    }

    #[test]
    fn html_only_message_still_yields_text() {
        let raw = "From: a@example.com\r\nSubject: x\r\nContent-Type: text/html; charset=utf-8\r\n\r\n<html><body><p>Only <i>html</i> here</p></body></html>\r\n";
        let b = parse_body(raw.as_bytes()).unwrap();
        assert!(b.html.is_some());
        assert!(b.text.as_deref().unwrap_or("").contains("Only"), "text derived from html: {:?}", b.text);
    }

    #[test]
    fn oversized_body_is_truncated_on_a_char_boundary() {
        let big = "é".repeat(MAX_BODY_BYTES); // 2 bytes each
        let raw = format!("From: a@example.com\r\nSubject: x\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n{big}\r\n");
        let b = parse_body(raw.as_bytes()).unwrap();
        assert!(b.text.unwrap().len() <= MAX_BODY_BYTES);
    }
}
