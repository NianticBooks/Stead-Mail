//! Sanitising untrusted HTML mail for display.
//!
//! Layers: this module strips scripts, active content, dangerous URLs and tracking images; the UI
//! then shows the result in a sandboxed frame without scripts under its own Content-Security-Policy.
//! Remote images are off by default because loading one tells the sender you opened the message.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SanitizedHtml {
    /// An HTML fragment safe to place in a `<body>`.
    pub html: String,
    /// Remote images that were removed (always 0 when remote images are allowed).
    pub blocked_images: usize,
    pub remote_images_allowed: bool,
}

/// Declarations that can fetch resources, run code, or lay content over the surrounding UI.
fn clean_style(value: &str) -> String {
    const BAD_PROPS: [&str; 5] = ["position", "behavior", "-moz-binding", "z-index", "filter"];
    const BAD_VALUES: [&str; 9] = ["url(", "expression", "javascript:", "@import", "image-set", "image(", "src(", "\\", "-moz-binding"];
    value
        .split(';')
        .filter_map(|decl| {
            let (prop, val) = decl.trim().split_once(':')?;
            let prop = prop.trim().to_ascii_lowercase();
            let lower = val.to_ascii_lowercase();
            if prop.is_empty() || BAD_PROPS.contains(&prop.as_str()) || BAD_VALUES.iter().any(|b| lower.contains(b)) {
                return None;
            }
            Some(format!("{prop}:{}", val.trim()))
        })
        .collect::<Vec<_>>()
        .join(";")
}

fn is_inline_image(src: &str) -> bool {
    let s = src.trim().to_ascii_lowercase();
    // SVG can carry script and external references, so only raster formats are inline-safe.
    ["data:image/png", "data:image/jpeg", "data:image/jpg", "data:image/gif", "data:image/webp"].iter().any(|p| s.starts_with(p))
}

