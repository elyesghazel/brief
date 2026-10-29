//! `~/.config/brief/config.toml`: look, behaviour and the accounts.
//!
//! The file stays hand-editable: brief never rewrites it as a whole. Changing the theme replaces
//! the `theme = ` line, adding an account appends an `[[account]]` block, removing one cuts out
//! just that block.

use serde::Deserialize;
use std::collections::HashMap;
use std::fs;
use std::io::Write as _;
use std::path::PathBuf;
use std::process::Stdio;

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
    /// Title bar. Off on Linux (Hyprland tiles the window anyway), on on Windows.
    pub decorations: bool,
    /// Render HTML mail in the window (WebKit). false = always show it as text.
    pub html: bool,
    /// One list row per conversation, the reader shows the whole conversation.
    pub threads: bool,
    /// Sender icons: "duckduckgo" (the sender sees nothing), "direct" (sharper, asks the
    /// sender's site), "off" (initials only, never online).
    pub favicons: String,
    /// Command for Ctrl+E in the composer, the file path is appended. Empty = $TERMINAL -e $EDITOR
    /// (Windows: %EDITOR%, else Notepad).
    pub editor: String,
    #[serde(rename = "account")]
    pub accounts: Vec<Account>,
    /// OAuth app per provider (`[oauth.google]`, `[oauth.microsoft]`), shared by its accounts.
    pub oauth: HashMap<String, OAuth>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            theme: crate::theme::DEFAULT.into(),
            font: crate::platform::FONT.into(),
            font_size: 10.5,
            html: true,
            threads: true,
            favicons: "duckduckgo".into(),
            sync_minutes: 5,
            cache_limit: 1000,
            notify: true,
            decorations: crate::platform::DECORATIONS,
            editor: String::new(),
            accounts: Vec::new(),
            oauth: HashMap::new(),
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
    /// "password" (default) or "oauth2" (browser sign-in, SASL XOAUTH2).
    #[serde(default)]
    pub auth: Auth,
    #[serde(default)]
    pub oauth: OAuth,
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
pub enum Auth {
    #[default]
    Password,
    Oauth2,
}

/// OAuth app settings. Empty fields fall back to `[oauth.<provider>]`, then to the built-in
/// defaults of the provider (see `oauth::Provider`).
#[derive(Deserialize, Clone, Debug, Default, PartialEq, Eq)]
#[serde(default)]
pub struct OAuth {
    /// "google", "microsoft", or "custom" (then auth_url, token_url and scope are needed).
    pub provider: String,
    pub client_id: String,
    /// Only for providers that want one; Google does for desktop apps.
    pub client_secret: String,
    pub auth_url: String,
    pub token_url: String,
    /// Space-separated scopes.
    pub scope: String,
}

impl OAuth {
    /// Fills empty fields from `other`.
    fn fill(&mut self, other: &OAuth) {
        for (mine, theirs) in [
            (&mut self.client_id, &other.client_id),
            (&mut self.client_secret, &other.client_secret),
            (&mut self.auth_url, &other.auth_url),
            (&mut self.token_url, &other.token_url),
            (&mut self.scope, &other.scope),
        ] {
            if mine.is_empty() {
                mine.clone_from(theirs);
            }
        }
    }
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

    pub fn is_oauth(&self) -> bool {
        self.auth == Auth::Oauth2
    }

    /// What the servers get: the password, or a current OAuth access token.
    pub fn secret(&self) -> Result<String, String> {
        if self.is_oauth() { crate::oauth::access_token(self) } else { self.password() }
    }

    /// The OAuth refresh token from the keyring, else from secrets.toml.
    pub fn refresh_token(&self) -> Result<String, String> {
        crate::platform::keyring_get(OAUTH_SERVICE, &self.email)
            .or_else(|| secrets_file_get(&oauth_key(&self.email)))
            .ok_or_else(|| format!("{} is not signed in: run `brief login {}`", self.email, self.name))
    }

