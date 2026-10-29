//! Turning raw RFC 5322 bytes into what the reader and the composer need.

use mail_parser::{Message, MessageParser};

pub struct Attachment {
    pub name: String,
    pub mime: String,
    pub size: usize,
    pub data: Vec<u8>,
}

pub struct Parsed {
    pub subject: String,
    pub from: String,
    pub from_addr: String,
    pub reply_to: Vec<String>,
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub date: i64,
    pub msgid: String,
    pub references: String,
    /// The body as text: text/plain if there is one, else the HTML rendered to text.
    pub text: String,
    pub html: Option<String>,
    /// True when `text` came from HTML.
    pub from_html: bool,
    pub attachments: Vec<Attachment>,
    /// Images the HTML references as `cid:` (content id, mime, bytes).
    pub inline: Vec<(String, String, Vec<u8>)>,
}

fn list(addrs: Option<mail_parser::AddressList<'_>>) -> Vec<String> {
    let Some(addrs) = addrs else { return Vec::new() };
    addrs
        .mailboxes()
        .filter_map(|m| {
            let addr = m.address()?;
            Some(match m.name() {
                Some(n) if !n.is_empty() && n != addr => format!("{n} <{addr}>"),
                _ => addr.to_owned(),
            })
        })
        .collect()
}

/// Drops `<img>` tags: remote images are blocked anyway and their alt texts are clutter.
fn strip_images(html: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let mut out = String::with_capacity(html.len());
    let mut i = 0;
    while let Some(start) = lower[i..].find("<img").map(|s| s + i) {
        out += &html[i..start];
        match lower[start..].find('>') {
            Some(end) => i = start + end + 1,
            None => {
                i = html.len();
                break;
            }
        }
    }
    out += &html[i..];
    out
}

pub fn html_to_text(html: &str, width: usize) -> String {
    // raw mode reads layout tables cell by cell instead of side by side, which is what mail needs
    let html = strip_images(html);
    let text = html2text::config::plain()
        .raw_mode(true)
        .link_footnotes(true)
        .string_from_read(html.as_bytes(), width)
        .unwrap_or_else(|_| html.to_owned());
    let mut out = String::with_capacity(text.len());
    let mut blank = 0;
    for line in text.lines() {
        let line = line.trim_end();
        // link brackets around block content end up on their own lines: "[" ... "][3]"
        if line == "[" || (line.starts_with("][") && line.ends_with(']') && line[2..line.len() - 1].chars().all(|c| c.is_ascii_digit())) {
            continue;
        }
        let empty = line.chars().all(|c| c.is_whitespace() || c == '\u{200c}' || c == '\u{a0}');
        blank = if empty { blank + 1 } else { 0 };
        if blank <= 1 {
            out += if empty { "" } else { line };
            out.push('\n');
        }
    }
    out.trim().to_owned() + "\n"
}

