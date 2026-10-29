//! OAuth2 sign-in for Gmail and Microsoft: authorization code flow with PKCE and a loopback
//! redirect, then SASL XOAUTH2 for IMAP and SMTP.
//!
//! The refresh token is stored like a password (keyring, else secrets.toml). Access tokens stay in
//! memory and are refreshed when they run out or a server rejects them.

use crate::config::{self, Account, OAuth, Security, Server};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ring::rand::SecureRandom as _;
use std::collections::HashMap;
use std::io::{BufRead as _, BufReader, Write as _};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// brief's own OAuth apps. A build takes them from `BRIEF_GOOGLE_CLIENT_ID`,
/// `BRIEF_GOOGLE_CLIENT_SECRET` and `BRIEF_MICROSOFT_CLIENT_ID` when set, else from the second
/// argument: the ids of the apps registered for brief (Google Cloud project, Entra ID app). Empty
/// = this build has none, the user brings a client id in config.toml.
const GOOGLE_CLIENT_ID: &str = built_in(option_env!("BRIEF_GOOGLE_CLIENT_ID"), "");
const GOOGLE_CLIENT_SECRET: &str = built_in(option_env!("BRIEF_GOOGLE_CLIENT_SECRET"), "");
const MICROSOFT_CLIENT_ID: &str = built_in(option_env!("BRIEF_MICROSOFT_CLIENT_ID"), "");

/// The build-time value unless it is missing or empty.
const fn built_in(env: Option<&'static str>, fallback: &'static str) -> &'static str {
    match env {
        Some(v) if !v.is_empty() => v,
        _ => fallback,
    }
}

/// The README section for people without built-in ids.
pub const README_SECTION: &str = "\"Gmail and Outlook without app passwords\" in the README";

/// A token this close to expiry counts as expired.
const MARGIN: Duration = Duration::from_secs(60);
/// How long the browser sign-in may take.
const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Provider {
    Google,
    Microsoft,
}

impl Provider {
    pub fn from_name(name: &str) -> Option<Provider> {
        match name {
            "google" => Some(Provider::Google),
            "microsoft" => Some(Provider::Microsoft),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Provider::Google => "google",
            Provider::Microsoft => "microsoft",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Provider::Google => "Google",
            Provider::Microsoft => "Microsoft",
        }
    }

    /// IMAP and SMTP for accounts of this provider, also on a custom domain.
    pub fn servers(self) -> (Server, Server) {
        let s = |host: &str, port, security| Server { host: host.into(), port, security };
        match self {
            Provider::Google => (s("imap.gmail.com", 993, Security::Tls), s("smtp.gmail.com", 465, Security::Tls)),
            Provider::Microsoft => (s("outlook.office365.com", 993, Security::Tls), s("smtp.office365.com", 587, Security::Starttls)),
        }
    }

    /// Endpoints, scopes and the built-in client.
    pub fn defaults(self) -> OAuth {
        let o = |client_id: &str, client_secret: &str, auth_url: &str, token_url: &str, scope: &str| OAuth {
            provider: String::new(),
            client_id: client_id.into(),
            client_secret: client_secret.into(),
            auth_url: auth_url.into(),
            token_url: token_url.into(),
            scope: scope.into(),
        };
        match self {
            Provider::Google => o(
                GOOGLE_CLIENT_ID,
                GOOGLE_CLIENT_SECRET,
                "https://accounts.google.com/o/oauth2/v2/auth",
                "https://oauth2.googleapis.com/token",
                "https://mail.google.com/ openid email profile",
            ),
            Provider::Microsoft => o(
                MICROSOFT_CLIENT_ID,
                "",
                "https://login.microsoftonline.com/common/oauth2/v2.0/authorize",
                "https://login.microsoftonline.com/common/oauth2/v2.0/token",
                "https://outlook.office.com/IMAP.AccessAsUser.All https://outlook.office.com/SMTP.Send offline_access openid profile email",
            ),
        }
    }
}

