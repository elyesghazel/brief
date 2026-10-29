//! Sender icons: the favicon of the sender's organisation, cached on disk, or initials.
//!
//! Icons are fetched on a background thread from DuckDuckGo's icon service by default, so the
//! sender never sees a request. `favicons = "direct"` asks the sender's site for its
//! apple-touch-icon first (sharper), `"off"` never goes online.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::mpsc::{self, Sender};
use std::time::{Duration, SystemTime};

/// Personal-mail providers: their logo says nothing about the person, so these get initials.
const FREEMAIL: &[&str] = &[
    "gmail.com", "googlemail.com", "outlook.com", "hotmail.com", "hotmail.ch", "hotmail.de", "live.com", "live.de",
    "msn.com", "yahoo.com", "yahoo.de", "ymail.com", "icloud.com", "me.com", "mac.com", "gmx.ch", "gmx.net", "gmx.de",
    "gmx.at", "gmx.com", "web.de", "bluewin.ch", "hispeed.ch", "sunrise.ch", "protonmail.com", "proton.me", "pm.me",
    "aol.com", "mail.com", "yandex.com", "yandex.ru", "t-online.de", "posteo.de", "mailbox.org", "tutanota.com",
    "tuta.io", "fastmail.com", "zoho.com", "outlook.de", "outlook.ch", "freenet.de",
];

/// Second-level suffixes where the organisation sits one label further left.
const SUFFIX2: &[&str] = &["co.uk", "org.uk", "ac.uk", "gov.uk", "com.au", "co.nz", "co.jp", "com.br", "co.za", "com.tr", "co.in", "com.cn", "co.kr"];

/// `news.post.ch` -> `post.ch`; None for personal-mail providers and odd input.
pub fn org_domain(addr: &str) -> Option<String> {
    let domain = addr.rsplit_once('@')?.1.trim().trim_end_matches('>').to_lowercase();
    if domain.is_empty() || !domain.contains('.') || domain.contains(|c: char| !(c.is_ascii_alphanumeric() || c == '.' || c == '-')) {
        return None;
    }
    let labels: Vec<&str> = domain.split('.').filter(|l| !l.is_empty()).collect();
    let take = if labels.len() >= 3 && SUFFIX2.contains(&labels[labels.len() - 2..].join(".").as_str()) { 3 } else { 2 };
    let org = labels[labels.len().saturating_sub(take)..].join(".");
    (!FREEMAIL.contains(&org.as_str())).then_some(org)
}

/// Up to two letters for an initials avatar.
pub fn initials(name: &str, addr: &str) -> String {
    let source = if name.trim().is_empty() { addr.split('@').next().unwrap_or(addr) } else { name };
    let words: Vec<&str> = source
        .split(|c: char| c.is_whitespace() || c == '.' || c == '_' || c == '-')
        .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()))
        .filter(|w| !w.is_empty())
        .collect();
    let mut out: String = words.iter().take(2).filter_map(|w| w.chars().next()).flat_map(char::to_uppercase).collect();
    if out.is_empty() {
        out = "?".into();
    }
    out
}

/// Stable color slot 0..8 for an address.
pub fn color_slot(addr: &str) -> usize {
    addr.to_lowercase().bytes().fold(5381u32, |h, b| h.wrapping_mul(33) ^ b as u32) as usize % 8
}

pub fn dir() -> PathBuf {
    dirs::cache_dir().unwrap_or_else(std::env::temp_dir).join("brief/icons")
}

pub enum Cached {
    Icon(Vec<u8>),
    /// Looked up recently, nothing there.
    None,
    Unknown,
}

const NONE_TTL: Duration = Duration::from_secs(14 * 24 * 3600);

pub fn cached(domain: &str) -> Cached {
    let base = dir();
    if let Ok(bytes) = std::fs::read(base.join(format!("{domain}.img"))) {
        return Cached::Icon(bytes);
    }
    let none = base.join(format!("{domain}.none"));
    match std::fs::metadata(&none).and_then(|m| m.modified()) {
        Ok(t) if SystemTime::now().duration_since(t).unwrap_or_default() < NONE_TTL => Cached::None,
        _ => Cached::Unknown,
    }
}

fn looks_like_image(b: &[u8]) -> bool {
    b.len() > 120 && (b.starts_with(b"\x89PNG") || b.starts_with(&[0, 0, 1, 0]) || b.starts_with(b"\xff\xd8") || b.starts_with(b"GIF8") || b.starts_with(b"RIFF"))
}

fn get(agent: &ureq::Agent, url: &str) -> Option<Vec<u8>> {
    let mut resp = agent.get(url).header("User-Agent", "brief-mail/0.2 (favicon)").call().ok()?;
    let body = resp.body_mut().with_config().limit(512 * 1024).read_to_vec().ok()?;
    looks_like_image(&body).then_some(body)
}

/// Starts the fetch thread. Send it domains; it writes the cache and calls `done(domain)`.
pub fn spawn(mode: String, done: impl Fn(String) + Send + 'static) -> Sender<String> {
    let (tx, rx) = mpsc::channel::<String>();
    std::thread::Builder::new()
        .name("favicons".into())
        .spawn(move || {
            let _ = std::fs::create_dir_all(dir());
            let agent: ureq::Agent = ureq::Agent::config_builder()
                .timeout_global(Some(Duration::from_secs(6)))
                .http_status_as_error(true)
                .build()
                .into();
            let mut tried = HashSet::new();
            for domain in rx {
                if !tried.insert(domain.clone()) || !matches!(cached(&domain), Cached::Unknown) {
                    continue;
                }
                let mut urls = Vec::new();
                if mode == "direct" {
                    urls.push(format!("https://{domain}/apple-touch-icon.png"));
                }
                urls.push(format!("https://icons.duckduckgo.com/ip3/{domain}.ico"));
                let found = urls.iter().find_map(|u| get(&agent, u));
                match found {
                    Some(bytes) => {
                        let _ = std::fs::write(dir().join(format!("{domain}.img")), bytes);
                        done(domain);
                    }
                    None => {
                        let _ = std::fs::write(dir().join(format!("{domain}.none")), b"");
                    }
                }
            }
        })
        .unwrap();
    tx
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn org_domains() {
        assert_eq!(org_domain("notifications@news.post.ch").as_deref(), Some("post.ch"));
        assert_eq!(org_domain("a@shop.example.co.uk").as_deref(), Some("example.co.uk"));
        assert_eq!(org_domain("lea@gmail.com"), None);
        assert_eq!(org_domain("x@mail.bluewin.ch"), None);
        assert_eq!(org_domain("broken"), None);
        assert_eq!(org_domain("a@b/../c.ch"), None);
    }

    #[test]
    fn initials_from_names() {
        assert_eq!(initials("Lea Muster", "lea@x.ch"), "LM");
        assert_eq!(initials("", "elyes.ghazel@x.ch"), "EG");
        assert_eq!(initials("\"IKEA\"", "x@ikea.ch"), "I");
        assert_eq!(initials("", ""), "?");
    }
}
