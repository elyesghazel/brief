//! Server settings for domains brief has no preset for: Thunderbird's ISPDB, then the domain's
//! own `autoconfig.<domain>` (the clientConfig v1.1 format). Short timeouts; any failure just
//! means brief falls back to `mail.<domain>`.

use crate::config::{Security, Server};
use std::time::Duration;

/// What an autoconfig file says about IMAP and SMTP.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    pub imap: Server,
    pub smtp: Server,
    /// The login, when it is not the address (`%EMAILLOCALPART%`).
    pub user: String,
}

/// Asks the ISPDB, then autoconfig.<domain>, each within `timeout`.
pub fn lookup(email: &str, timeout: Duration) -> Option<Found> {
    let domain = email.rsplit_once('@')?.1.to_lowercase();
    let ispdb = std::env::var("BRIEF_ISPDB_URL").unwrap_or_else(|_| "https://autoconfig.thunderbird.net/v1.1/".into());
    let urls = [
        format!("{ispdb}{domain}"),
        format!("https://autoconfig.{domain}/mail/config-v1.1.xml?emailaddress={email}"),
    ];
    let agent: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(timeout)).build().into();
    urls.iter().find_map(|u| {
        let body = agent
            .get(u)
            .header("User-Agent", concat!("brief-mail/", env!("CARGO_PKG_VERSION")))
            .call()
            .ok()?
            .body_mut()
            .with_config()
            .limit(256 * 1024)
            .read_to_string()
            .ok()?;
        parse(&body, email)
    })
}

/// The first IMAP and SMTP server of a clientConfig v1.1 document.
pub fn parse(xml: &str, email: &str) -> Option<Found> {
    let (local, domain) = email.rsplit_once('@')?;
    let fill = |s: &str| s.replace("%EMAILDOMAIN%", domain).replace("%EMAILADDRESS%", email).replace("%EMAILLOCALPART%", local);
    let server = |tag: &str, kind: &str| -> Option<(Server, String)> {
        let block = blocks(xml, tag).into_iter().find(|b| b.contains(&format!("type=\"{kind}\"")))?;
        let host = fill(&inner(block, "hostname")?);
        let port = inner(block, "port")?.parse().ok()?;
        let security = match inner(block, "socketType").unwrap_or_default().to_uppercase().as_str() {
            "SSL" | "TLS" => Security::Tls,
            "STARTTLS" => Security::Starttls,
            "PLAIN" => Security::None,
            _ => return None,
        };
        let user = inner(block, "username").unwrap_or_default();
        Some((Server { host, port, security }, user))
    };
    let (imap, user) = server("incomingServer", "imap")?;
    let (smtp, _) = server("outgoingServer", "smtp")?;
    let user = if user.contains("%EMAILLOCALPART%") { fill(&user) } else { String::new() };
    (!imap.host.is_empty() && !smtp.host.is_empty()).then_some(Found { imap, smtp, user })
}

/// Every `<tag ...>...</tag>` element's text, opening tag included.
fn blocks<'a>(xml: &'a str, tag: &str) -> Vec<&'a str> {
    let (open, close) = (format!("<{tag}"), format!("</{tag}>"));
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(start) = rest.find(&open) {
        let Some(len) = rest[start..].find(&close) else { break };
        out.push(&rest[start..start + len]);
        rest = &rest[start + len + close.len()..];
    }
    out
}

/// The trimmed text of the first `<tag>` inside `block`.
fn inner(block: &str, tag: &str) -> Option<String> {
    let start = block.find(&format!("<{tag}>"))? + tag.len() + 2;
    let end = block[start..].find(&format!("</{tag}>"))?;
    Some(block[start..start + end].trim().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    const ISPDB: &str = r#"<?xml version="1.0"?>
<clientConfig version="1.1">
  <emailProvider id="example.ch">
    <domain>example.ch</domain>
    <incomingServer type="pop3">
      <hostname>pop.example.ch</hostname><port>995</port><socketType>SSL</socketType>
    </incomingServer>
    <incomingServer type="imap">
      <hostname>imap.%EMAILDOMAIN%</hostname>
      <port>143</port>
      <socketType>STARTTLS</socketType>
      <username>%EMAILLOCALPART%</username>
    </incomingServer>
    <outgoingServer type="smtp">
      <hostname>smtp.example.ch</hostname>
      <port>465</port>
      <socketType>SSL</socketType>
      <username>%EMAILADDRESS%</username>
    </outgoingServer>
  </emailProvider>
</clientConfig>"#;

    #[test]
    fn reads_client_config() {
        let f = parse(ISPDB, "lea@example.ch").unwrap();
        assert_eq!(f.imap, Server { host: "imap.example.ch".into(), port: 143, security: Security::Starttls });
        assert_eq!(f.smtp, Server { host: "smtp.example.ch".into(), port: 465, security: Security::Tls });
        assert_eq!(f.user, "lea");
        let plain = ISPDB.replace("%EMAILLOCALPART%", "%EMAILADDRESS%");
        assert_eq!(parse(&plain, "lea@example.ch").unwrap().user, "");
    }

    #[test]
    fn rejects_incomplete_documents() {
        assert_eq!(parse("<html>not found</html>", "a@b.ch"), None);
        assert_eq!(parse(&ISPDB.replace("type=\"imap\"", "type=\"pop3\""), "a@b.ch"), None);
        assert_eq!(parse(&ISPDB.replace("STARTTLS", "weird"), "a@b.ch"), None);
        assert_eq!(parse(ISPDB, "no-at-sign"), None);
    }
}