/// The provider behind a free mail address: gmail.com, googlemail.com, and Microsoft's outlook.*,
/// hotmail.*, live.* and msn.com. Custom domains need `provider_for_mx`.
pub fn provider_for_address(email: &str) -> Option<Provider> {
    let domain = email.trim().rsplit_once('@')?.1.to_lowercase();
    let (first, rest) = domain.split_once('.')?;
    // outlook.com, hotmail.co.uk, live.de; not live.example.com
    let tld = !rest.is_empty() && rest.split('.').count() <= 2 && rest.split('.').all(|l| (2..=3).contains(&l.len()));
    match (first, rest) {
        ("gmail" | "googlemail", "com") => Some(Provider::Google),
        ("msn", "com") => Some(Provider::Microsoft),
        ("outlook" | "hotmail" | "live", _) if tld => Some(Provider::Microsoft),
        _ => None,
    }
}

/// The provider that receives a domain's mail, from its MX hosts (Google Workspace, Microsoft 365).
pub fn provider_for_mx<'a>(hosts: impl IntoIterator<Item = &'a str>) -> Option<Provider> {
    hosts.into_iter().find_map(|h| {
        let h = h.trim_end_matches('.').to_lowercase();
        if h.ends_with(".google.com") || h.ends_with(".googlemail.com") {
            Some(Provider::Google)
        } else if h.ends_with(".mail.protection.outlook.com") {
            Some(Provider::Microsoft)
        } else {
            None
        }
    })
}

/// Looks up a domain's MX over DNS-over-HTTPS (Cloudflare), within `timeout`. None on any error.
pub fn lookup_mx(domain: &str, timeout: Duration) -> Option<Vec<String>> {
    let agent: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(timeout)).build().into();
    let url = std::env::var("BRIEF_DOH_URL").unwrap_or_else(|_| "https://cloudflare-dns.com/dns-query".into());
    let body = agent
        .get(&url)
        .query("name", domain)
        .query("type", "MX")
        .header("Accept", "application/dns-json")
        .header("User-Agent", concat!("brief-mail/", env!("CARGO_PKG_VERSION")))
        .call()
        .ok()?
        .body_mut()
        .with_config()
        .limit(64 * 1024)
        .read_to_string()
        .ok()?;
    let v: serde_json::Value = serde_json::from_str(&body).ok()?;
    let answers = v.get("Answer")?.as_array()?;
    // "10 aspmx.l.google.com."
    Some(answers.iter().filter_map(|a| a.get("data")?.as_str()?.split_whitespace().last().map(str::to_owned)).collect())
}

/// "google" or "microsoft" for addresses and servers that support the browser sign-in.
pub fn guess_provider(email: &str, imap_host: &str) -> Option<&'static str> {
    if let Some(p) = provider_for_address(email) {
        return Some(p.name());
    }
    match imap_host.to_lowercase().as_str() {
        "imap.gmail.com" => Some("google"),
        "outlook.office365.com" | "imap-mail.outlook.com" => Some("microsoft"),
        _ => None,
    }
}

/// This build (or the config) has a client id for the provider.
pub fn has_client(o: &OAuth) -> bool {
    !o.client_id.is_empty()
}

/// Microsoft matches loopback redirects by the name `localhost`, Google wants the address.
fn redirect_host(provider: &str) -> &'static str {
    if provider == "microsoft" { "localhost" } else { "127.0.0.1" }
}

/// Everything set to start a sign-in, else what to add to the config.
pub fn check(o: &OAuth) -> Result<(), String> {
    let table = if o.provider.is_empty() { "custom" } else { &o.provider };
    if o.client_id.is_empty() {
        let who = Provider::from_name(&o.provider).map_or("this provider", Provider::label);
        return Err(format!("this build has no {who} sign-in: add [oauth.{table}] to config.toml, see {README_SECTION}"));
    }
    if o.auth_url.is_empty() || o.token_url.is_empty() || o.scope.is_empty() {
        return Err(format!("[oauth.{table}] needs auth_url, token_url and scope"));
    }
    Ok(())
}

