//! `~/.config/brief/config.toml`: look, behaviour and the accounts.
//!
//! The file stays hand-editable: brief never rewrites it as a whole. Changing the theme replaces
//! the `theme = ` line, adding an account appends an `[[account]]` block.

use serde::Deserialize;
use std::fs;
use std::io::Write as _;
use std::path::PathBuf;
use std::process::{Command, Stdio};

#[derive(Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Config {
    /// Built-in theme, a file in ~/.config/brief/themes/, or "zunder" to follow zunder.
    pub theme: String,
    /// CSS font family for everything.
    pub font: String,
    pub font_size: f64,
    /// Full sync of every account, besides IDLE push on the inbox.
    pub sync_minutes: u64,
    /// Newest messages kept per folder.
    pub cache_limit: u32,
    /// Desktop notification (notify-send) for new mail while the window is not focused.
    pub notify: bool,
    /// Draw no title bar. Hyprland tiles the window anyway.
    pub decorations: bool,
    /// Render HTML mail in the window (WebKit). false = always show it as text.
    pub html: bool,
    /// Sender icons: "duckduckgo" (the sender sees nothing), "direct" (sharper, asks the
    /// sender's site), "off" (initials only, never online).
    pub favicons: String,
    /// Command for Ctrl+E in the composer, the file path is appended. Empty = $TERMINAL -e $EDITOR.
    pub editor: String,
    #[serde(rename = "account")]
    pub accounts: Vec<Account>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            theme: crate::theme::DEFAULT.into(),
            font: "Inter, Noto Sans, Cantarell, DejaVu Sans, sans-serif".into(),
            font_size: 10.5,
            html: true,
            favicons: "duckduckgo".into(),
            sync_minutes: 5,
            cache_limit: 1000,
            notify: true,
            decorations: false,
            editor: String::new(),
            accounts: Vec::new(),
        }
    }
}

#[derive(Deserialize, Clone, Debug)]
pub struct Account {
    /// Short label in the sidebar, also the account's id in the cache.
    pub name: String,
    pub email: String,
    /// Name in the From header.
    #[serde(default)]
    pub display_name: String,
    /// Login name, defaults to the email.
    #[serde(default)]
    pub user: String,
    pub imap: Server,
    pub smtp: Server,
    /// Prints the password, e.g. `secret-tool lookup service brief account me@x.ch` or `pass mail/x`.
    #[serde(default)]
    pub password_cmd: String,
    /// Plain password. Prefer password_cmd or the keyring.
    #[serde(default)]
    pub password: String,
    #[serde(default)]
    pub signature: String,
    /// Dot in the unified inbox and the sidebar, e.g. "#f9e2af". Empty = accent.
    #[serde(default)]
    pub color: String,
}

#[derive(Deserialize, Clone, Debug)]
pub struct Server {
    pub host: String,
    pub port: u16,
    #[serde(default)]
    pub security: Security,
}

#[derive(Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Security {
    #[default]
    Tls,
    Starttls,
    None,
}

impl Security {
    pub fn as_str(self) -> &'static str {
        match self {
            Security::Tls => "tls",
            Security::Starttls => "starttls",
            Security::None => "none",
        }
    }
}

impl Account {
    pub fn login(&self) -> &str {
        if self.user.is_empty() { &self.email } else { &self.user }
    }

    pub fn from_name(&self) -> &str {
        if self.display_name.is_empty() { &self.name } else { &self.display_name }
    }

    /// password_cmd, then the plain field, then the keyring (secret-tool), then secrets.toml.
    pub fn password(&self) -> Result<String, String> {
        if !self.password_cmd.is_empty() {
            let out = Command::new("sh")
                .args(["-c", &self.password_cmd])
                .stderr(Stdio::piped())
                .output()
                .map_err(|e| format!("password_cmd: {e}"))?;
            if !out.status.success() {
                return Err(format!("password_cmd failed: {}", String::from_utf8_lossy(&out.stderr).trim()));
            }
            let pass = String::from_utf8_lossy(&out.stdout);
            return Ok(pass.lines().next().unwrap_or_default().to_owned());
        }
        if !self.password.is_empty() {
            return Ok(self.password.clone());
        }
        if let Ok(out) = Command::new("secret-tool").args(["lookup", "service", "brief", "account", &self.email]).output()
            && out.status.success() && !out.stdout.is_empty() {
                return Ok(String::from_utf8_lossy(&out.stdout).trim_end_matches('\n').to_owned());
            }
        if let Some(pass) = secrets_file_get(&self.email) {
            return Ok(pass);
        }
        Err(format!("no password for {} (set password_cmd)", self.email))
    }

