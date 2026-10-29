//! Linux: notify-send, xdg-open, secret-tool, `sh -c`, and a private fontconfig for Apple emoji.

use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};

/// Title bar off: Hyprland tiles the window anyway.
pub const DECORATIONS: bool = false;
pub const FONT: &str = "Inter, Noto Sans, Cantarell, DejaVu Sans, sans-serif";

/// Nothing to do: a unix binary always has its terminal.
pub fn attach_console() {}

/// Environment for the window, set before GTK starts.
pub fn prepare_gui() {
    apple_emoji();
    // WebKit's DMA-BUF renderer shows blank pages on many NVIDIA + Wayland setups
    if Path::new("/proc/driver/nvidia").exists() && std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none() {
        unsafe { std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1") };
    }
}

pub fn app_flags() -> gtk::gio::ApplicationFlags {
    gtk::gio::ApplicationFlags::empty()
}

/// Runs a command line through the shell.
pub fn shell(cmd: &str) -> Command {
    let mut c = Command::new("sh");
    c.args(["-c", cmd]);
    c
}

/// The editor for Ctrl+E on `path`: the configured command, else $TERMINAL -e $EDITOR.
pub fn editor(configured: &str, path: &Path) -> Command {
    let cmd = if configured.is_empty() {
        let term = std::env::var("TERMINAL").unwrap_or_else(|_| "kitty".into());
        let editor = std::env::var("EDITOR").unwrap_or_else(|_| "nvim".into());
        format!("{term} -e {editor}")
    } else {
        configured.to_owned()
    };
    shell(&format!("{cmd} '{}'", path.display().to_string().replace('\'', "'\\''")))
}

pub fn notify(summary: &str, body: &str) {
    let _ = Command::new("notify-send")
        .args(["-a", "brief", "-i", "mail-unread", summary, body])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

pub fn open(target: &str) {
    let _ = Command::new("xdg-open").arg(target).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
}

/// `service` is "brief" for passwords, "brief-oauth" for sign-in tokens.
pub fn keyring_get(service: &str, email: &str) -> Option<String> {
    let out = Command::new("secret-tool").args(["lookup", "service", service, "account", email]).output().ok()?;
    (out.status.success() && !out.stdout.is_empty()).then(|| String::from_utf8_lossy(&out.stdout).trim_end_matches('\n').to_owned())
}

/// Stores a secret in the keyring via secret-tool. Returns false when there is none.
pub fn keyring_store(service: &str, email: &str, pass: &str) -> bool {
    let label = if service == "brief" { format!("brief: {email}") } else { format!("{service}: {email}") };
    let child = Command::new("secret-tool")
        .args(["store", "--label", &label, "service", service, "account", email])
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

/// The `password_cmd` written for an account whose password went to the keyring.
pub fn keyring_cmd(email: &str) -> String {
    format!("secret-tool lookup service brief account {email}")
}

/// Writes a file only the user can read (mode 0600).
pub fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(path)?;
    f.write_all(bytes)
}

/// Finds an installed Apple Color Emoji font (ttf-apple-emoji).
fn apple_emoji_font() -> Option<std::path::PathBuf> {
    let mut dirs = vec![std::path::PathBuf::from("/usr/share/fonts"), std::path::PathBuf::from("/usr/local/share/fonts")];
    if let Some(d) = dirs::data_dir() {
        dirs.push(d.join("fonts"));
    }
    if let Some(h) = dirs::home_dir() {
        dirs.push(h.join(".fonts"));
    }
    fn walk(dir: &std::path::Path, depth: u8) -> Option<std::path::PathBuf> {
        for e in std::fs::read_dir(dir).ok()?.flatten() {
            let p = e.path();
            let name = e.file_name().to_string_lossy().to_lowercase();
            if name.starts_with("applecoloremoji") && (name.ends_with(".ttf") || name.ends_with(".ttc")) {
                return Some(p);
            }
            if depth > 0 && p.is_dir()
                && let Some(f) = walk(&p, depth - 1) {
                    return Some(f);
                }
        }
        None
    }
    dirs.iter().find_map(|d| walk(d, 3))
}

/// With Apple Color Emoji installed, brief runs with a private fontconfig that hides the other
/// color emoji fonts, so fallback (GTK and the HTML view) lands on Apple's. Nothing outside brief
/// changes. Skipped when FONTCONFIG_FILE is already set.
fn apple_emoji() {
    if std::env::var_os("FONTCONFIG_FILE").is_some() || apple_emoji_font().is_none() {
        return;
    }
    let dir = dirs::cache_dir().unwrap_or_else(std::env::temp_dir).join("brief");
    let path = dir.join("fonts.conf");
    let conf = r#"<?xml version="1.0"?>
<!DOCTYPE fontconfig SYSTEM "urn:fontconfig:fonts.dtd">
<!-- written by brief: system fonts, minus the color emoji fonts that are not Apple's -->
<fontconfig>
  <include ignore_missing="yes">/etc/fonts/fonts.conf</include>
  <selectfont>
    <rejectfont>
      <glob>*NotoColorEmoji*</glob>
      <glob>*TwitterColorEmoji*</glob>
      <glob>*Twemoji*</glob>
      <glob>*JoyPixels*</glob>
      <glob>*EmojiOne*</glob>
      <glob>*OpenMoji*</glob>
    </rejectfont>
  </selectfont>
</fontconfig>
"#;
    let _ = std::fs::create_dir_all(&dir);
    if std::fs::read_to_string(&path).ok().as_deref() != Some(conf) && std::fs::write(&path, conf).is_err() {
        return;
    }
    unsafe { std::env::set_var("FONTCONFIG_FILE", &path) };
}
