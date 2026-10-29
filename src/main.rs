//! brief - a keyboard-first mail client for Hyprland.

// no console window behind the app on Windows; the CLI attaches to the parent's instead
#![cfg_attr(windows, windows_subsystem = "windows")]

mod autoconfig;
mod avatar;
mod config;
mod imap;
mod log;
mod mailto;
mod message;
mod oauth;
mod platform;
mod rich;
mod smtp;
mod store;
mod theme;
mod thread;
mod ui;

use gtk::prelude::*;

const HELP: &str = "brief - keyboard-first mail

usage:
  brief            open the window (a second call focuses the running one)
  brief mailto:…   write to a mailto: link, in the running window if there is one
  brief sync       sync every account once without a window and print what changed
  brief login ACC  browser sign-in for an account with auth = \"oauth2\"
  brief themes     list themes
  brief --version

in the app: ? shows every key, Ctrl+K every command";

/// The usage text with the real config and cache paths (they differ on Windows).
fn help() -> String {
    format!("{HELP}\nconfig: {}\ncache:  {}", config::path().display(), store::path().display())
}

fn main() -> glib::ExitCode {
    platform::attach_console();
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("-h" | "--help" | "help") => {
            println!("{}", help());
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
        Some("login") if args.len() == 2 => return login(&args[1]),
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
        Some("render") if args.len() == 2 => {
            platform::prepare_gui();
            return ui::render(&args[1]);
        }
        Some(url) if mailto::is_mailto(url) => {}
        Some(other) if !other.starts_with("--gapplication") => {
            eprintln!("brief: unknown command {other}\n\n{}", help());
            return glib::ExitCode::FAILURE;
        }
        _ => {}
    }
    platform::prepare_gui();
    // single instance: launching again brings the window back, and a mailto: link given to the
    // second call travels to the first one
    let app = gtk::Application::builder()
        .application_id("ch.elyesghazel.brief")
        .flags(platform::app_flags() | gtk::gio::ApplicationFlags::HANDLES_COMMAND_LINE)
        .build();
    app.connect_startup(|_| gtk::Window::set_default_icon_name("ch.elyesghazel.brief"));
    app.connect_activate(|app| ui::activate(app, None));
    app.connect_command_line(|app, cmd| {
        let url = cmd.arguments().iter().skip(1).map(|a| a.to_string_lossy().into_owned()).find(|a| mailto::is_mailto(a));
        ui::activate(app, url.as_deref());
        glib::ExitCode::SUCCESS
    });
    let argv: Vec<String> = std::iter::once("brief".to_owned()).chain(args.into_iter().filter(|a| mailto::is_mailto(a))).collect();
    app.run_with_args(&argv)
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

/// Browser sign-in for an account from the config; stores the refresh token.
fn login(name: &str) -> glib::ExitCode {
    let fail = |e: String| {
        eprintln!("brief: {e}");
        glib::ExitCode::FAILURE
    };
    let cfg = match config::load() {
        Ok(c) => c,
        Err(e) => return fail(e),
    };
    let Some(acc) = cfg.accounts.iter().find(|a| a.name == name || a.email == name) else {
        return fail(format!("no account called {name}"));
    };
    if !acc.is_oauth() {
        return fail(format!("{name} logs in with a password; set auth = \"oauth2\" first"));
    }
    let open = |url: &str| {
        println!("Opening the browser. If nothing happens, open this link:\n\n{url}\n");
        platform::open(url);
    };
    let tokens = match oauth::sign_in(acc, open, &std::sync::atomic::AtomicBool::new(false)) {
        Ok(t) => t,
        Err(e) => return fail(e),
    };
    if let Err(e) = config::store_refresh_token(&acc.email, tokens.refresh.as_deref().unwrap_or_default()) {
        return fail(e);
    }
    print!("signed in as {}, ", acc.email);
    let checks = imap::connect(acc, &tokens.access).map(|mut s| drop(s.logout())).and_then(|()| smtp::test(acc, &tokens.access));
    match checks {
        Ok(()) => {
            println!("IMAP and SMTP work");
            glib::ExitCode::SUCCESS
        }
        Err(e) => {
            println!("but the servers refused: {e}");
            glib::ExitCode::FAILURE
        }
    }
}
