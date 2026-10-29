//! Building and sending outgoing mail.

use crate::config::{Account, Security};
use lettre::message::header::ContentType;
use lettre::message::{Attachment, Mailbox, MultiPart, SinglePart};
use lettre::transport::smtp::authentication::Credentials;
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
    if d.attachments.is_empty() {
        return b.singlepart(text).map_err(|e| e.to_string());
    }
    let mut multi = MultiPart::mixed().singlepart(text);
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

/// Sends and returns the formatted message, for saving into Sent.
pub fn send(acc: &Account, d: &Draft) -> Result<Vec<u8>, String> {
    let msg = build(acc, d)?;
    let pass = acc.password()?;
    let s = &acc.smtp;
    let builder = match s.security {
        Security::Tls => SmtpTransport::relay(&s.host),
        Security::Starttls => SmtpTransport::starttls_relay(&s.host),
        Security::None => Ok(SmtpTransport::builder_dangerous(&s.host)),
    }
    .map_err(|e| e.to_string())?;
    let transport = builder
        .port(s.port)
        .credentials(Credentials::new(acc.login().to_owned(), pass))
        .timeout(Some(Duration::from_secs(30)))
        .build();
    transport.send(&msg).map_err(|e| format!("{}: {e}", s.host))?;
    Ok(msg.formatted())
}

/// Logs in to SMTP without sending, for the account form.
pub fn test(acc: &Account, pass: &str) -> Result<(), String> {
    let s = &acc.smtp;
    let builder = match s.security {
        Security::Tls => SmtpTransport::relay(&s.host),
        Security::Starttls => SmtpTransport::starttls_relay(&s.host),
        Security::None => Ok(SmtpTransport::builder_dangerous(&s.host)),
    }
    .map_err(|e| e.to_string())?;
    let t = builder
        .port(s.port)
        .credentials(Credentials::new(acc.login().to_owned(), pass.to_owned()))
        .timeout(Some(Duration::from_secs(15)))
        .build();
    match t.test_connection() {
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
}