/// The SASL XOAUTH2 initial response, before base64.
pub fn xoauth2(user: &str, token: &str) -> String {
    format!("user={user}\x01auth=Bearer {token}\x01\x01")
}

fn random_bytes<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    ring::rand::SystemRandom::new().fill(&mut b).expect("system random");
    b
}

/// RFC 7636 proof key: the verifier stays here, the S256 challenge goes to the browser.
pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

impl Pkce {
    pub fn new() -> Pkce {
        Pkce::from_verifier(URL_SAFE_NO_PAD.encode(random_bytes::<32>()))
    }

    fn from_verifier(verifier: String) -> Pkce {
        let digest = ring::digest::digest(&ring::digest::SHA256, verifier.as_bytes());
        Pkce { challenge: URL_SAFE_NO_PAD.encode(digest), verifier }
    }
}

pub fn authorize_url(o: &OAuth, redirect: &str, pkce: &Pkce, state: &str, login_hint: &str) -> String {
    let mut q = form_urlencoded::Serializer::new(String::new());
    q.append_pair("response_type", "code")
        .append_pair("client_id", &o.client_id)
        .append_pair("redirect_uri", redirect)
        .append_pair("scope", &o.scope)
        .append_pair("code_challenge", &pkce.challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("state", state);
    if !login_hint.is_empty() {
        q.append_pair("login_hint", login_hint);
    }
    if o.provider == "google" {
        // without these Google hands out no refresh token on a repeated sign-in
        q.append_pair("access_type", "offline").append_pair("prompt", "consent");
    }
    let sep = if o.auth_url.contains('?') { '&' } else { '?' };
    format!("{}{sep}{}", o.auth_url, q.finish())
}

#[derive(Clone, Debug)]
pub struct Tokens {
    pub access: String,
    pub refresh: Option<String>,
    pub expires: Instant,
    /// Who signed in, from the OpenID id_token when the provider sent one.
    pub id: Identity,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Identity {
    pub name: String,
    pub email: String,
}

impl Identity {
    /// The claims of an id_token. It came straight from the token endpoint over TLS, so the
    /// signature is not checked (OpenID Connect Core 3.1.3.7).
    fn from_jwt(jwt: &str) -> Identity {
        let claims = jwt
            .split('.')
            .nth(1)
            .and_then(|p| URL_SAFE_NO_PAD.decode(p.trim_end_matches('=')).ok())
            .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok());
        let Some(c) = claims else { return Identity::default() };
        let s = |k: &str| c.get(k).and_then(|x| x.as_str()).unwrap_or_default().trim().to_owned();
        Identity { name: s("name"), email: s("email") }
    }
}

/// The browser signed in a different Google account than the form's address.
pub fn other_account(typed: &str, id: &Identity) -> Option<String> {
    let norm = |a: &str| a.trim().to_lowercase().replace("@googlemail.com", "@gmail.com");
    (!id.email.is_empty() && norm(&id.email) != norm(typed)).then(|| id.email.clone())
}

impl Tokens {
    fn fresh(&self, now: Instant) -> bool {
        now + MARGIN < self.expires
    }
}

/// Reads a token endpoint response (RFC 6749 5.1 / 5.2).
fn parse_tokens(body: &str, now: Instant) -> Result<Tokens, String> {
    let v: serde_json::Value = serde_json::from_str(body).map_err(|_| {
        let snippet: String = body.chars().take(120).collect();
        format!("token endpoint: unexpected answer: {snippet}")
    })?;
    let s = |k: &str| v.get(k).and_then(|x| x.as_str()).filter(|x| !x.is_empty()).map(str::to_owned);
    if let Some(err) = s("error") {
        return Err(match s("error_description") {
            Some(d) => format!("{err}: {}", d.lines().next().unwrap_or_default()),
            None => err,
        });
    }
    let access = s("access_token").ok_or("token endpoint: no access_token")?;
    // some servers send the number as a string
    let secs = match v.get("expires_in") {
        Some(serde_json::Value::Number(n)) => n.as_u64(),
        Some(serde_json::Value::String(t)) => t.parse().ok(),
        _ => None,
    };
    let id = s("id_token").map(|t| Identity::from_jwt(&t)).unwrap_or_default();
    Ok(Tokens { access, refresh: s("refresh_token"), expires: now + Duration::from_secs(secs.unwrap_or(3600)), id })
}

fn post_form(url: &str, form: &[(&str, &str)]) -> Result<Tokens, String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(20)))
        .http_status_as_error(false)
        .build()
        .into();
    let mut resp = agent
        .post(url)
        .header("User-Agent", concat!("brief-mail/", env!("CARGO_PKG_VERSION")))
        .header("Accept", "application/json")
        .send_form(form.iter().copied())
        .map_err(|e| format!("token endpoint: {e}"))?;
    let status = resp.status();
    let body = resp.body_mut().with_config().limit(64 * 1024).read_to_string().map_err(|e| format!("token endpoint: {e}"))?;
    parse_tokens(&body, Instant::now()).map_err(|e| if status.is_success() { e } else { format!("{e} (HTTP {})", status.as_u16()) })
}

