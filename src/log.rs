//! `brief.log` next to the cache: warnings and errors, so they survive a start from a launcher.
//!
//! Every line also goes to stderr. Lines are redacted before they hit the file (passwords, tokens,
//! sign-in codes), and the file rotates at half a megabyte into `brief.log.1`, so the two stay
//! around one megabyte together.

use std::fs;
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::Mutex;

/// Size at which `brief.log` moves to `brief.log.1`.
const ROTATE_AT: u64 = 512 * 1024;

pub fn path() -> PathBuf {
    dirs::cache_dir().unwrap_or_else(std::env::temp_dir).join("brief/brief.log")
}

pub fn warn(msg: &str) {
    write("warn", msg);
}

pub fn error(msg: &str) {
    write("error", msg);
}

fn write(level: &str, msg: &str) {
    if msg.starts_with('[') {
        eprintln!("brief {msg}");
    } else {
        eprintln!("brief: {msg}");
    }
    static LOCK: Mutex<()> = Mutex::new(());
    let _guard = LOCK.lock();
    let path = path();
    if fs::metadata(&path).is_ok_and(|m| m.len() > ROTATE_AT) {
        let _ = fs::rename(&path, path.with_extension("log.1"));
    }
    let _ = fs::create_dir_all(path.parent().unwrap_or(&path));
    let line = format!("{} {:<5} {}\n", chrono::Local::now().format("%Y-%m-%d %H:%M:%S"), level, redact(msg).replace(['\n', '\r'], " | "));
    if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = f.write_all(line.as_bytes());
    }
}

/// Words after which the value is a secret.
const SECRET_KEYS: &[&str] = &[
    "password", "passwd", "pass", "pwd", "secret", "client_secret", "token", "access_token", "refresh_token", "id_token",
    "code", "code_verifier", "authorization", "bearer", "apikey", "api_key",
];

fn token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || "-._~+/=%".contains(c)
}

/// Looks like a credential on its own: Google tokens and client secrets, or a long random run.
fn secret_word(w: &str) -> bool {
    if w.starts_with("ya29.") || w.starts_with("1//") || w.starts_with("GOCSPX-") || w.starts_with("EwB") && w.len() > 40 {
        return true;
    }
    w.len() >= 40
        && !w.contains('/')
        && w.chars().all(token_char)
        && w.chars().any(|c| c.is_ascii_digit())
        && w.chars().any(|c| c.is_ascii_alphabetic())
}

/// The message with credentials replaced by `***`: values after `password=`, `token: `,
/// `"refresh_token":"…"`, `Bearer …` and the like, and anything that looks like a token.
pub fn redact(msg: &str) -> String {
    let lower = msg.to_ascii_lowercase();
    let bytes = msg.as_bytes();
    let mut out = String::with_capacity(msg.len());
    let mut i = 0;
    while i < msg.len() {
        // a secret key at a word start, followed by a separator and a value
        let at_word = i == 0 || !(bytes[i - 1] as char).is_ascii_alphanumeric() && bytes[i - 1] != b'_';
        let key = at_word.then(|| SECRET_KEYS.iter().filter(|k| lower[i..].starts_with(*k)).max_by_key(|k| k.len())).flatten();
        if let Some(k) = key {
            let mut j = i + k.len();
            if j < msg.len() && ((bytes[j] as char).is_ascii_alphanumeric() || bytes[j] == b'_') {
                // only part of a longer word ("passed", "codes")
                out.push_str(&msg[i..j]);
                i = j;
                continue;
            }
            let sep_start = j;
            while j < msg.len() && matches!(bytes[j], b' ' | b'"' | b'\'' | b':' | b'=') {
                j += 1;
            }
            let sep = &msg[sep_start..j];
            let separated = sep.contains([':', '=']) || k.eq_ignore_ascii_case("bearer") && !sep.is_empty();
            let value_end = msg[j..].find(|c: char| c.is_whitespace() || "\"',;&)}]".contains(c)).map_or(msg.len(), |e| j + e);
            if separated && value_end > j {
                out.push_str(&msg[i..j]);
                out.push_str("***");
                i = value_end;
                continue;
            }
            out.push_str(&msg[i..sep_start]);
            i = sep_start;
            continue;
        }
        let c = msg[i..].chars().next().unwrap();
        if token_char(c) && (i == 0 || !token_char(msg[..i].chars().next_back().unwrap())) {
            let end = msg[i..].find(|c: char| !token_char(c)).map_or(msg.len(), |e| i + e);
            let word = &msg[i..end];
            out.push_str(if secret_word(word.trim_end_matches(['.', ','])) { "***" } else { word });
            i = end;
            continue;
        }
        out.push(c);
        i += c.len_utf8();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_are_redacted() {
        assert_eq!(redact("login refused: password=hunter2 for x"), "login refused: password=*** for x");
        assert_eq!(redact(r#"{"access_token":"abc","refresh_token": "def"}"#), r#"{"access_token":"***","refresh_token": "***"}"#);
        assert_eq!(redact("Authorization: Bearer ya29.a0AfB_xyz"), "Authorization: *** ***");
        assert_eq!(redact("GET /cb?code=4/0AQSTgQ&state=x"), "GET /cb?code=***&state=x");
        assert_eq!(redact("client_secret = GOCSPX-abc123"), "client_secret = ***");
        assert_eq!(redact("refresh 1//09abcdefg failed"), "refresh *** failed");
        assert_eq!(redact("got AbCdEf0123456789AbCdEf0123456789AbCdEf0123 back"), "got *** back");
    }

    #[test]
    fn ordinary_text_stays() {
        for s in [
            "[Work] SyncAll: imap.example.com:993: Connection refused (os error 111)",
            "invalid_grant: Token has been expired or revoked.",
            "passed 3 codes, see /home/me/.cache/brief/some/really/long/path/that/goes/on/and/on.log",
            "sign-in refused (access_denied: The user denied the request)",
            "smtp.gmail.com: 535 5.7.8 Username and Password not accepted",
        ] {
            assert_eq!(redact(s), s);
        }
    }
}