    /// The TOML block that `append_account` writes.
    pub fn to_toml(&self) -> String {
        let q = |s: &str| toml::Value::String(s.to_owned()).to_string();
        let mut out = String::from("\n[[account]]\n");
        out += &format!("name = {}\nemail = {}\n", q(&self.name), q(&self.email));
        if !self.display_name.is_empty() {
            out += &format!("display_name = {}\n", q(&self.display_name));
        }
        if !self.user.is_empty() && self.user != self.email {
            out += &format!("user = {}\n", q(&self.user));
        }
        for (key, s) in [("imap", &self.imap), ("smtp", &self.smtp)] {
            out += &format!("{key} = {{ host = {}, port = {}, security = \"{}\" }}\n", q(&s.host), s.port, s.security.as_str());
        }
        if !self.password_cmd.is_empty() {
            out += &format!("password_cmd = {}\n", q(&self.password_cmd));
        }
        out
    }
}

pub fn dir() -> PathBuf {
    dirs::config_dir().unwrap_or_default().join("brief")
}

pub fn path() -> PathBuf {
    dir().join("config.toml")
}

const TEMPLATE: &str = r##"# brief - config. Restart brief after editing accounts by hand.

theme = "graphite"        # Ctrl+T to pick; "zunder" follows zunder's theme
# font = "Inter, Noto Sans, sans-serif"   # emoji come from Apple Color Emoji when installed
# font_size = 10.5
# html = true              # render HTML mail; remote images stay blocked until you press I
# favicons = "duckduckgo"  # sender icons: "duckduckgo", "direct" (sharper) or "off"
# sync_minutes = 5        # full sync interval, the inbox is pushed via IDLE anyway
# cache_limit = 1000      # newest messages kept offline per folder
# notify = true
# decorations = false
# editor = "kitty -e nvim" # Ctrl+E in the composer

# Add accounts with `A` in the app, or by hand:
#
# [[account]]
# name = "Personal"
# email = "me@example.com"
# display_name = "Me"
# imap = { host = "imap.example.com", port = 993, security = "tls" }
# smtp = { host = "smtp.example.com", port = 587, security = "starttls" }
# password_cmd = "secret-tool lookup service brief account me@example.com"
# signature = "-- \nMe"
# color = "#f9e2af"
"##;

/// Loads the config, writing the commented template on first start.
pub fn load() -> Result<Config, String> {
    let path = path();
    if !path.exists() {
        let _ = fs::create_dir_all(dir());
        let _ = fs::write(&path, TEMPLATE);
    }
    let text = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
}

/// Replaces (or adds) the top-level `theme = ` line.
pub fn save_theme(name: &str) {
    let path = path();
    let text = fs::read_to_string(&path).unwrap_or_default();
    let line = format!("theme = \"{name}\"");
    let mut done = false;
    let mut out: Vec<String> = Vec::new();
    let mut in_table = false;
    for l in text.lines() {
        if l.trim_start().starts_with('[') {
            in_table = true;
        }
        if !done && !in_table && l.trim_start().starts_with("theme") && l.contains('=') {
            let comment = l.find('#').map(|i| format!("        {}", &l[i..])).unwrap_or_default();
            out.push(line.clone() + &comment);
            done = true;
        } else {
            out.push(l.to_owned());
        }
    }
    if !done {
        out.insert(0, line);
    }
    let _ = fs::write(&path, out.join("\n") + "\n");
}

pub fn append_account(acc: &Account) -> std::io::Result<()> {
    let mut f = fs::OpenOptions::new().append(true).create(true).open(path())?;
    f.write_all(acc.to_toml().as_bytes())
}