    /// password_cmd, then the plain field, then the keyring (secret-tool or the Windows Credential
    /// Manager), then secrets.toml.
    pub fn password(&self) -> Result<String, String> {
        if !self.password_cmd.is_empty() {
            let out = crate::platform::shell(&self.password_cmd)
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
        if let Some(pass) = crate::platform::keyring_get("brief", &self.email).or_else(|| secrets_file_get(&self.email)) {
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
        if self.is_oauth() {
            out += "auth = \"oauth2\"\n";
            let o = &self.oauth;
            let mut fields = vec![format!("provider = {}", q(&o.provider))];
            for (k, v) in [("client_id", &o.client_id), ("client_secret", &o.client_secret), ("auth_url", &o.auth_url), ("token_url", &o.token_url), ("scope", &o.scope)] {
                if !v.is_empty() {
                    fields.push(format!("{k} = {}", q(v)));
                }
            }
            out += &format!("oauth = {{ {} }}\n", fields.join(", "));
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
# threads = true           # one row per conversation; Ctrl+K toggles it
# favicons = "duckduckgo"  # sender icons: "duckduckgo", "direct" (sharper) or "off"
# sync_minutes = 5        # full sync interval, the inbox is pushed via IDLE anyway
# cache_limit = 1000      # newest messages kept offline per folder
# notify = true
# decorations = false    # title bar, on by default on Windows
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
#
# Gmail / Outlook can sign in through the browser instead (Ctrl+O in the form, or `brief login
# <name>`), with your own OAuth client id:
#
# [oauth.google]
# client_id = "1234-abc.apps.googleusercontent.com"
# client_secret = "GOCSPX-..."
#
# [[account]]
# name = "Gmail"
# email = "me@gmail.com"
# imap = { host = "imap.gmail.com", port = 993, security = "tls" }
# smtp = { host = "smtp.gmail.com", port = 465, security = "tls" }
# auth = "oauth2"
# oauth = { provider = "google" }
"##;

/// Loads the config, writing the commented template on first start.
pub fn load() -> Result<Config, String> {
    let path = path();
    if !path.exists() {
        let _ = fs::create_dir_all(dir());
        let _ = fs::write(&path, TEMPLATE);
    }
    let text = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut cfg: Config = toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    for i in 0..cfg.accounts.len() {
        let acc = cfg.accounts[i].clone();
        cfg.accounts[i] = cfg.resolve_oauth(acc);
    }
    Ok(cfg)
}

impl Config {
    /// Completes an OAuth account's settings from `[oauth.<provider>]` and the built-in defaults.
    pub fn resolve_oauth(&self, mut acc: Account) -> Account {
        if !acc.is_oauth() {
            return acc;
        }
        if acc.oauth.provider.is_empty() {
            acc.oauth.provider = crate::oauth::guess_provider(&acc.email, &acc.imap.host).unwrap_or("custom").into();
        }
        if let Some(shared) = self.oauth.get(&acc.oauth.provider) {
            acc.oauth.fill(shared);
        }
        if let Some(p) = crate::oauth::Provider::from_name(&acc.oauth.provider) {
            acc.oauth.fill(&p.defaults());
        }
        acc
    }
}

/// Replaces (or adds) the top-level `theme = ` line.
pub fn save_theme(name: &str) {
    save_value("theme", &format!("\"{name}\""));
}

/// Replaces (or adds) a top-level `key = value` line, keeping its comment.
pub fn save_value(key: &str, value: &str) {
    let path = path();
    let text = fs::read_to_string(&path).unwrap_or_default();
    let _ = fs::write(&path, set_value(&text, key, value));
}

fn set_value(text: &str, key: &str, value: &str) -> String {
    let line = format!("{key} = {value}");
    let mut done = false;
    let mut out: Vec<String> = Vec::new();
    let mut in_table = false;
    for l in text.lines() {
        if l.trim_start().starts_with('[') {
            in_table = true;
        }
        let is_key = l.trim_start().strip_prefix(key).is_some_and(|r| r.trim_start().starts_with('='));
        if !done && !in_table && is_key {
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
    out.join("\n") + "\n"
}

pub fn append_account(acc: &Account) -> std::io::Result<()> {
    let mut f = fs::OpenOptions::new().append(true).create(true).open(path())?;
    f.write_all(acc.to_toml().as_bytes())
}

/// Keyring service for OAuth refresh tokens, apart from the passwords.
const OAUTH_SERVICE: &str = "brief-oauth";

/// secrets.toml key for a refresh token.
fn oauth_key(email: &str) -> String {
    format!("oauth:{email}")
}

/// `[[account]]`, or `[account.imap]` and friends when `sub`.
fn account_header(line: &str, sub: bool) -> bool {
    let t: String = line.split('#').next().unwrap_or_default().chars().filter(|c| !c.is_whitespace()).collect();
    if sub { t.starts_with("[account.") } else { t == "[[account]]" }
}

fn is_comment(line: &str) -> bool {
    line.trim_start().starts_with('#')
}

/// The config text without the `[[account]]` block named `name` and the comment lines right above
/// it; every other line stays as it was. None when there is no such account, or when the cut
/// would change anything else (then the file needs a hand edit).
pub fn without_account(text: &str, name: &str) -> Option<String> {
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let blank = |i: usize| lines[i].trim().is_empty();
    for start in (0..lines.len()).filter(|&i| account_header(lines[i], false)) {
        let mut end = (start + 1..lines.len())
            .find(|&i| lines[i].trim_start().starts_with('[') && !account_header(lines[i], true))
            .unwrap_or(lines.len());
        // comments and blank lines before the next block (or the end) are not this block's
        while end > start + 1 && (blank(end - 1) || is_comment(lines[end - 1])) {
            end -= 1;
        }
        let Ok(block) = lines[start..end].concat().parse::<toml::Table>() else { continue };
        let this = block.get("account").and_then(|a| a.get(0)).and_then(|a| a.get("name")).and_then(|n| n.as_str());
        if this != Some(name) {
            continue;
        }
        let mut from = start;
        while from > 0 && is_comment(lines[from - 1]) {
            from -= 1;
        }
        if from > 0 && blank(from - 1) {
            from -= 1;
        }
        let out = lines[..from].concat() + &lines[end..].concat();
        // the rest must parse to exactly what it was, minus this account
        let mut want: toml::Table = text.parse().ok()?;
        let list = want.get_mut("account")?.as_array_mut()?;
        let pos = list.iter().position(|a| a.get("name").and_then(|n| n.as_str()) == Some(name))?;
        list.remove(pos);
        if list.is_empty() {
            want.remove("account");
        }
        return (out.parse::<toml::Table>().ok()? == want).then_some(out);
    }
    None
}

/// Removes an account's block from the config file.
pub fn remove_account(name: &str) -> Result<(), String> {
    let path = path();
    let text = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let out = without_account(&text, name).ok_or_else(|| format!("could not cut {name} out of {}, edit it by hand", path.display()))?;
    fs::write(&path, out).map_err(|e| format!("{}: {e}", path.display()))
}

/// Forgets what brief saved for an address: password and OAuth sign-in, in the keyring and in
/// secrets.toml.
pub fn forget_password(email: &str) {
    crate::platform::keyring_clear("brief", email);
    crate::platform::keyring_clear(OAUTH_SERVICE, email);
    let Some(mut table) = fs::read_to_string(secrets_path()).ok().and_then(|t| t.parse::<toml::Table>().ok()) else { return };
    if table.remove(email).is_some() | table.remove(&oauth_key(email)).is_some() {
        let _ = crate::platform::write_private(&secrets_path(), toml::to_string(&table).unwrap_or_default().as_bytes());
    }
}

/// Stores a password in the keyring. Returns false when there is none.
pub fn keyring_store(email: &str, pass: &str) -> bool {
    crate::platform::keyring_store("brief", email, pass)
}

/// Stores an OAuth refresh token: the keyring, else secrets.toml.
pub fn store_refresh_token(email: &str, token: &str) -> Result<(), String> {
    if crate::platform::keyring_store(OAUTH_SERVICE, email, token) {
        return Ok(());
    }
    secrets_file_store(&oauth_key(email), token).map_err(|e| format!("could not store the sign-in: {e}"))
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
    let mut table: toml::Table =
        fs::read_to_string(secrets_path()).ok().and_then(|t| t.parse().ok()).unwrap_or_default();
    table.insert(email.to_owned(), toml::Value::String(pass.to_owned()));
    crate::platform::write_private(&secrets_path(), toml::to_string(&table).unwrap_or_default().as_bytes())
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
            auth: Auth::Password,
            oauth: OAuth::default(),
            signature: String::new(),
            color: String::new(),
        };
        let cfg: Config = toml::from_str(&acc.to_toml()).unwrap();
        assert_eq!(cfg.accounts[0].name, "Gmail \"x\"");
        assert_eq!(cfg.accounts[0].smtp.port, 465);
        assert_eq!(cfg.accounts[0].password_cmd, "pass mail/gmail");
        assert_eq!(cfg.accounts[0].auth, Auth::Password);
    }

    #[test]
    fn oauth_block_round_trips() {
        let (imap, smtp) = guess_servers("me@outlook.com");
        let acc = Account {
            name: "Outlook".into(),
            email: "me@outlook.com".into(),
            display_name: String::new(),
            user: String::new(),
            imap,
            smtp,
            password_cmd: String::new(),
            password: String::new(),
            auth: Auth::Oauth2,
            oauth: OAuth { provider: "microsoft".into(), client_id: "abc".into(), ..Default::default() },
            signature: String::new(),
            color: String::new(),
        };
        let toml = acc.to_toml();
        assert!(toml.contains("oauth = { provider = \"microsoft\", client_id = \"abc\" }"));
        let cfg: Config = toml::from_str(&toml).unwrap();
        assert!(cfg.accounts[0].is_oauth());
        assert_eq!(cfg.accounts[0].oauth, acc.oauth);
    }

    #[test]
    fn oauth_settings_resolve() {
        let cfg: Config = toml::from_str(
            r#"
[oauth.google]
client_id = "shared-id"
client_secret = "shared-secret"

[[account]]
name = "A"
email = "a@gmail.com"
imap = { host = "imap.gmail.com", port = 993 }
smtp = { host = "smtp.gmail.com", port = 465 }
auth = "oauth2"

[[account]]
name = "B"
email = "b@gmail.com"
imap = { host = "imap.gmail.com", port = 993 }
smtp = { host = "smtp.gmail.com", port = 465 }
auth = "oauth2"
oauth = { provider = "google", client_id = "own-id" }

[[account]]
name = "C"
email = "c@example.com"
imap = { host = "mail.example.com", port = 993 }
smtp = { host = "mail.example.com", port = 587 }
"#,
        )
        .unwrap();
        let accs: Vec<Account> = cfg.accounts.iter().map(|a| cfg.resolve_oauth(a.clone())).collect();
        assert_eq!(accs[0].oauth.provider, "google");
        assert_eq!(accs[0].oauth.client_id, "shared-id");
        assert_eq!(accs[0].oauth.scope, "https://mail.google.com/");
        assert_eq!(accs[1].oauth.client_id, "own-id");
        assert_eq!(accs[1].oauth.client_secret, "shared-secret");
        assert!(!accs[2].is_oauth());
        assert_eq!(accs[2].oauth, OAuth::default());
    }

    const SEVERAL: &str = r#"# my mail
theme = "eg"  # dark

# Personal, the main one
[[account]]
name = "Personal"
email = "me@x.ch"
imap = { host = "imap.x.ch", port = 993 }
smtp = { host = "smtp.x.ch", port = 465 }
password_cmd = "pass x"   # keyring is broken here

# work, careful
[[account]]
name = "Work"
email = "me@work.ch"
signature = "-- \nMe"
[account.imap]
host = "imap.work.ch"
port = 993
[account.smtp]
host = "smtp.work.ch"
port = 587
security = "starttls"

[[account]] # the old one
name = "Old"
email = "old@x.ch"
imap = { host = "imap.x.ch", port = 993 }
smtp = { host = "smtp.x.ch", port = 465 }
# trailing note
"#;

    fn names(text: &str) -> Vec<String> {
        toml::from_str::<Config>(text).unwrap().accounts.into_iter().map(|a| a.name).collect()
    }

    #[test]
    fn removes_middle_block_with_subtables() {
        let out = without_account(SEVERAL, "Work").unwrap();
        assert_eq!(names(&out), ["Personal", "Old"]);
        assert!(!out.contains("work"));
        assert!(out.contains("password_cmd = \"pass x\"   # keyring is broken here\n\n[[account]] # the old one"));
        assert!(out.starts_with("# my mail\ntheme = \"eg\"  # dark\n\n# Personal, the main one\n"));
    }

    #[test]
    fn removes_first_and_last_blocks() {
        let out = without_account(SEVERAL, "Personal").unwrap();
        assert_eq!(names(&out), ["Work", "Old"]);
        assert!(out.starts_with("# my mail\ntheme = \"eg\"  # dark\n\n# work, careful\n[[account]]"));
        let out = without_account(SEVERAL, "Old").unwrap();
        assert_eq!(names(&out), ["Personal", "Work"]);
        assert!(out.ends_with("security = \"starttls\"\n# trailing note\n"));
        let none = without_account(&without_account(&out, "Work").unwrap(), "Personal").unwrap();
        assert!(names(&none).is_empty());
        assert_eq!(toml::from_str::<Config>(&none).unwrap().theme, "eg");
    }

    #[test]
    fn appended_block_comes_out_clean() {
        let (imap, smtp) = guess_servers("me@gmail.com");
        let acc = Account {
            name: "Gmail".into(),
            email: "me@gmail.com".into(),
            display_name: String::new(),
            user: String::new(),
            imap,
            smtp,
            password_cmd: String::new(),
            password: String::new(),
            signature: String::new(),
            color: String::new(),
            auth: Auth::Password,
            oauth: OAuth::default(),
        };
        let text = format!("{TEMPLATE}{}", acc.to_toml());
        assert_eq!(without_account(&text, "Gmail").unwrap(), TEMPLATE);
    }

    #[test]
    fn unknown_or_unsafe_cut_is_refused() {
        assert!(without_account(SEVERAL, "Nope").is_none());
        assert!(without_account(TEMPLATE, "Personal").is_none());
        // a multi-line string with a line that looks like a table header
        let tricky = "[[account]]\nname = \"A\"\nemail = \"a@x\"\nsignature = \"\"\"\n[x]\n\"\"\"\n";
        assert!(without_account(tricky, "A").is_none());
    }

    #[test]
    fn template_parses() {
        let cfg: Config = toml::from_str(TEMPLATE).unwrap();
        assert_eq!(cfg.theme, "graphite");
        assert!(cfg.threads);
        assert!(cfg.accounts.is_empty());
    }

    #[test]
    fn value_lines_replace_or_add() {
        let text = set_value(TEMPLATE, "threads", "false");
        let cfg: Config = toml::from_str(&text).unwrap();
        assert!(!cfg.threads);
        assert_eq!(cfg.theme, "graphite");
        let text = set_value(&text, "threads", "true");
        assert!(text.starts_with("threads = true\n"));
        let text = set_value(&text, "theme", "\"eg\"");
        assert!(text.contains("theme = \"eg\"        # Ctrl+T"));
    }
}
