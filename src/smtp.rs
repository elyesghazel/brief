//! Building and sending outgoing mail.

use crate::config::{Account, Security};
use lettre::message::header::ContentType;
use lettre::message::{Attachment, Mailbox, MultiPart, SinglePart};
use lettre::transport::smtp::authentication::{Credentials, Mechanism};
use lettre::{Message, SmtpTransport, Transport};
use std::path::PathBuf;
use std::time::Duration;

#[derive(Clone, Debug, Default)]
pub struct Draft {
    pub to: String,
    pub cc: String,
    pub bcc: String,
    pub subject: String,
    pub body: String,
    pub in_reply_to: String,
    pub references: String,
    pub attachments: Vec<PathBuf>,
    /// The rich version; `body` is then its plain text alternative.
    pub html: Option<String>,
    /// Images the HTML shows as `cid:`.
    pub inline: Vec<Inline>,
}

#[derive(Clone, Debug)]
pub struct Inline {
    pub cid: String,
    pub mime: String,
    pub data: Vec<u8>,
}

/// Splits "a@x.ch, Lea <lea@y.ch>; b@z" into mailboxes.
pub fn mailboxes(list: &str) -> Result<Vec<Mailbox>, String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    for c in list.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                cur.push(c);
            }
            ',' | ';' if !quoted => {
                out.push(std::mem::take(&mut cur));
            }
            _ => cur.push(c),
        }
    }
    out.push(cur);
    out.into_iter()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .map(|s| s.parse::<Mailbox>().map_err(|e| format!("{s}: {e}")))
        .collect()
}

pub fn build(acc: &Account, d: &Draft) -> Result<Message, String> {
    let from = Mailbox::new(Some(acc.from_name().to_owned()), acc.email.parse().map_err(|e| format!("from: {e}"))?);
    let mut b = Message::builder().from(from).subject(d.subject.clone()).user_agent("brief".into());
    let to = mailboxes(&d.to)?;
    if to.is_empty() {
        return Err("no recipient".into());
    }
    for m in to {
        b = b.to(m);
    }
    for m in mailboxes(&d.cc)? {
        b = b.cc(m);
    }
    for m in mailboxes(&d.bcc)? {
        b = b.bcc(m);
    }
    if !d.in_reply_to.is_empty() {
        b = b.in_reply_to(d.in_reply_to.clone());
        let refs = format!("{} {}", d.references, d.in_reply_to);
        b = b.references(refs.trim().to_owned());
    }
    let text = SinglePart::builder().header(ContentType::TEXT_PLAIN).body(d.body.clone());
    // multipart/alternative of the text and the HTML, the HTML in multipart/related with its images
    let rich = d.html.as_ref().map(|h| {
        let html = SinglePart::html(h.clone());
        let alt = MultiPart::alternative().singlepart(text.clone());
        if d.inline.is_empty() {
            return alt.singlepart(html);
        }
        let related = d.inline.iter().fold(MultiPart::related().singlepart(html), |m, i| {
            let mime = ContentType::parse(&i.mime).unwrap_or(ContentType::parse("application/octet-stream").unwrap());
            m.singlepart(Attachment::new_inline(i.cid.clone()).body(i.data.clone(), mime))
        });
        alt.multipart(related)
    });
    if d.attachments.is_empty() {
        return match rich {
            Some(alt) => b.multipart(alt),
            None => b.singlepart(text),
        }
        .map_err(|e| e.to_string());
    }
    let mut multi = match rich {
        Some(alt) => MultiPart::mixed().multipart(alt),
        None => MultiPart::mixed().singlepart(text),
    };
    for path in &d.attachments {
        let data = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "file".into());
        let mime = ContentType::parse(mime_of(&name)).unwrap_or(ContentType::parse("application/octet-stream").unwrap());
        multi = multi.singlepart(Attachment::new(name).body(data, mime));
    }
    b.multipart(multi).map_err(|e| e.to_string())
}

fn mime_of(name: &str) -> &'static str {
    match name.rsplit('.').next().unwrap_or_default().to_lowercase().as_str() {
        "pdf" => "application/pdf",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "txt" | "md" => "text/plain",
        "html" => "text/html",
        "zip" => "application/zip",
        "csv" => "text/csv",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "mp4" => "video/mp4",
        "mp3" => "audio/mpeg",
        _ => "application/octet-stream",
    }
}