/// Collapses whitespace and cuts a body down to a one-line preview.
pub fn preview(text: &str) -> String {
    let mut out = String::new();
    for word in text.lines().filter(|l| !l.trim_start().starts_with('>')).flat_map(str::split_whitespace) {
        if word.starts_with("http") || word.starts_with('[') || !word.chars().any(char::is_alphanumeric) {
            continue;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out += word;
        if out.chars().count() > 160 {
            break;
        }
    }
    out.chars().take(160).collect()
}

fn body(msg: &Message<'_>) -> (String, Option<String>, bool) {
    let html = msg.body_html(0).map(|h| h.into_owned());
    let plain = msg
        .text_body()
        .next()
        .filter(|p| p.is_content_type("text", "plain"))
        .and_then(|p| p.text())
        .map(|t| t.into_owned())
        .filter(|t| !t.trim().is_empty());
    match (plain, &html) {
        (Some(text), _) => (text, html, false),
        (None, Some(h)) => (html_to_text(h, 90), html, true),
        (None, None) => (String::new(), None, false),
    }
}

pub fn parse(raw: &[u8]) -> Option<Parsed> {
    let msg = MessageParser::default().parse(raw)?;
    let from = msg.from().and_then(|f| f.first());
    let (text, html, from_html) = body(&msg);
    let attachments = msg
        .attachments()
        .filter(|p| {
            let explicit = p.content_disposition().is_some_and(|d| d.ctype().eq_ignore_ascii_case("attachment"));
            // inline images referenced by the HTML (logos, signatures) are not attachments
            let embedded = p.content_id().is_some() && p.content_type().is_some_and(|c| c.ctype().eq_ignore_ascii_case("image"));
            let body_text = p.is_content_type("text", "plain") || p.is_content_type("text", "html");
            let signature = p.content_type().is_some_and(|c| c.subtype().is_some_and(|s| s.ends_with("pkcs7-signature") || s == "pgp-signature"));
            if signature {
                return false;
            }
            explicit || (!embedded && !body_text && (!p.is_inline() || p.attachment_name().is_some()))
        })
        .map(|p| {
            let data = p.decoded().into_owned();
            let mime = p
                .content_type()
                .map(|c| format!("{}/{}", c.ctype(), c.subtype().unwrap_or("octet-stream")))
                .unwrap_or_else(|| "application/octet-stream".into());
            Attachment {
                name: p.attachment_name().unwrap_or("attachment").to_owned(),
                mime,
                size: data.len(),
                data,
            }
        })
        .collect();
    let inline = msg
        .parts()
        .filter(|p| p.content_type().is_some_and(|c| c.ctype().eq_ignore_ascii_case("image")))
        .filter_map(|p| {
            let cid = p.content_id()?.trim_matches(['<', '>']).to_owned();
            let mime = p.content_type().map(|c| format!("image/{}", c.subtype().unwrap_or("png"))).unwrap_or_default();
            Some((cid, mime, p.decoded().into_owned()))
        })
        .collect();
    let refs = msg.references().map(|r| r.iter().map(|s| format!("<{s}>")).collect::<Vec<_>>().join(" "));
    Some(Parsed {
        subject: msg.subject().unwrap_or("(no subject)").to_owned(),
        from: from.as_ref().and_then(|m| m.name()).or(from.as_ref().and_then(|m| m.address())).unwrap_or("").to_owned(),
        from_addr: from.as_ref().and_then(|m| m.address()).unwrap_or("").to_owned(),
        reply_to: list(msg.reply_to()),
        to: list(msg.to()),
        cc: list(msg.cc()),
        date: msg.date().map(|d| d.to_timestamp()).unwrap_or(0),
        msgid: msg.message_id().map(|m| format!("<{m}>")).unwrap_or_default(),
        references: refs.unwrap_or_default(),
        text,
        html,
        from_html,
        attachments,
        inline,
    })
}

/// Fields of a header-only fetch, for the list.
pub struct Summary {
    pub subject: String,
    pub from_name: String,
    pub from_addr: String,
    pub to: String,
    pub cc: String,
    pub date: i64,
    pub msgid: String,
    pub refs: String,
    pub multipart_mixed: bool,
}

pub fn summary(header: &[u8]) -> Summary {
    let parser = MessageParser::default();
    let msg = parser.parse_headers(header);
    let Some(msg) = msg else {
        return Summary {
            subject: "(unreadable)".into(),
            from_name: String::new(),
            from_addr: String::new(),
            to: String::new(),
            cc: String::new(),
            date: 0,
            msgid: String::new(),
            refs: String::new(),
            multipart_mixed: false,
        };
    };
    let from = msg.from().and_then(|f| f.first());
    Summary {
        subject: msg.subject().unwrap_or("(no subject)").to_owned(),
        from_name: from.as_ref().and_then(|m| m.name()).unwrap_or("").to_owned(),
        from_addr: from.as_ref().and_then(|m| m.address()).unwrap_or("").to_owned(),
        to: list(msg.to()).join(", "),
        cc: list(msg.cc()).join(", "),
        date: msg.date().map(|d| d.to_timestamp()).unwrap_or(0),
        msgid: msg.message_id().map(|m| format!("<{m}>")).unwrap_or_default(),
        refs: msg.references().map(|r| r.iter().map(|s| format!("<{s}>")).collect::<Vec<_>>().join(" ")).unwrap_or_default(),
        multipart_mixed: msg.content_type().is_some_and(|c| c.subtype().is_some_and(|s| s.eq_ignore_ascii_case("mixed"))),
    }
}

/// Preview text and whether the message has real attachments, once the body is fetched.
pub fn digest(raw: &[u8]) -> (String, bool) {
    match parse(raw) {
        Some(p) => (preview(&p.text), !p.attachments.is_empty()),
        None => (String::new(), false),
    }
}

/// `> `-quoted body with an attribution line, for replies.
pub fn quote(p: &Parsed) -> String {
    let when = chrono::DateTime::from_timestamp(p.date, 0)
        .map(|d| d.with_timezone(&chrono::Local).format("%a, %d %b %Y %H:%M").to_string())
        .unwrap_or_default();
    let mut out = format!("\n\nOn {when}, {} wrote:\n", if p.from.is_empty() { &p.from_addr } else { &p.from });
    for line in p.text.trim_end().lines() {
        out += if line.starts_with('>') { ">" } else { "> " };
        out += line;
        out.push('\n');
    }
    out
}

pub fn prefixed(prefix: &str, subject: &str) -> String {
    let lower = subject.to_lowercase();
    let p = prefix.to_lowercase();
    if lower.starts_with(&p) || (p == "re:" && (lower.starts_with("aw:") || lower.starts_with("sv:"))) {
        subject.to_owned()
    } else {
        format!("{prefix} {subject}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RAW: &[u8] = b"From: Lea Muster <lea@example.com>\r\nTo: me@example.com, Bob <bob@example.com>\r\n\
Subject: =?utf-8?q?Gr=C3=BCezi?=\r\nDate: Tue, 27 Oct 2026 10:00:00 +0100\r\nMessage-ID: <abc@x>\r\n\
Content-Type: multipart/alternative; boundary=b\r\n\r\n--b\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n\
Hallo zusammen\r\n> old\r\nsee https://x.ch\r\n--b\r\nContent-Type: text/html\r\n\r\n<p>Hallo <b>zusammen</b></p>\r\n--b--\r\n";

    #[test]
    fn parses_plain_first() {
        let p = parse(RAW).unwrap();
        assert_eq!(p.subject, "Grüezi");
        assert_eq!(p.from, "Lea Muster");
        assert_eq!(p.to, ["me@example.com", "Bob <bob@example.com>"]);
        assert!(p.text.starts_with("Hallo zusammen"));
        assert!(!p.from_html && p.html.is_some());
        assert_eq!(p.msgid, "<abc@x>");
        assert_eq!(preview(&p.text), "Hallo zusammen see");
        assert!(quote(&p).contains("> Hallo zusammen\n>> old"));
    }

    #[test]
    fn subject_prefix_once() {
        assert_eq!(prefixed("Re:", "Re: x"), "Re: x");
        assert_eq!(prefixed("Re:", "AW: x"), "AW: x");
        assert_eq!(prefixed("Fwd:", "x"), "Fwd: x");
    }

    #[test]
    fn summary_from_headers() {
        let s = summary(RAW);
        assert_eq!(s.from_addr, "lea@example.com");
        assert_eq!(s.date, 1_793_091_600);
    }
}

fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if c.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}

/// How many remote resources (images, styles, fonts) an HTML mail would load.
pub fn count_remote(html: &str) -> usize {
    let lower = html.to_ascii_lowercase();
    ["src=\"http", "src='http", "src=http", "url(http", "url(\"http", "url('http", "background=\"http", "srcset=\"http", "href=\"http"]
        .iter()
        .map(|p| if *p == "href=\"http" { lower.matches("rel=\"stylesheet\"").count().min(lower.matches(p).count()) } else { lower.matches(p).count() })
        .sum()
}

/// The HTML as the reader loads it: `cid:` images inlined, a base style, and a Content Security
/// Policy that blocks every remote load unless `allow_remote`. Scripts never run.
pub fn prepare_html(html: &str, inline: &[(String, String, Vec<u8>)], allow_remote: bool) -> String {
    let mut html = html.to_owned();
    for (cid, mime, data) in inline {
        let needle = format!("cid:{cid}");
        if html.contains(&needle) {
            html = html.replace(&needle, &format!("data:{mime};base64,{}", base64(data)));
        }
    }
    let csp = if allow_remote {
        "default-src 'none'; img-src * data: blob:; style-src * 'unsafe-inline'; font-src * data:; media-src *"
    } else {
        "default-src 'none'; img-src data:; style-src 'unsafe-inline'; font-src data:"
    };
    let head = format!(
        "<meta charset=\"utf-8\"><meta http-equiv=\"Content-Security-Policy\" content=\"{csp}\">\
         <meta name=\"referrer\" content=\"no-referrer\">\
         <style>html{{background:#fff;color:#1a1a1a}}body{{margin:0;padding:20px 24px;font-family:Inter,\"Noto Sans\",sans-serif;font-size:14px;line-height:1.45;overflow-wrap:anywhere}}img{{max-width:100%;height:auto}}table{{max-width:100%}}pre{{white-space:pre-wrap}}</style>"
    );
    let lower = html.to_ascii_lowercase();
    // the policy has to sit in <head>, after a doctype (before it would switch to quirks mode)
    let at = lower
        .find("<head")
        .and_then(|i| lower[i..].find('>').map(|e| i + e + 1))
        .or_else(|| lower.find("<html").and_then(|i| lower[i..].find('>').map(|e| i + e + 1)))
        .or_else(|| lower.find("<!doctype").and_then(|i| lower[i..].find('>').map(|e| i + e + 1)))
        .unwrap_or(0);
    html.insert_str(at, &head);
    html
}

/// Plain text as a small HTML page, for "open in browser" of text mail.
pub fn text_page(text: &str) -> String {
    let esc = text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
    format!("<pre>{esc}</pre>")
}

#[cfg(test)]
mod html_tests {
    use super::*;

    #[test]
    fn policy_lands_in_head_and_cids_inline() {
        let html = "<!DOCTYPE html><html><head><title>x</title></head><body><img src=\"cid:logo@x\"><img src=\"https://t.co/p.gif\"></body></html>";
        let out = prepare_html(html, &[("logo@x".into(), "image/png".into(), vec![1, 2, 3])], false);
        assert!(out.starts_with("<!DOCTYPE html><html><head><meta charset"));
        assert!(out.contains("img-src data:;"));
        assert!(out.contains("data:image/png;base64,AQID"));
        assert_eq!(count_remote(html), 1);
        assert!(prepare_html(html, &[], true).contains("img-src * data:"));
    }

    #[test]
    fn base64_padding() {
        assert_eq!(base64(b"ab"), "YWI=");
        assert_eq!(base64(b"abc"), "YWJj");
        assert_eq!(base64(b"a"), "YQ==");
    }
}