fn with_client<'a>(o: &'a OAuth, mut form: Vec<(&'a str, &'a str)>) -> Vec<(&'a str, &'a str)> {
    form.push(("client_id", &o.client_id));
    if !o.client_secret.is_empty() {
        form.push(("client_secret", &o.client_secret));
    }
    form
}

fn exchange(o: &OAuth, code: &str, verifier: &str, redirect: &str) -> Result<Tokens, String> {
    let form = vec![("grant_type", "authorization_code"), ("code", code), ("code_verifier", verifier), ("redirect_uri", redirect)];
    post_form(&o.token_url, &with_client(o, form))
}

fn refresh(o: &OAuth, token: &str) -> Result<Tokens, String> {
    post_form(&o.token_url, &with_client(o, vec![("grant_type", "refresh_token"), ("refresh_token", token)]))
}

/// The `code` from the redirect's request line (`GET /?code=..&state=.. HTTP/1.1`).
/// `Ok(None)` for anything else the browser asks for, like the favicon.
fn parse_callback(request_line: &str, state: &str) -> Result<Option<String>, String> {
    let target = request_line.split_whitespace().nth(1).unwrap_or_default();
    let Some((_, query)) = target.split_once('?') else { return Ok(None) };
    let params: HashMap<String, String> = form_urlencoded::parse(query.as_bytes()).into_owned().collect();
    if let Some(err) = params.get("error") {
        let desc = params.get("error_description").map(|d| format!(": {d}")).unwrap_or_default();
        return Err(format!("sign-in refused ({err}{desc})"));
    }
    let Some(code) = params.get("code") else { return Ok(None) };
    if params.get("state").map(String::as_str) != Some(state) {
        return Err("sign-in answer does not match the request (state)".into());
    }
    Ok(Some(code.clone()))
}

/// The page the browser lands on after the sign-in: black on white (white on black in dark
/// mode), nothing loaded from elsewhere.
fn respond(mut stream: TcpStream, ok: bool, text: &str) {
    let text = text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
    let title = if ok { "Signed in" } else { "Sign-in failed" };
    let html = format!(
        "<!doctype html><html lang=en><meta charset=utf-8><meta name=viewport content=\"width=device-width\">\
         <title>brief: {title}</title><style>\
         :root{{color-scheme:light dark}}\
         body{{margin:0;min-height:100vh;display:grid;place-items:center;background:#fff;color:#000;\
         font:15px/1.5 system-ui,-apple-system,\"Segoe UI\",sans-serif}}\
         main{{border:1px solid currentColor;border-radius:3px;padding:28px 32px;max-width:26em;margin:24px}}\
         b{{display:block;font-size:12px;letter-spacing:.12em;text-transform:uppercase;opacity:.55;margin-bottom:14px}}\
         h1{{font-size:20px;margin:0 0 6px}}p{{margin:0;opacity:.75}}\
         @media(prefers-color-scheme:dark){{body{{background:#000;color:#fff}}}}\
         </style><main><b>brief</b><h1>{title}</h1><p>{text}</p></main></html>"
    );
    let status = if ok { "200 OK" } else { "400 Bad Request" };
    let _ = write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{html}",
        html.len()
    );
}

