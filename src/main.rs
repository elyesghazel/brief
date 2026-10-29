//! brief - a keyboard-first mail client for Hyprland.

mod avatar;
mod config;
mod imap;
mod message;
mod smtp;
mod store;
mod theme;
mod ui;

use gtk::prelude::*;

const HELP: &str = "brief - keyboard-first mail

usage:
  brief            open the window (a second call focuses the running one)
  brief sync       sync every account once without a window and print what changed
  brief themes     list themes
  brief --version

config: ~/.config/brief/config.toml   cache: ~/.cache/brief/mail.db
in the app: ? shows every key, Ctrl+K every command";

fn main() -> glib::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("-h" | "--help" | "help") => {
            println!("{HELP}");
            return glib::ExitCode::SUCCESS;
        }
        Some("-V" | "--version") => {
            println!("brief {}", env!("CARGO_PKG_VERSION"));
            return glib::ExitCode::SUCCESS;
        }
        Some("themes") => {
            for t in theme::names() {
                println!("{t}");
            }
            return glib::ExitCode::SUCCESS;
        }
        Some("sync") => return headless_sync(),
        Some("cat") if args.len() == 2 => {
            // debug: print a .eml the way the reader shows it
            let raw = std::fs::read(&args[1]).unwrap_or_default();
            match message::parse(&raw) {
                Some(p) => {
                    let names: Vec<String> = p.attachments.iter().map(|a| format!("{} ({})", a.name, a.mime)).collect();
                    println!("{}\nfrom {} <{}>\nattachments {:?}\n\n{}", p.subject, p.from, p.from_addr, names, p.text)
                }
                None => eprintln!("brief: not a message"),
            }
            return glib::ExitCode::SUCCESS;
        }
        Some(other) if !other.starts_with("--gapplication") => {
            eprintln!("brief: unknown command {other}\n\n{HELP}");
            return glib::ExitCode::FAILURE;
        }
        _ => {}
    }
    apple_emoji();
    // WebKit's DMA-BUF renderer shows blank pages on many NVIDIA + Wayland setups
    if std::path::Path::new("/proc/driver/nvidia").exists() && std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none() {
        unsafe { std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1") };
    }
    let app = gtk::Application::builder().application_id("ch.elyesghazel.brief").build();
    app.connect_activate(|app| {
        // single instance: launching again just brings the window back
        if let Some(w) = app.active_window() {
            w.present();
            return;
        }
        ui::build(app);
    });
    app.run_with_args::<&str>(&[])
}

use gtk::glib;

fn headless_sync() -> glib::ExitCode {
    let cfg = match config::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("brief: {e}");
            return glib::ExitCode::FAILURE;
        }
    };
    if cfg.accounts.is_empty() {
        eprintln!("brief: no accounts in {}", config::path().display());
        return glib::ExitCode::FAILURE;
    }
    let (tx, rx) = async_channel::unbounded();
    let n = cfg.accounts.len();
    let _workers: Vec<_> = cfg
        .accounts
        .iter()
        .enumerate()
        .map(|(i, a)| imap::spawn(i, a.clone(), tx.clone(), cfg.cache_limit, 60 * 24))
        .collect();
    let started = std::time::Instant::now();
    let mut done = vec![false; n];
    let mut ok = true;
    while done.iter().any(|d| !d) {
        let Ok(ev) = rx.recv_blocking() else { break };
        match ev {
            imap::Event::Status { acct, busy: false, text } => {
                println!("[{}] {text}", cfg.accounts[acct].name);
                done[acct] = true;
            }
            imap::Event::Error { acct, text } => {
                eprintln!("[{}] error: {text}", cfg.accounts[acct].name);
                done[acct] = true;
                ok = false;
            }
            imap::Event::NewMail { acct, from, subject, count } => {
                println!("[{}] {count} new, latest from {from}: {subject}", cfg.accounts[acct].name)
            }
            _ => {}
        }
    }
    let store = store::Store::open().unwrap();
    for a in &cfg.accounts {
        for f in store.folders(&a.name) {
            println!("  {:<12} {:<28} {:>5} msgs {:>4} unread  [{}]", a.name, imap::decode_utf7(&f.name), f.total, f.unseen, f.role);
        }
    }
    println!("took {:.1}s", started.elapsed().as_secs_f64());
    if ok { glib::ExitCode::SUCCESS } else { glib::ExitCode::FAILURE }
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