fn transport(acc: &Account, secret: &str, timeout: u64) -> Result<SmtpTransport, String> {
    let s = &acc.smtp;
    let builder = match s.security {
        Security::Tls => SmtpTransport::relay(&s.host),
        Security::Starttls => SmtpTransport::starttls_relay(&s.host),
        Security::None => Ok(SmtpTransport::builder_dangerous(&s.host)),
    }
    .map_err(|e| e.to_string())?
    .port(s.port)
    .credentials(Credentials::new(acc.login().to_owned(), secret.to_owned()))
    .timeout(Some(Duration::from_secs(timeout)));
    let builder = if acc.is_oauth() { builder.authentication(vec![Mechanism::Xoauth2]) } else { builder };
    Ok(builder.build())
}

/// An XOAUTH2 login the server refused: an error challenge or 534/535.
fn auth_refused(e: &lettre::transport::smtp::Error) -> bool {
    e.is_client() || e.status().is_some_and(|c| matches!(u16::from(c), 530 | 534 | 535))
}

/// Sends and returns the formatted message, for saving into Sent.
pub fn send(acc: &Account, d: &Draft) -> Result<Vec<u8>, String> {
    let msg = build(acc, d)?;
    let host = &acc.smtp.host;
    let mut result = transport(acc, &acc.secret()?, 30)?.send(&msg);
    if acc.is_oauth() && result.as_ref().is_err_and(auth_refused) {
        crate::oauth::invalidate(acc);
        result = transport(acc, &acc.secret()?, 30)?.send(&msg);
    }
    result.map_err(|e| format!("{host}: {e}"))?;
    Ok(msg.formatted())
}

/// Logs in to SMTP without sending, for the account form.
pub fn test(acc: &Account, secret: &str) -> Result<(), String> {
    let s = &acc.smtp;
    match transport(acc, secret, 15)?.test_connection() {
        Ok(true) => Ok(()),
        Ok(false) => Err(format!("SMTP {}: no connection", s.host)),
        Err(e) => Err(format!("SMTP {}: {e}", s.host)),
    }
}

/// Gmail and Outlook file sent mail themselves; saving again would duplicate it.
pub fn server_saves_sent(acc: &Account) -> bool {
    let h = acc.smtp.host.to_lowercase();
    h.ends_with("gmail.com") || h.ends_with("office365.com") || h.ends_with("outlook.com")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_addresses() {
        let m = mailboxes("a@x.ch, \"Muster, Lea\" <lea@y.ch>; ").unwrap();
        assert_eq!(m.len(), 2);
        assert_eq!(m[1].email.to_string(), "lea@y.ch");
        assert!(mailboxes("not an address").is_err());
    }

    fn acc() -> Account {
        toml::from_str(
            r#"name = "t"
email = "me@x.ch"
imap = { host = "x", port = 1 }
smtp = { host = "x", port = 1 }"#,
        )
        .unwrap()
    }

    #[test]
    fn plain_stays_single_part() {
        let d = Draft { to: "a@x.ch".into(), body: "hi".into(), ..Default::default() };
        let raw = String::from_utf8(build(&acc(), &d).unwrap().formatted()).unwrap();
        assert!(raw.contains("Content-Type: text/plain"));
        assert!(!raw.contains("multipart"));
    }

    #[test]
    fn rich_is_alternative_with_related_images() {
        let png = vec![137, 80, 78, 71, 1, 2, 3];
        let d = Draft {
            to: "a@x.ch".into(),
            body: "hi *you*".into(),
            html: Some("<b>hi</b> <img src=\"cid:img1@brief\">".into()),
            inline: vec![Inline { cid: "img1@brief".into(), mime: "image/png".into(), data: png }],
            attachments: vec![std::env::current_exe().unwrap()],
            ..Default::default()
        };
        let raw = build(&acc(), &d).unwrap().formatted();
        let msg = mail_parser::MessageParser::default().parse(&raw).unwrap();
        let types: Vec<String> = msg
            .parts()
            .map(|p| p.content_type().map(|c| format!("{}/{}", c.ctype(), c.subtype().unwrap_or(""))).unwrap_or_default())
            .collect();
        assert_eq!(types, ["multipart/mixed", "multipart/alternative", "text/plain", "multipart/related", "text/html", "image/png", "application/octet-stream"]);
        assert_eq!(msg.parts().nth(5).unwrap().content_id(), Some("img1@brief"));
        assert_eq!(msg.body_text(0).unwrap().trim(), "hi *you*");
        let p = crate::message::parse(&raw).unwrap();
        assert_eq!(p.attachments.len(), 1);
        assert_eq!(p.inline.len(), 1);
    }
}