/// Waits for the browser to come back to one of the loopback listeners.
fn wait_for_code(listeners: &[TcpListener], state: &str, cancel: &AtomicBool) -> Result<String, String> {
    let start = Instant::now();
    for l in listeners {
        l.set_nonblocking(true).map_err(|e| e.to_string())?;
    }
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err("sign-in cancelled".into());
        }
        if start.elapsed() > SIGN_IN_TIMEOUT {
            return Err("sign-in timed out".into());
        }
        let Some(stream) = listeners.iter().find_map(|l| l.accept().ok().map(|(s, _)| s)) else {
            std::thread::sleep(Duration::from_millis(100));
            continue;
        };
        let _ = stream.set_nonblocking(false);
        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
        let mut line = String::new();
        let Ok(reader) = stream.try_clone() else { continue };
        if BufReader::new(reader).read_line(&mut line).is_err() {
            continue;
        }
        match parse_callback(&line, state) {
            Ok(Some(code)) => {
                respond(stream, true, "You can close this tab and go back to brief.");
                return Ok(code);
            }
            Ok(None) => respond(stream, false, "This is not the page brief is waiting for."),
            Err(e) => {
                respond(stream, false, &e);
                return Err(e);
            }
        }
    }
}

/// Runs the browser sign-in for `acc` (settings already resolved): opens `open(url)`, waits for
/// the redirect, trades the code for tokens and keeps them in memory. Storing the refresh token
/// is up to the caller.
pub fn sign_in(acc: &Account, open: impl Fn(&str), cancel: &AtomicBool) -> Result<Tokens, String> {
    let o = &acc.oauth;
    check(o)?;
    let listener = TcpListener::bind("127.0.0.1:0").map_err(|e| format!("loopback listener: {e}"))?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let mut listeners = vec![listener];
    // `localhost` may resolve to ::1 first
    if let Ok(v6) = TcpListener::bind(("::1", port)) {
        listeners.push(v6);
    }
    let redirect = format!("http://{}:{port}", redirect_host(&o.provider));
    let pkce = Pkce::new();
    let state = URL_SAFE_NO_PAD.encode(random_bytes::<16>());
    open(&authorize_url(o, &redirect, &pkce, &state, &acc.email));
    let code = wait_for_code(&listeners, &state, cancel)?;
    let tokens = exchange(o, &code, &pkce.verifier, &redirect)?;
    if tokens.refresh.is_none() {
        return Err("the provider sent no refresh token (is offline access allowed?)".into());
    }
    remember(acc, &tokens);
    Ok(tokens)
}

fn cache() -> &'static Mutex<HashMap<String, Tokens>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Tokens>>> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

pub fn remember(acc: &Account, tokens: &Tokens) {
    cache().lock().unwrap().insert(acc.email.clone(), tokens.clone());
    signed_out().lock().unwrap().remove(&acc.email);
}

/// Addresses whose sign-in ran out or was revoked; they need the browser again.
fn signed_out() -> &'static Mutex<std::collections::HashSet<String>> {
    static SET: OnceLock<Mutex<std::collections::HashSet<String>>> = OnceLock::new();
    SET.get_or_init(Default::default)
}

/// The account has to sign in with the browser again before it can sync.
pub fn needs_sign_in(acc: &Account) -> bool {
    acc.is_oauth() && signed_out().lock().unwrap().contains(&acc.email)
}

/// Drops the access token after a server refused it, so the next call refreshes.
pub fn invalidate(acc: &Account) {
    if let Some(t) = cache().lock().unwrap().get_mut(&acc.email) {
        t.expires = Instant::now();
    }
}