/// Stores a password in the keyring via secret-tool. Returns false when there is none.
pub fn keyring_store(email: &str, pass: &str) -> bool {
    let child = Command::new("secret-tool")
        .args(["store", "--label", &format!("brief: {email}"), "service", "brief", "account", email])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    let Ok(mut child) = child else { return false };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(pass.as_bytes());
    }
    child.wait().is_ok_and(|s| s.success())
}

fn secrets_path() -> PathBuf {
    dir().join("secrets.toml")
}

fn secrets_file_get(email: &str) -> Option<String> {
    let table: toml::Table = fs::read_to_string(secrets_path()).ok()?.parse().ok()?;
    table.get(email)?.as_str().map(str::to_owned)
}

/// Fallback when there is no keyring: `secrets.toml`, mode 0600.
pub fn secrets_file_store(email: &str, pass: &str) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut table: toml::Table =
        fs::read_to_string(secrets_path()).ok().and_then(|t| t.parse().ok()).unwrap_or_default();
    table.insert(email.to_owned(), toml::Value::String(pass.to_owned()));
    let mut f = fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(secrets_path())?;
    f.write_all(toml::to_string(&table).unwrap_or_default().as_bytes())
}

/// Server presets for well-known providers, else a guess from the domain.
pub fn guess_servers(email: &str) -> (Server, Server) {
    let domain = email.rsplit('@').next().unwrap_or_default().to_lowercase();
    let s = |host: &str, port, security| Server { host: host.into(), port, security };
    use Security::*;
    match domain.as_str() {
        "gmail.com" | "googlemail.com" => (s("imap.gmail.com", 993, Tls), s("smtp.gmail.com", 465, Tls)),
        "outlook.com" | "hotmail.com" | "live.com" | "msn.com" | "outlook.de" | "hotmail.ch" => {
            (s("outlook.office365.com", 993, Tls), s("smtp.office365.com", 587, Starttls))
        }
        "icloud.com" | "me.com" | "mac.com" => (s("imap.mail.me.com", 993, Tls), s("smtp.mail.me.com", 587, Starttls)),
        "yahoo.com" | "ymail.com" => (s("imap.mail.yahoo.com", 993, Tls), s("smtp.mail.yahoo.com", 465, Tls)),
        "fastmail.com" | "fastmail.fm" => (s("imap.fastmail.com", 993, Tls), s("smtp.fastmail.com", 465, Tls)),
        "gmx.ch" | "gmx.net" | "gmx.de" | "gmx.com" => (s("imap.gmx.net", 993, Tls), s("mail.gmx.net", 587, Starttls)),
        "bluewin.ch" => (s("imaps.bluewin.ch", 993, Tls), s("smtpauths.bluewin.ch", 465, Tls)),
        "proton.me" | "protonmail.com" | "pm.me" => (s("127.0.0.1", 1143, Starttls), s("127.0.0.1", 1025, Starttls)),
        "zoho.com" | "zohomail.com" => (s("imap.zoho.com", 993, Tls), s("smtp.zoho.com", 465, Tls)),
        "" => (s("", 993, Tls), s("", 587, Starttls)),
        d => (s(&format!("mail.{d}"), 993, Tls), s(&format!("mail.{d}"), 587, Starttls)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_block_round_trips() {
        let (imap, smtp) = guess_servers("me@gmail.com");
        let acc = Account {
            name: "Gmail \"x\"".into(),
            email: "me@gmail.com".into(),
            display_name: String::new(),
            user: String::new(),
            imap,
            smtp,
            password_cmd: "pass mail/gmail".into(),
            password: String::new(),
            signature: String::new(),
            color: String::new(),
        };
        let cfg: Config = toml::from_str(&acc.to_toml()).unwrap();
        assert_eq!(cfg.accounts[0].name, "Gmail \"x\"");
        assert_eq!(cfg.accounts[0].smtp.port, 465);
        assert_eq!(cfg.accounts[0].password_cmd, "pass mail/gmail");
    }

    #[test]
    fn template_parses() {
        let cfg: Config = toml::from_str(TEMPLATE).unwrap();
        assert_eq!(cfg.theme, "graphite");
        assert!(cfg.accounts.is_empty());
    }
}