pub fn sanitize_html(input: &str, allow_remote_images: bool) -> SanitizedHtml {
    let blocked = Arc::new(AtomicUsize::new(0));
    let counter = blocked.clone();

    let mut b = ammonia::Builder::default();
    b.add_tags(["center", "font", "u", "s", "strike"])
        .add_generic_attributes([
            "style", "align", "valign", "bgcolor", "border", "cellpadding", "cellspacing", "color", "size", "face", "width", "height", "dir",
        ])
        .url_schemes(HashSet::from(["http", "https", "mailto", "tel", "data"]))
        .link_rel(Some("noopener noreferrer nofollow"))
        .strip_comments(true)
        .attribute_filter(move |element, attr, value| {
            let v = value.trim();
            match (element, attr) {
                (_, "style") => {
                    let cleaned = clean_style(value);
                    if cleaned.is_empty() { None } else { Some(cleaned.into()) }
                }
                ("img", "src") => {
                    let lower = v.to_ascii_lowercase();
                    if is_inline_image(v) || (allow_remote_images && lower.starts_with("https://")) {
                        Some(v.to_string().into())
                    } else {
                        if lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("//") {
                            counter.fetch_add(1, Ordering::Relaxed);
                        }
                        None
                    }
                }
                ("a", "href") => {
                    let lower = v.to_ascii_lowercase();
                    if ["http://", "https://", "mailto:", "tel:"].iter().any(|p| lower.starts_with(p)) { Some(v.to_string().into()) } else { None }
                }
                _ => Some(value.into()),
            }
        });
    let html = b.clean(input).to_string();
    SanitizedHtml { html, blocked_images: blocked.load(Ordering::Relaxed), remote_images_allowed: allow_remote_images }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clean(s: &str) -> String {
        sanitize_html(s, false).html.to_ascii_lowercase()
    }

    #[test]
    fn strips_active_content() {
        let attacks = [
            "<script>alert(1)</script>hi",
            "<img src=x onerror=alert(1)>",
            "<a href=\"javascript:alert(1)\">x</a>",
            "<a href=\"JaVaScRiPt:alert(1)\">x</a>",
            "<a href=\"  javascript:alert(1)\">x</a>",
            "<iframe src=\"https://evil.test\"></iframe>",
            "<object data=\"x\"></object><embed src=\"x\">",
            "<form action=\"https://evil.test\"><input name=p></form>",
            "<svg onload=alert(1)><circle/></svg>",
            "<meta http-equiv=\"refresh\" content=\"0;url=https://evil.test\">",
            "<base href=\"https://evil.test/\">",
            "<link rel=stylesheet href=\"https://evil.test/x.css\">",
            "<div onclick=\"alert(1)\">x</div>",
            "<a href=\"data:text/html;base64,PHNjcmlwdD4=\">x</a>",
            "<math><mtext><script>alert(1)</script></mtext></math>",
            "<style>body{background:url(https://evil.test/t.gif)}</style>x",
            "<button formaction=\"https://evil.test\">x</button>",
        ];
        for a in attacks {
            let out = clean(a);
            for needle in ["<script", "onerror", "onclick", "onload", "javascript:", "<iframe", "<object", "<embed", "<form", "<input", "<svg", "<meta", "<base", "<link", "<style", "data:text", "formaction", "evil.test"] {
                assert!(!out.contains(needle), "{a:?} -> {out:?} still contains {needle}");
            }
        }
    }

    #[test]
    fn keeps_ordinary_formatting_and_safe_links() {
        let out = sanitize_html(
            "<table width=600 bgcolor=\"#fff\"><tr><td align=center><b>Hi</b> <i>there</i> <a href=\"https://example.com/x?a=1&b=2\">link</a> <a href=\"mailto:a@b.com\">mail</a></td></tr></table><blockquote>quoted</blockquote><ul><li>one</li></ul>",
            false,
        )
        .html;
        for keep in ["<table", "<b>Hi</b>", "<i>there</i>", "href=\"https://example.com/x?a=1&amp;b=2\"", "mailto:a@b.com", "<blockquote>", "<li>one</li>"] {
            assert!(out.contains(keep), "{out}");
        }
        assert!(out.contains("noopener") && out.contains("noreferrer"), "{out}");
    }

    #[test]
    fn remote_images_are_blocked_and_counted_until_allowed() {
        let html = "<p>a</p><img src=\"https://tracker.test/p.gif?u=1\" width=1 height=1><img src=\"http://insecure.test/x.png\"><img src=\"//proto-relative.test/x.png\">\
                    <img src=\"data:image/png;base64,iVBORw0KGgo=\" alt=\"inline\"><img src=\"data:image/svg+xml;base64,PHN2Zy8+\">";
        let blocked = sanitize_html(html, false);
        assert_eq!(blocked.blocked_images, 3, "http, https and protocol-relative are counted: {}", blocked.html);
        assert!(!blocked.html.contains("tracker.test") && !blocked.html.contains("insecure.test") && !blocked.html.contains("proto-relative"));
        assert!(blocked.html.contains("data:image/png"), "inline raster images survive");
        assert!(!blocked.html.contains("svg+xml"), "inline SVG does not");

        let allowed = sanitize_html(html, true);
        assert!(allowed.remote_images_allowed);
        assert_eq!(allowed.blocked_images, 1 + 1, "only insecure and protocol-relative http remain blocked");
        assert!(allowed.html.contains("https://tracker.test/p.gif"));
        assert!(!allowed.html.contains("insecure.test"), "plain http is never loaded");
    }

    #[test]
    fn styles_lose_anything_that_fetches_or_overlays() {
        let out = sanitize_html(
            "<div style=\"color:red; background:url(https://evil.test/a.png); position:fixed; font-size:14px; width:expression(alert(1)); background-image:image-set('x.png' 1x); margin:0 \\75rl(x)\">x</div>",
            false,
        )
        .html;
        assert!(out.contains("color:red") && out.contains("font-size:14px"), "{out}");
        for bad in ["url(", "position", "expression", "image-set", "evil.test", "\\"] {
            assert!(!out.contains(bad), "{out} still has {bad}");
        }
        assert_eq!(clean_style("position:absolute;z-index:9"), "");
    }

    #[test]
    fn deeply_nested_or_malformed_input_does_not_panic() {
        let nested = "<div>".repeat(5000) + "x";
        let _ = sanitize_html(&nested, false);
        let _ = sanitize_html("<<<>>><a href=<img src=x>", false);
        let _ = sanitize_html("", false);
        assert_eq!(sanitize_html("plain text", false).html, "plain text");
    }
}