/// A current access token: from memory, else refreshed with the stored refresh token.
pub fn access_token(acc: &Account) -> Result<String, String> {
    let cached = cache().lock().unwrap().get(&acc.email).cloned();
    if let Some(t) = &cached
        && t.fresh(Instant::now()) {
            return Ok(t.access.clone());
        }
    check(&acc.oauth)?;
    let again = |why: &str| {
        eprintln!("brief [{}] sign-in gone: {why}", acc.name);
        signed_out().lock().unwrap().insert(acc.email.clone());
        format!("signed out, sign in again (Ctrl+K, or `brief login {}`)", acc.name)
    };
    let stored = match cached.and_then(|t| t.refresh) {
        Some(r) => r,
        None => acc.refresh_token().map_err(|e| again(&e))?,
    };
    let mut tokens = refresh(&acc.oauth, &stored).map_err(|e| {
        if e.starts_with("invalid_grant") {
            again(&e)
        } else {
            format!("token refresh: {e}")
        }
    })?;
    match &tokens.refresh {
        // Microsoft rotates refresh tokens
        Some(new) if *new != stored => {
            if let Err(e) = config::store_refresh_token(&acc.email, new) {
                eprintln!("brief [{}] {e}", acc.name);
            }
        }
        _ => tokens.refresh = Some(stored),
    }
    remember(acc, &tokens);
    Ok(tokens.access)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_matches_rfc7636() {
        let p = Pkce::from_verifier("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk".into());
        assert_eq!(p.challenge, "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
        let fresh = Pkce::new();
        assert_eq!(fresh.verifier.len(), 43);
        assert_ne!(fresh.verifier, Pkce::new().verifier);
    }

    #[test]
    fn xoauth2_string() {
        assert_eq!(xoauth2("me@gmail.com", "ya29.x"), "user=me@gmail.com\x01auth=Bearer ya29.x\x01\x01");
    }

    #[test]
    fn token_responses() {
        let now = Instant::now();
        let t = parse_tokens(r#"{"access_token":"a1","expires_in":3599,"refresh_token":"r1","token_type":"Bearer"}"#, now).unwrap();
        assert_eq!(t.access, "a1");
        assert_eq!(t.refresh.as_deref(), Some("r1"));
        assert!(t.fresh(now));
        assert!(t.fresh(now + Duration::from_secs(3500)));
        assert!(!t.fresh(now + Duration::from_secs(3560)));
        let t = parse_tokens(r#"{"access_token":"a2","expires_in":"120"}"#, now).unwrap();
        assert!(t.refresh.is_none());
        assert!(!t.fresh(now + Duration::from_secs(61)));
        let e = parse_tokens(r#"{"error":"invalid_grant","error_description":"Token has been expired or revoked."}"#, now);
        assert_eq!(e.unwrap_err(), "invalid_grant: Token has been expired or revoked.");
        assert!(parse_tokens("<html>", now).is_err());
        assert!(parse_tokens("{}", now).is_err());
    }

    #[test]
    fn callbacks() {
        let line = "GET /?state=s1&code=4%2F0Ab&scope=x HTTP/1.1\r\n";
        assert_eq!(parse_callback(line, "s1").unwrap().as_deref(), Some("4/0Ab"));
        assert!(parse_callback(line, "other").is_err());
        assert_eq!(parse_callback("GET /favicon.ico HTTP/1.1", "s1").unwrap(), None);
        let denied = parse_callback("GET /?error=access_denied&state=s1 HTTP/1.1", "s1").unwrap_err();
        assert!(denied.contains("access_denied"));
    }

    #[test]
    fn authorize_urls() {
        let mut o = Provider::Google.defaults();
        o.provider = "google".into();
        o.client_id = "id 1".into();
        let p = Pkce::from_verifier("v".repeat(43));
        let url = authorize_url(&o, "http://127.0.0.1:5000", &p, "st", "me@gmail.com");
        assert!(url.starts_with("https://accounts.google.com/o/oauth2/v2/auth?response_type=code&client_id=id+1&"));
        assert!(url.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A5000"));
        assert!(url.contains("scope=https%3A%2F%2Fmail.google.com%2F"));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains("login_hint=me%40gmail.com"));
        assert!(url.contains("access_type=offline"));
        let mut ms = Provider::Microsoft.defaults();
        ms.provider = "microsoft".into();
        let url = authorize_url(&ms, "http://localhost:5000", &p, "st", "");
        assert!(url.contains("offline_access"));
        assert!(!url.contains("access_type") && !url.contains("login_hint"));
    }

    #[test]
    fn built_in_ids() {
        assert_eq!(built_in(Some("from-env"), "in-source"), "from-env");
        assert_eq!(built_in(Some(""), "in-source"), "in-source");
        assert_eq!(built_in(None, "in-source"), "in-source");
        assert_eq!(Provider::Google.defaults().client_id, GOOGLE_CLIENT_ID);
        assert_eq!(Provider::Microsoft.defaults().client_secret, "");
        assert!(Provider::Microsoft.defaults().auth_url.contains("/common/"));
    }

    #[test]
    fn free_mail_domains() {
        use Provider::*;
        for (addr, want) in [
            ("me@gmail.com", Some(Google)),
            ("Me@GoogleMail.com", Some(Google)),
            ("me@outlook.com", Some(Microsoft)),
            ("me@outlook.de", Some(Microsoft)),
            ("me@hotmail.co.uk", Some(Microsoft)),
            ("me@hotmail.ch", Some(Microsoft)),
            ("me@live.com", Some(Microsoft)),
            ("me@live.de", Some(Microsoft)),
            ("me@msn.com", Some(Microsoft)),
            ("me@gmail.ch", None),
            ("me@live.example.com", None),
            ("me@outlookfans.com", None),
            ("me@firma.ch", None),
            ("me@live", None),
            ("gmail.com", None),
        ] {
            assert_eq!(provider_for_address(addr), want, "{addr}");
        }
    }

    #[test]
    fn mx_hosts() {
        assert_eq!(provider_for_mx(["alt1.aspmx.l.google.com.", "aspmx.l.google.com."]), Some(Provider::Google));
        assert_eq!(provider_for_mx(["firma-ch.mail.protection.outlook.com"]), Some(Provider::Microsoft));
        assert_eq!(provider_for_mx(["mx01.mail.ch", "smtp.google.com.evil.ch"]), None);
        assert_eq!(provider_for_mx([]), None);
    }

    #[test]
    fn id_tokens() {
        let claims = URL_SAFE_NO_PAD.encode(r#"{"email":"me@gmail.com","name":"Elyes G","sub":"1"}"#);
        let t = parse_tokens(&format!(r#"{{"access_token":"a","id_token":"eyJhbGciOiJub25lIn0.{claims}.sig"}}"#), Instant::now()).unwrap();
        assert_eq!(t.id, Identity { name: "Elyes G".into(), email: "me@gmail.com".into() });
        assert_eq!(Identity::from_jwt("garbage"), Identity::default());
        assert_eq!(other_account("me@googlemail.com", &t.id), None);
        assert_eq!(other_account("ME@gmail.com", &t.id), None);
        assert_eq!(other_account("you@gmail.com", &t.id).as_deref(), Some("me@gmail.com"));
        assert_eq!(other_account("you@gmail.com", &Identity::default()), None);
    }

    #[test]
    fn providers_and_checks() {
        assert_eq!(guess_provider("me@gmail.com", ""), Some("google"));
        assert_eq!(guess_provider("me@hotmail.ch", ""), Some("microsoft"));
        assert_eq!(guess_provider("me@firma.ch", "imap.gmail.com"), Some("google"));
        assert_eq!(guess_provider("me@firma.ch", "outlook.office365.com"), Some("microsoft"));
        assert_eq!(guess_provider("me@firma.ch", "mail.firma.ch"), None);
        let mut o = Provider::Google.defaults();
        o.provider = "google".into();
        o.client_id.clear();
        let e = check(&o).unwrap_err();
        assert!(e.contains("[oauth.google]") && e.contains("README") && !e.contains('\n'));
        o.client_id = "x".into();
        assert!(check(&o).is_ok());
        assert!(check(&OAuth { client_id: "x".into(), ..Default::default() }).is_err());
    }
}
