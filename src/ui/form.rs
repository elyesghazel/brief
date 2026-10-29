//! `A`: the account setup. A few steps in one card, keyboard-first:
//!
//! - welcome (first run only): what brief is, a theme picker
//! - address: the email and the name for the From line; brief looks up unknown domains (MX for
//!   Google Workspace / Microsoft 365, autoconfig for the servers)
//! - sign in: browser sign-in for Google (Microsoft too when config.toml has an own
//!   `[oauth.microsoft]` client), a password with a provider hint for the rest; the servers wait
//!   folded under Advanced
//! - check: a live checklist of the sign-in, IMAP and SMTP, failures in plain words
//! - done: label and color, then the account is saved and starts syncing
//!
//! The password goes to the keyring (or secrets.toml, 0600), a browser sign-in's refresh token the
//! same way. The check page alone also signs an existing account in again when its token ran out.

use super::{App, Mode, label};
use crate::autoconfig::{self, Found};
use crate::config::{self, Account, Auth, OAuth, Security, Server};
use crate::imap::Cmd;
use crate::oauth::{self, Provider};
use crate::{imap, smtp, theme};
use gtk::glib;
use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// Themes offered on the welcome page.
const THEMES: [&str; 6] = ["graphite", "eg", "eg-light", "catppuccin-mocha", "rose-pine-dawn", "tokyo-night"];
/// Account colors on the done page; "" follows the theme.
pub(super) const COLORS: [&str; 7] = ["", "#7aa2f7", "#6cc28a", "#d9ad5b", "#ff5f56", "#c4a7e7", "#89dceb"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Page {
    Welcome,
    Address,
    SignIn,
    Check,
    Done,
}

impl Page {
    fn name(self) -> &'static str {
        match self {
            Page::Welcome => "welcome",
            Page::Address => "address",
            Page::SignIn => "signin",
            Page::Check => "check",
            Page::Done => "done",
        }
    }
}

/// A finished browser sign-in.
#[derive(Clone)]
pub(super) struct SignedIn {
    email: String,
    provider: String,
    refresh: String,
    /// The name the provider knows, for the From line.
    name: String,
}

/// What DNS and autoconfig said about a domain.
#[derive(Clone, Default)]
struct Lookup {
    mx: Option<Provider>,
    found: Option<Found>,
}

/// One line of the check page.
struct CheckRow {
    icon: gtk::Label,
    spinner: gtk::Spinner,
    text: gtk::Label,
    detail: gtk::Label,
}

/// What the check page runs.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Item {
    SignIn,
    Imap,
    Smtp,
}

pub struct Form {
    pub root: gtk::Box,
    /// Holds the focus on pages without a field, so keys never reach the window behind.
    sink: gtk::Button,
    pages: gtk::Stack,
    page: Cell<Page>,
    indicator: gtk::Box,
    steps: Vec<gtk::Label>,
    // welcome
    swatches: Vec<gtk::Box>,
    welcome_go: gtk::Box,
    theme_idx: Cell<usize>,
    theme_before: RefCell<String>,
    first_run: Cell<bool>,
    // address
    email: gtk::Entry,
    display: gtk::Entry,
    address_note: gtk::Label,
    // sign in
    service_name: gtk::Label,
    service_addr: gtk::Label,
    pass_row: gtk::Box,
    pass: gtk::PasswordEntry,
    oauth_row: gtk::Box,
    oauth_inner: gtk::Box,
    adv_inner: gtk::Box,
    /// Ctrl+Shift+A on the address page: open the server settings once sign-in shows.
    want_adv: Cell<bool>,
    signin_text: gtk::Label,
    signin_hint: gtk::Label,
    adv_summary: gtk::Label,
    adv: gtk::Revealer,
    user: gtk::Entry,
    imap_host: gtk::Entry,
    imap_port: gtk::Entry,
    imap_sec: gtk::Entry,
    smtp_host: gtk::Entry,
    smtp_port: gtk::Entry,
    smtp_sec: gtk::Entry,
    signin_note: gtk::Label,
    // check
    check_title: gtk::Label,
    check_box: gtk::Box,
    rows: RefCell<Vec<(Item, CheckRow)>>,
    check_error: gtk::Label,
    check_hint: gtk::Label,
    // done
    done_title: gtk::Label,
    done_from: gtk::Label,
    name: gtk::Entry,
    /// Focusable (a Box would hand the focus on to children it does not have).
    color_row: gtk::Button,
    dots: Vec<gtk::Box>,
    color_idx: Cell<usize>,
    // state
    /// The user changed a server field; stop overwriting them from the address.
    touched: Cell<bool>,
    /// Set while brief itself fills the server fields.
    filling: Cell<bool>,
    busy: Cell<bool>,
    /// The checks failed; Enter tries again.
    failed: Cell<bool>,
    /// The last failure was about the servers; going back opens Advanced.
    server_trouble: Cell<bool>,
    /// Bumped when the user leaves the check page, so a late answer is ignored.
    run: Cell<u64>,
    signed: RefCell<Option<SignedIn>>,
    /// Set while the browser sign-in waits; Esc cancels it.
    cancel: RefCell<Option<Arc<AtomicBool>>>,
    /// The sign-in link while the browser is waiting, for Ctrl+L.
    link: RefCell<String>,
    /// Browser sign-in with this provider instead of a password.
    method: Cell<Option<Provider>>,
    /// Ctrl+O picked the method; the domain no longer switches it.
    chosen: Cell<bool>,
    /// Per domain: None while the lookup runs.
    lookups: RefCell<HashMap<String, Option<Lookup>>>,
    lookup_gen: Cell<u64>,
    /// Enter on the address waits for the lookup, then moves on.
    advance: Cell<bool>,
    /// Signing in this existing account again instead of adding one.
    again: RefCell<Option<String>>,
}

fn entry(ph: &str) -> gtk::Entry {
    let e = gtk::Entry::new();
    e.add_css_class("field");
    e.set_placeholder_text(Some(ph));
    e.set_hexpand(true);
    e
}

/// A labelled line, like the rest of brief's forms.
fn row(l: &str, w: &impl IsA<gtk::Widget>) -> gtk::Box {
    let b = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    b.add_css_class("form-row");
    let lab = label(l, "form-label");
    lab.set_width_chars(11);
    b.append(&lab);
    b.append(w);
    b
}

/// Makes `w` clickable, with a pointer cursor.
fn on_click(w: &impl IsA<gtk::Widget>, f: impl Fn() + 'static) {
    let click = gtk::GestureClick::new();
    click.connect_released(move |g, _, _, _| {
        g.set_state(gtk::EventSequenceState::Claimed);
        f();
    });
    w.add_controller(click);
    w.set_cursor_from_name(Some("pointer"));
}

fn chip(text: &str) -> gtk::Label {
    let k = label(text, "form-key");
    k.set_valign(gtk::Align::Center);
    k
}

fn vbox(spacing: i32, class: &str) -> gtk::Box {
    let b = gtk::Box::new(gtk::Orientation::Vertical, spacing);
    if !class.is_empty() {
        b.add_css_class(class);
    }
    b
}

/// A wrapped hint line, hidden while it is empty.
fn note() -> gtk::Label {
    let l = label("", "form-hint");
    l.set_wrap(true);
    l.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    l.set_max_width_chars(60);
    l.set_visible(false);
    l.connect_label_notify(|l| l.set_visible(!l.label().is_empty()));
    l
}

impl Form {
    pub fn new() -> Form {
        let root = vbox(0, "dialog");
        root.add_css_class("setup");
        root.set_halign(gtk::Align::Center);
        root.set_valign(gtk::Align::Center);
        root.set_vexpand(true);
        root.set_size_request(600, -1);

        // head: logo, wordmark, where we are
        let head = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        head.add_css_class("setup-head");
        head.append(&super::widgets::logo(18));
        head.append(&label("brief", "brand"));
        let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        spacer.set_hexpand(true);
        head.append(&spacer);
        let indicator = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        let mut steps = Vec::new();
        for (i, s) in ["address", "sign in", "check", "done"].iter().enumerate() {
            if i > 0 {
                indicator.append(&label("·", "step step-sep"));
            }
            let l = label(s, "step");
            indicator.append(&l);
            steps.push(l);
        }
        head.append(&indicator);
        let sink = gtk::Button::new();
        sink.add_css_class("focus-sink");
        sink.set_can_target(false);
        root.append(&head);

        let pages = gtk::Stack::new();
        pages.set_transition_type(gtk::StackTransitionType::Crossfade);
        pages.set_transition_duration(120);
        pages.set_vhomogeneous(false);
        pages.set_interpolate_size(true);
        root.append(&pages);

        // welcome
        let welcome = vbox(0, "setup-page");
        welcome.add_css_class("welcome");
        let big = super::widgets::logo(56);
        big.set_halign(gtk::Align::Start);
        welcome.append(&big);
        welcome.append(&label("Mail, from the keyboard.", "setup-title"));
        let tag = label(
            "brief reads and writes mail over IMAP and SMTP: Gmail, Outlook, iCloud, your own server. j k to move, Enter to read, c to write.",
            "setup-sub",
        );
        tag.set_wrap(true);
        tag.set_max_width_chars(58);
        welcome.append(&tag);
        welcome.append(&label("theme", "setup-kicker"));
        let sw_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        sw_row.add_css_class("swatches");
        sw_row.set_homogeneous(true);
        let mut swatches = Vec::new();
        for (i, name) in THEMES.iter().enumerate() {
            let cell = vbox(6, "swatch-cell");
            let sw = gtk::Box::new(gtk::Orientation::Vertical, 4);
            sw.add_css_class("swatch");
            sw.add_css_class(&format!("swatch-{i}"));
            sw.append(&label("Aa", "swatch-fg"));
            let bar = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            bar.add_css_class("swatch-accent");
            bar.set_halign(gtk::Align::Start);
            sw.append(&bar);
            cell.append(&sw);
            cell.append(super::ellipsize(&label(name, "swatch-name")));
            sw_row.append(&cell);
            swatches.push(cell);
        }
        welcome.append(&sw_row);
        let go = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        go.add_css_class("setup-go");
        go.append(&chip("Enter"));
        go.append(&label("add your first account", "setup-go-text"));
        welcome.append(&go);
        pages.add_named(&welcome, Some("welcome"));

        // address
        let address = vbox(0, "setup-page");
        address.append(&label("Add an account", "setup-title"));
        address.append(&label("Your address is enough, brief finds the servers.", "setup-sub"));
        let email = entry("you@example.com");
        email.add_css_class("big-field");
        address.append(&email);
        let display = entry("optional, shown in the From line");
        let grid = vbox(0, "form");
        grid.append(&row("Your name", &display));
        address.append(&grid);
        let address_note = note();
        address.append(&address_note);
        pages.add_named(&address, Some("address"));

        // sign in
        let signin = vbox(0, "setup-page");
        let service_name = label("", "setup-title");
        let service_addr = label("", "setup-sub");
        signin.append(&service_name);
        signin.append(&service_addr);
        let grid = vbox(0, "form");
        let pass = gtk::PasswordEntry::new();
        pass.add_css_class("field");
        pass.set_show_peek_icon(true);
        pass.set_hexpand(true);
        let pass_row = row("Password", &pass);
        grid.append(&pass_row);
        let signin_text = label("", "signin-text");
        signin_text.set_hexpand(true);
        let oauth_inner = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        oauth_inner.append(&signin_text);
        oauth_inner.append(&chip("Enter"));
        oauth_inner.set_hexpand(true);
        let oauth_row = row("Sign-in", &oauth_inner);
        grid.append(&oauth_row);
        let adv_summary = label("", "servers-text");
        adv_summary.set_hexpand(true);
        super::ellipsize(&adv_summary);
        let adv_inner = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        adv_inner.append(&adv_summary);
        adv_inner.append(&chip("Ctrl+Shift+A"));
        adv_inner.set_hexpand(true);
        grid.append(&row("Advanced", &adv_inner));
        let user = entry("login, if not the address");
        let imap_host = entry("imap.example.com");
        let imap_port = entry("993");
        let imap_sec = entry("tls | starttls | none");
        let smtp_host = entry("smtp.example.com");
        let smtp_port = entry("587");
        let smtp_sec = entry("tls | starttls | none");
        let adv_rows = vbox(0, "");
        for (l, e) in [
            ("Username", &user),
            ("IMAP host", &imap_host),
            ("IMAP port", &imap_port),
            ("IMAP sec", &imap_sec),
            ("SMTP host", &smtp_host),
            ("SMTP port", &smtp_port),
            ("SMTP sec", &smtp_sec),
        ] {
            adv_rows.append(&row(l, e));
        }
        let adv = gtk::Revealer::new();
        adv.set_transition_type(gtk::RevealerTransitionType::SlideDown);
        adv.set_transition_duration(120);
        adv.set_child(Some(&adv_rows));
        grid.append(&adv);
        signin.append(&grid);
        let signin_hint = note();
        signin_hint.set_use_markup(true);
        signin.append(&signin_hint);
        let signin_note = note();
        signin.append(&signin_note);
        pages.add_named(&signin, Some("signin"));

        // check
        let check = vbox(0, "setup-page");
        let check_title = label("Checking", "setup-title");
        check.append(&check_title);
        let check_box = vbox(0, "checklist");
        check.append(&check_box);
        let check_error = label("", "check-error");
        check_error.set_wrap(true);
        check_error.set_max_width_chars(60);
        check_error.set_visible(false);
        check_error.connect_label_notify(|l| l.set_visible(!l.label().is_empty()));
        let check_hint = note();
        check.append(&check_error);
        check.append(&check_hint);
        pages.add_named(&check, Some("check"));

        // done
        let done = vbox(0, "setup-page");
        let done_title = label("", "setup-title");
        let done_from = label("", "setup-sub");
        super::ellipsize(&done_from);
        done.append(&done_title);
        done.append(&done_from);
        let grid = vbox(0, "form");
        let name = entry("short label, e.g. Work");
        grid.append(&row("Label", &name));
        let dot_box = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        let color_row = gtk::Button::new();
        color_row.add_css_class("dots");
        color_row.set_child(Some(&dot_box));
        let mut dots = Vec::new();
        for i in 0..COLORS.len() {
            let d = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            d.add_css_class("color-dot");
            d.add_css_class(&format!("dot-c{i}"));
            d.set_valign(gtk::Align::Center);
            dot_box.append(&d);
            dots.push(d);
        }
        grid.append(&row("Color", &color_row));
        done.append(&grid);
        let done_note = note();
        done_note.set_text("Enter saves it and starts syncing. Ctrl+N saves it and adds another one.");
        done.append(&done_note);
        pages.add_named(&done, Some("done"));
        root.append(&sink);

        Form {
            root,
            sink,
            pages,
            page: Cell::new(Page::Address),
            indicator,
            steps,
            swatches,
            welcome_go: go,
            theme_idx: Cell::new(0),
            theme_before: RefCell::new(String::new()),
            first_run: Cell::new(false),
            email,
            display,
            address_note,
            service_name,
            service_addr,
            pass_row,
            pass,
            oauth_row,
            oauth_inner,
            adv_inner,
            want_adv: Cell::new(false),
            signin_text,
            signin_hint,
            adv_summary,
            adv,
            user,
            imap_host,
            imap_port,
            imap_sec,
            smtp_host,
            smtp_port,
            smtp_sec,
            signin_note,
            check_title,
            check_box,
            rows: RefCell::new(Vec::new()),
            check_error,
            check_hint,
            done_title,
            done_from,
            name,
            color_row,
            dots,
            color_idx: Cell::new(0),
            touched: Cell::new(false),
            filling: Cell::new(false),
            busy: Cell::new(false),
            failed: Cell::new(false),
            server_trouble: Cell::new(false),
            run: Cell::new(0),
            signed: RefCell::new(None),
            cancel: RefCell::new(None),
            link: RefCell::new(String::new()),
            method: Cell::new(None),
            chosen: Cell::new(false),
            lookups: RefCell::new(HashMap::new()),
            lookup_gen: Cell::new(0),
            advance: Cell::new(false),
            again: RefCell::new(None),
        }
    }

    pub fn wire(&self, app: &Rc<App>) {
        // theme swatches paint themselves in their own theme
        let mut css = String::new();
        for (i, name) in THEMES.iter().enumerate() {
            if let Some([bg, fg, accent]) = theme::swatch(name) {
                css += &format!(
                    ".swatch-{i} {{ background: {bg}; }} .swatch-{i} .swatch-fg {{ color: {fg}; }} .swatch-{i} .swatch-accent {{ background: {accent}; }}\n"
                );
            }
        }
        let provider = gtk::CssProvider::new();
        provider.load_from_string(&css);
        gtk::style_context_add_provider_for_display(&self.root.display(), &provider, gtk::STYLE_PROVIDER_PRIORITY_USER + 3);

        for (i, cell) in self.swatches.iter().enumerate() {
            let weak = Rc::downgrade(app);
            on_click(cell, move || {
                if let Some(app) = weak.upgrade() {
                    app.form_pick_theme(i);
                }
            });
        }
        for w in [&self.welcome_go, &self.oauth_inner] {
            let weak = Rc::downgrade(app);
            on_click(w, move || {
                if let Some(app) = weak.upgrade() {
                    app.form_key("Return");
                }
            });
        }
        let weak = Rc::downgrade(app);
        on_click(&self.adv_inner, move || {
            if let Some(app) = weak.upgrade() {
                app.form_key("C-S-a");
            }
        });

        let weak = Rc::downgrade(app);
        self.email.connect_changed(move |_| {
            if let Some(app) = weak.upgrade() {
                app.form_email_changed();
            }
        });
        let n2 = self.name.clone();
        self.name.connect_changed(move |e| {
            if e.has_focus() {
                n2.set_widget_name("");
            }
        });
        for e in self.server_entries() {
            let weak = Rc::downgrade(app);
            e.connect_changed(move |_| {
                let Some(app) = weak.upgrade() else { return };
                if !app.form.filling.get() {
                    app.form.touched.set(true);
                }
                app.form.show_servers();
            });
        }
    }

    fn server_entries(&self) -> [&gtk::Entry; 7] {
        [&self.user, &self.imap_host, &self.imap_port, &self.imap_sec, &self.smtp_host, &self.smtp_port, &self.smtp_sec]
    }

    pub fn open(&self, app: &Rc<App>) {
        self.reset();
        for e in [&self.email, &self.display, &self.name] {
            e.set_text("");
        }
        let first = app.accounts.borrow().is_empty();
        self.first_run.set(first);
        let cur = app.theme_name.borrow().clone();
        self.theme_before.replace(cur.clone());
        self.theme_idx.set(THEMES.iter().position(|t| *t == cur).unwrap_or(0));
        self.show_swatches();
        app.show_overlay(self.root.upcast_ref());
        if first {
            // a page of its own, with the status bar's key hints still showing
            app.overlay_scrim.add_css_class("solid");
            app.overlay_scrim.set_margin_bottom(app.hints.parent().map_or(0, |p| p.measure(gtk::Orientation::Vertical, -1).1));
        }
        app.set_mode(Mode::Form);
        app.form_page(if first { Page::Welcome } else { Page::Address });
    }

    /// Back to an empty setup.
    fn reset(&self) {
        self.again.replace(None);
        self.pass.set_text("");
        self.filling.set(true);
        for e in self.server_entries() {
            e.set_text("");
        }
        self.filling.set(false);
        self.touched.set(false);
        self.signed.replace(None);
        self.method.set(None);
        self.chosen.set(false);
        self.busy.set(false);
        self.failed.set(false);
        self.server_trouble.set(false);
        self.adv.set_reveal_child(false);
        self.want_adv.set(false);
        self.link.replace(String::new());
        self.color_idx.set(0);
        self.show_dots();
        self.advance.set(false);
        self.address_note.set_text("");
        self.signin_note.set_text("");
    }

    fn account(&self) -> Result<Account, String> {
        let sec = |e: &gtk::Entry| match e.text().trim().to_lowercase().as_str() {
            "" | "tls" | "ssl" => Ok(Security::Tls),
            "starttls" => Ok(Security::Starttls),
            "none" | "plain" => Ok(Security::None),
            other => Err(format!("security must be tls, starttls or none, not {other}")),
        };
        let port = |e: &gtk::Entry, what: &str| e.text().trim().parse::<u16>().map_err(|_| format!("the {what} port is not a number"));
        let email = self.email.text().trim().to_owned();
        if !email.contains('@') {
            return Err("enter an email address".into());
        }
        let name = match self.name.text().trim() {
            "" => email.clone(),
            n => n.to_owned(),
        };
        let mut acc = Account {
            name,
            email,
            display_name: self.display.text().trim().to_owned(),
            user: self.user.text().trim().to_owned(),
            imap: Server { host: self.imap_host.text().trim().to_owned(), port: port(&self.imap_port, "IMAP")?, security: sec(&self.imap_sec)? },
            smtp: Server { host: self.smtp_host.text().trim().to_owned(), port: port(&self.smtp_port, "SMTP")?, security: sec(&self.smtp_sec)? },
            password_cmd: String::new(),
            password: String::new(),
            auth: Auth::Password,
            oauth: OAuth::default(),
            signature: String::new(),
            color: COLORS[self.color_idx.get()].to_owned(),
        };
        // only the provider goes into config.toml, the client comes from [oauth.<provider>] or
        // the built-in app
        if let Some(p) = self.method.get() {
            acc.auth = Auth::Oauth2;
            acc.oauth = OAuth { provider: p.name().into(), ..OAuth::default() };
        }
        Ok(acc)
    }

    fn waiting(&self) -> bool {
        self.cancel.borrow().is_some()
    }

    fn domain(&self) -> String {
        self.email.text().trim().rsplit_once('@').map(|(_, d)| d.to_lowercase()).unwrap_or_default()
    }

    fn lookup(&self) -> Option<Lookup> {
        self.lookups.borrow().get(&self.domain()).cloned().flatten()
    }

    /// The servers for the address and the sign-in method, unless the user typed their own.
    fn fill_servers(&self) {
        if self.touched.get() {
            return;
        }
        let text = self.email.text();
        if !text.contains('@') {
            return;
        }
        let lookup = self.lookup().unwrap_or_default();
        // Workspace and Microsoft 365 domains use the provider's servers, password or not
        let hosted = self.method.get().or(lookup.mx).filter(|_| oauth::provider_for_address(&text).is_none());
        let (i, s, user) = match (hosted, lookup.found) {
            (Some(p), _) => {
                let (i, s) = p.servers();
                (i, s, String::new())
            }
            (None, Some(f)) => (f.imap, f.smtp, f.user),
            _ => {
                let (i, s) = config::guess_servers(&text);
                (i, s, String::new())
            }
        };
        self.filling.set(true);
        self.user.set_text(&user);
        self.imap_host.set_text(&i.host);
        self.imap_port.set_text(&i.port.to_string());
        self.imap_sec.set_text(i.security.as_str());
        self.smtp_host.set_text(&s.host);
        self.smtp_port.set_text(&s.port.to_string());
        self.smtp_sec.set_text(s.security.as_str());
        self.filling.set(false);
        self.show_servers();
    }

    /// The folded Advanced row's summary.
    fn show_servers(&self) {
        let (i, s) = (self.imap_host.text(), self.smtp_host.text());
        let text = if i == s { i.to_string() } else { format!("{i} · {s}") };
        self.adv_summary.set_text(&text);
    }

    /// The sign-in page for the current method and address.
    fn show_method(&self) {
        let p = self.method.get();
        let email = self.email.text().trim().to_owned();
        let svc = service(&email, p, self.lookup().and_then(|l| l.mx));
        self.service_name.set_text(&svc.name);
        self.service_addr.set_text(&email);
        self.pass_row.set_visible(p.is_none());
        self.oauth_row.set_visible(p.is_some());
        let hint = match p {
            Some(p) => {
                self.signin_text.set_text(&format!("Sign in with {}", p.label()));
                format!("{} signs you in, in your browser. brief never sees your password and keeps only a token.", p.label())
            }
            None if !svc.app_password.is_empty() => format!(
                "{} <a href=\"{}\">Create one</a>, or Ctrl+L.",
                glib::markup_escape_text(svc.hint),
                glib::markup_escape_text(svc.app_password)
            ),
            None => glib::markup_escape_text(svc.hint).to_string(),
        };
        self.signin_hint.set_markup(&hint);
    }

    fn show_swatches(&self) {
        for (i, s) in self.swatches.iter().enumerate() {
            if i == self.theme_idx.get() {
                s.add_css_class("current");
            } else {
                s.remove_css_class("current");
            }
        }
    }

    fn show_dots(&self) {
        for (i, d) in self.dots.iter().enumerate() {
            if i == self.color_idx.get() {
                d.add_css_class("current");
            } else {
                d.remove_css_class("current");
            }
        }
    }

    fn show_indicator(&self) {
        let at = match self.page.get() {
            Page::Welcome => None,
            Page::Address => Some(0),
            Page::SignIn => Some(1),
            Page::Check => Some(2),
            Page::Done => Some(3),
        };
        self.indicator.set_visible(at.is_some() && self.again.borrow().is_none());
        for (i, l) in self.steps.iter().enumerate() {
            let state = if Some(i) == at {
                "now"
            } else if at.is_some_and(|a| i < a) {
                "past"
            } else {
                "next"
            };
            l.set_css_classes(&["step", state]);
        }
    }

    /// Keyboard hints for the status bar.
    pub fn hints(&self) -> &'static [(&'static str, &'static str)] {
        let has_link = self.method.get().is_none() && !service(&self.email.text(), None, None).app_password.is_empty();
        match self.page.get() {
            Page::Welcome => &[("← →", "theme"), ("Enter", "add your first account"), ("Esc", "skip")],
            Page::Address if self.first_run.get() => &[("Enter", "continue"), ("Tab", "your name"), ("Ctrl+Shift+A", "servers"), ("Esc", "back")],
            Page::Address => &[("Enter", "continue"), ("Tab", "your name"), ("Ctrl+Shift+A", "servers"), ("Esc", "cancel")],
            Page::SignIn if self.again.borrow().is_some() => &[("Enter", "sign in"), ("Esc", "cancel")],
            Page::SignIn if self.method.get().is_some() => {
                &[("Enter", "sign in"), ("Ctrl+O", "use a password"), ("Ctrl+Shift+A", "advanced"), ("Esc", "back")]
            }
            Page::SignIn if has_link => &[("Enter", "check"), ("Ctrl+L", "app password page"), ("Ctrl+Shift+A", "advanced"), ("Esc", "back")],
            Page::SignIn => &[("Enter", "check"), ("Ctrl+Shift+A", "advanced"), ("Esc", "back")],
            Page::Check if self.waiting() => &[("Esc", "cancel"), ("Ctrl+L", "copy the link")],
            Page::Check if self.failed.get() => &[("Enter", "try again"), ("Esc", "back")],
            Page::Check => &[("Esc", "back")],
            Page::Done => &[("Enter", "finish"), ("Ctrl+N", "finish, add another"), ("Tab", "color"), ("Esc", "back")],
        }
    }

    /// The check page's rows, all pending.
    fn set_items(&self, items: &[(Item, String)]) {
        while let Some(c) = self.check_box.first_child() {
            self.check_box.remove(&c);
        }
        let mut rows = Vec::new();
        for (item, text) in items {
            let b = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            b.add_css_class("check-row");
            let icon = label("○", "check-icon");
            let spinner = gtk::Spinner::new();
            spinner.add_css_class("check-spin");
            spinner.set_visible(false);
            let t = label(text, "check-text");
            let detail = label("", "check-detail");
            detail.set_hexpand(true);
            detail.set_xalign(1.0);
            super::ellipsize(&detail);
            b.append(&icon);
            b.append(&spinner);
            b.append(&t);
            b.append(&detail);
            self.check_box.append(&b);
            rows.push((*item, CheckRow { icon, spinner, text: t, detail }));
        }
        self.rows.replace(rows);
        self.check_error.set_text("");
        self.check_hint.set_text("");
    }

    /// One checklist line: "wait", "run", "ok" or "fail".
    fn mark(&self, item: Item, state: &str, detail: &str) {
        let rows = self.rows.borrow();
        let Some((_, r)) = rows.iter().find(|(i, _)| *i == item) else { return };
        let running = state == "run";
        r.spinner.set_visible(running);
        r.spinner.set_spinning(running);
        r.icon.set_visible(!running);
        r.icon.set_text(match state {
            "ok" => "✓",
            "fail" => "✗",
            _ => "○",
        });
        r.icon.set_css_classes(&["check-icon", state]);
        r.text.set_css_classes(&["check-text", state]);
        r.detail.set_text(detail);
    }
}

/// Who runs an address's mail, as the sign-in page names it.
pub(super) struct Service {
    name: String,
    hint: &'static str,
    /// Where to create an app password, when the service wants one.
    app_password: &'static str,
}

pub(super) fn service(email: &str, method: Option<Provider>, mx: Option<Provider>) -> Service {
    let domain = email.trim().rsplit_once('@').map(|(_, d)| d.to_lowercase()).unwrap_or_default();
    let s = |name: &str, hint, app_password| Service { name: name.into(), hint, app_password };
    match (oauth::provider_for_address(email), domain.as_str()) {
        (Some(Provider::Google), _) => s(
            "Gmail",
            "Gmail wants an app password here (2-Step Verification has to be on). Ctrl+O signs in with Google instead.",
            "https://myaccount.google.com/apppasswords",
        ),
        (Some(Provider::Microsoft), _) => s(
            "Outlook",
            "Outlook wants an app password (two-step verification has to be on).",
            "https://account.live.com/proofs/AppPassword",
        ),
        (None, "icloud.com" | "me.com" | "mac.com") => {
            s("iCloud", "iCloud wants an app-specific password from your Apple Account.", "https://account.apple.com/account/manage")
        }
        (None, "yahoo.com" | "ymail.com") => s("Yahoo", "Yahoo wants an app password.", "https://login.yahoo.com/account/security/app-passwords"),
        (None, "fastmail.com" | "fastmail.fm") => s(
            "Fastmail",
            "Fastmail wants an app password with IMAP and SMTP access.",
            "https://app.fastmail.com/settings/security/integrations",
        ),
        (None, d) if d.starts_with("gmx.") => s("GMX", "Your GMX password. Turn on IMAP first: GMX settings, E-Mail, POP3 & IMAP.", ""),
        (None, "bluewin.ch") => s("Bluewin", "Your Bluewin password, as it is.", ""),
        (None, "proton.me" | "protonmail.com" | "pm.me") => s(
            "Proton Mail Bridge",
            "The password Proton Mail Bridge shows for this address, not your Proton password. Bridge has to run.",
            "",
        ),
        (None, "zoho.com" | "zohomail.com") => {
            s("Zoho Mail", "Zoho wants an app-specific password when two-factor is on.", "https://accounts.zoho.com/home#security/app_password")
        }
        _ => {
            let (name, hint) = match method.or(mx) {
                Some(Provider::Google) => ("Google Workspace".into(), "Ctrl+O signs in with Google, or use an app password."),
                Some(Provider::Microsoft) => {
                    ("Microsoft 365".into(), "Your Microsoft 365 password. Your admin has to allow IMAP and SMTP AUTH for the mailbox.")
                }
                None => (domain.clone(), "Your mail password. If the servers are wrong, fix them under Advanced (Ctrl+Shift+A)."),
            };
            Service { name, hint, app_password: "" }
        }
    }
}

/// A failed check in plain words and what to try next; the flag says the servers are the likely
/// cause.
pub(super) fn explain(item: Item, err: &str, server: &Server, oauth: Option<Provider>, svc: &Service) -> (String, String, bool) {
    let e = err.to_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| e.contains(w));
    let at = format!("{}:{}", server.host, server.port);
    let who = oauth.map_or("The provider", Provider::label);
    if item == Item::SignIn {
        return if has(&["cancelled"]) {
            ("Sign-in cancelled.".into(), "Enter opens the browser again, Esc goes back.".into(), false)
        } else if has(&["timed out"]) {
            ("The browser sign-in took too long.".into(), "Enter opens the browser again.".into(), false)
        } else if has(&["access_denied", "refused"]) {
            (format!("{who} did not let brief in."), "Enter tries again; approve brief on the page that opens.".into(), false)
        } else if has(&["signed in as"]) {
            (format!("{}{}.", err[..1].to_uppercase(), &err[1..]), "Pick the right account in the browser, Enter tries again.".into(), false)
        } else {
            (err.into(), "Enter tries again.".into(), false)
        };
    }
    if has(&["lookup", "not known", "no address", "name resolution", "nodename"]) {
        (format!("{} does not exist.", server.host), "Check the server name under Advanced.".into(), true)
    } else if has(&["connection refused", "timed out", "unreachable", "reset by peer", "no route"]) {
        (format!("Could not reach {at}."), "Check host and port under Advanced; some networks block mail ports.".into(), true)
    } else if has(&["certificate", "handshake", "tls", "ssl", "corrupt message", "invalidcontenttype"]) {
        (
            format!("The secure connection to {at} failed."),
            "Under Advanced try 993 with tls or 143 with starttls for IMAP, 465 with tls or 587 with starttls for SMTP.".into(),
            true,
        )
    } else if has(&["login refused", "authenticat", "credentials", "password", "535", "534", "invalid login"]) {
        let what = if item == Item::Imap { "IMAP" } else { "SMTP" };
        let next = if oauth.is_some() {
            "Esc goes back, Enter there signs in again.".to_owned()
        } else if !svc.app_password.is_empty() {
            format!("{} wants an app password, not the normal one: Esc, then Ctrl+L opens the page.", svc.name)
        } else {
            "Check the password, and the username under Advanced.".to_owned()
        };
        (format!("{what} did not accept the login."), next, false)
    } else {
        (err.into(), "Check the settings under Advanced, then Enter tries again.".into(), true)
    }
}

/// Browser sign-in shows up for Google always (brief ships its app), for Microsoft only when
/// config.toml brings an own `[oauth.microsoft]` client id.
fn sign_in_offered(p: Provider) -> bool {
    p == Provider::Google || config::load().ok().and_then(|c| c.oauth.get(p.name()).map(|o| !o.client_id.is_empty())).unwrap_or(false)
}

/// The domain to look up: a complete-looking address that no built-in rule knows.
fn lookup_domain(email: &str) -> Option<String> {
    let (local, domain) = email.trim().rsplit_once('@')?;
    let domain = domain.to_lowercase();
    let tld = domain.rsplit_once('.')?.1;
    let known = oauth::provider_for_address(email).is_some() || !config::guess_servers(email).0.host.starts_with("mail.");
    (!local.is_empty() && tld.len() >= 2 && tld.chars().all(|c| c.is_ascii_alphabetic()) && !known).then_some(domain)
}

impl App {
    fn form_page(self: &Rc<Self>, page: Page) {
        let f = &self.form;
        f.page.set(page);
        f.pages.set_visible_child_name(page.name());
        f.show_indicator();
        match page {
            Page::Welcome => self.unfocus(),
            Page::Address => {
                f.email.grab_focus();
                f.email.set_position(-1);
            }
            Page::SignIn if f.want_adv.take() => {
                f.show_method();
                self.toggle_advanced(true);
            }
            Page::SignIn => {
                f.show_method();
                if f.method.get().is_some() {
                    // nothing to type here; keep keys away from the other pages' fields
                    self.unfocus();
                } else {
                    f.pass.grab_focus();
                }
            }
            Page::Check => self.unfocus(),
            Page::Done => {
                f.name.grab_focus();
                f.name.set_position(-1);
            }
        }
        self.update_hints();
    }

    /// Parks the focus in the card on pages without a field.
    fn unfocus(&self) {
        self.form.sink.grab_focus();
    }

    fn form_close(self: &Rc<Self>) {
        self.form.run.set(self.form.run.get() + 1);
        self.form.reset();
        self.overlay_scrim.remove_css_class("solid");
        self.overlay_scrim.set_margin_bottom(0);
        self.hide_overlay();
    }

    fn form_email_changed(self: &Rc<Self>) {
        let f = &self.form;
        let text = f.email.text().trim().to_owned();
        if f.signed.borrow().as_ref().is_some_and(|s| s.email != text) {
            f.signed.replace(None);
        }
        if f.again.borrow().is_none() && text.contains('@') && (f.name.text().is_empty() || f.name.widget_name() == "auto") {
            let domain = f.domain();
            let first = domain.split('.').next().unwrap_or("").to_owned();
            let mut c = first.chars();
            let cap = c.next().map(|x| x.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default();
            f.name.set_text(&cap);
            f.name.set_widget_name("auto");
        }
        if f.address_note.has_css_class("form-error") {
            f.address_note.set_text("");
            f.address_note.set_css_classes(&["form-hint"]);
        }
        self.start_lookup(true);
    }

    /// Asks DNS and autoconfig about the domain; with `debounce` only after a pause in typing.
    fn start_lookup(self: &Rc<Self>, debounce: bool) {
        let f = &self.form;
        let g = f.lookup_gen.get() + 1;
        f.lookup_gen.set(g);
        let email = f.email.text().trim().to_owned();
        let Some(domain) = lookup_domain(&email) else { return };
        if f.lookups.borrow().contains_key(&domain) {
            return;
        }
        let me = Rc::downgrade(self);
        let go = move || {
            let Some(app) = me.upgrade() else { return };
            if (debounce && app.form.lookup_gen.get() != g) || app.form.lookups.borrow().contains_key(&domain) {
                return;
            }
            app.form.lookups.borrow_mut().insert(domain.clone(), None);
            let (tx, rx) = async_channel::bounded(1);
            let (d, e) = (domain.clone(), email.clone());
            std::thread::spawn(move || {
                let timeout = Duration::from_secs(3);
                let mx = std::thread::spawn(move || oauth::lookup_mx(&d, timeout));
                let found = autoconfig::lookup(&e, timeout);
                let hosts = mx.join().ok().flatten().unwrap_or_default();
                let _ = tx.send_blocking(Lookup { mx: oauth::provider_for_mx(hosts.iter().map(String::as_str)), found });
            });
            glib::spawn_future_local(async move {
                let Ok(found) = rx.recv().await else { return };
                app.form.lookups.borrow_mut().insert(domain.clone(), Some(found));
                if app.form.advance.get() && app.form.domain() == domain && app.form.page.get() == Page::Address {
                    app.form.advance.set(false);
                    app.address_next();
                }
            });
        };
        if debounce {
            glib::timeout_add_local_once(Duration::from_millis(500), go);
        } else {
            go();
        }
    }

    /// Enter on the address: waits for a running lookup, then picks the method and servers.
    /// Previews theme `i` on the welcome page.
    fn form_pick_theme(self: &Rc<Self>, i: usize) {
        let f = &self.form;
        if f.page.get() != Page::Welcome {
            return;
        }
        f.theme_idx.set(i);
        f.show_swatches();
        self.apply_theme(THEMES[i]);
    }

    /// Shows or hides the server settings on the sign-in page.
    fn toggle_advanced(self: &Rc<Self>, open: bool) {
        let f = &self.form;
        f.adv.set_reveal_child(open);
        if open {
            f.imap_host.grab_focus();
        } else if f.method.get().is_some() {
            self.unfocus();
        } else {
            f.pass.grab_focus();
        }
    }

    fn address_next(self: &Rc<Self>) {
        let f = &self.form;
        let email = f.email.text().trim().to_owned();
        let ok = email.rsplit_once('@').is_some_and(|(l, d)| !l.is_empty() && d.contains('.') && !d.ends_with('.'));
        if !ok {
            f.address_note.set_text("That does not look like an email address yet.");
            f.address_note.set_css_classes(&["form-error"]);
            return;
        }
        if let Some(domain) = lookup_domain(&email) {
            let state = f.lookups.borrow().get(&domain).map(Option::is_some);
            if state != Some(true) {
                f.advance.set(true);
                f.address_note.set_text(&format!("Looking up {domain}…"));
                f.address_note.set_css_classes(&["form-hint"]);
                if state.is_none() {
                    self.start_lookup(false);
                }
                return;
            }
        }
        if !f.chosen.get() {
            let mx = f.lookup().and_then(|l| l.mx).filter(|_| f.pass.text().is_empty());
            f.method.set(oauth::provider_for_address(&email).or(mx).filter(|p| sign_in_offered(*p)));
        }
        f.fill_servers();
        f.address_note.set_text("");
        self.form_page(Page::SignIn);
    }

    pub fn form_key(self: &Rc<Self>, k: &str) -> bool {
        let f = &self.form;
        let page = f.page.get();
        match (page, k) {
            (_, "Escape") => self.form_back(),
            (_, "S-Tab") if self.form_at_start() => self.form_back(),
            (Page::Welcome, "Left" | "h" | "Right" | "l") => {
                let n = THEMES.len();
                let step = if matches!(k, "Left" | "h") { n - 1 } else { 1 };
                self.form_pick_theme((f.theme_idx.get() + step) % n);
            }
            (Page::Welcome, "Return") => {
                let name = THEMES[f.theme_idx.get()];
                if *f.theme_before.borrow() != name {
                    config::save_theme(name);
                    f.theme_before.replace(name.to_owned());
                }
                self.form_page(Page::Address);
            }
            (Page::Welcome, _) => {}
            (Page::Address, "Return") => self.address_next(),
            (Page::Address, "C-S-a") => {
                f.want_adv.set(true);
                self.address_next();
            }
            (Page::SignIn, "Return" | "C-Return") => self.form_run(),
            (Page::SignIn, "C-o") => self.switch_method(),
            (Page::SignIn, "C-S-a") => self.toggle_advanced(!f.adv.reveals_child()),
            (Page::SignIn, "C-l") => {
                let svc = service(&f.email.text(), f.method.get(), None);
                if f.method.get().is_none() && !svc.app_password.is_empty() {
                    super::open_uri(svc.app_password);
                }
            }
            (Page::Check, "C-l") if f.waiting() => {
                self.win.clipboard().set_text(&f.link.borrow());
                self.toast("sign-in link copied", false);
            }
            (Page::Check, "Return") if f.failed.get() && !f.busy.get() => self.form_run(),
            (Page::Check, _) => {}
            (Page::Done, "Return" | "C-Return") => self.form_finish(false),
            (Page::Done, "C-n") => self.form_finish(true),
            (Page::Done, "Left" | "h" | "Right" | "l") if f.color_row.has_focus() => {
                let n = COLORS.len();
                let step = if matches!(k, "Left" | "h") { n - 1 } else { 1 };
                f.color_idx.set((f.color_idx.get() + step) % n);
                f.show_dots();
            }
            (_, "Down") => {
                self.win.child_focus(gtk::DirectionType::TabForward);
            }
            (_, "Up") => {
                self.win.child_focus(gtk::DirectionType::TabBackward);
            }
            // no field here: the keys go nowhere
            (_, k) if f.sink.has_focus() && k != "Tab" => {}
            _ => return false,
        }
        true
    }

    /// Shift+Tab goes back only from a page's first field; elsewhere it moves the focus.
    fn form_at_start(&self) -> bool {
        let f = &self.form;
        let inside = |w: &gtk::Widget| w.has_focus() || w.focus_child().is_some();
        match f.page.get() {
            Page::Address => inside(f.email.upcast_ref()),
            Page::SignIn => f.sink.has_focus() || inside(f.pass.upcast_ref()),
            Page::Done => inside(f.name.upcast_ref()),
            _ => true,
        }
    }

    /// Esc: cancel the browser wait, else one step back, else close.
    fn form_back(self: &Rc<Self>) {
        let f = &self.form;
        if let Some(cancel) = f.cancel.borrow().as_ref() {
            cancel.store(true, Ordering::Relaxed);
            return;
        }
        let again = f.again.borrow().is_some();
        match f.page.get() {
            Page::Welcome => {
                let before = f.theme_before.borrow().clone();
                self.apply_theme(&before);
                self.form_close();
            }
            Page::Address if f.first_run.get() => self.form_page(Page::Welcome),
            Page::Address => self.form_close(),
            Page::SignIn | Page::Check if again => self.form_close(),
            Page::SignIn => {
                f.advance.set(false);
                self.form_page(Page::Address);
            }
            Page::Check => {
                f.run.set(f.run.get() + 1);
                f.busy.set(false);
                if f.server_trouble.get() {
                    f.adv.set_reveal_child(true);
                }
                self.form_page(Page::SignIn);
            }
            Page::Done => self.form_page(Page::SignIn),
        }
    }

    /// Ctrl+O: password, or browser sign-in. Gmail toggles between Google and a password, other
    /// domains go password -> Google (-> Microsoft with an own client). Outlook addresses without
    /// one stay on the password.
    fn switch_method(self: &Rc<Self>) {
        let f = &self.form;
        if f.again.borrow().is_some() {
            return;
        }
        let own = oauth::provider_for_address(&f.email.text());
        if own.is_some_and(|p| !sign_in_offered(p)) {
            return;
        }
        let ms = sign_in_offered(Provider::Microsoft);
        let next = match (f.method.get(), own) {
            (None, Some(p)) => Some(p),
            (Some(_), Some(_)) => None,
            (None, None) => Some(Provider::Google),
            (Some(Provider::Google), None) if ms => Some(Provider::Microsoft),
            (Some(_), None) => None,
        };
        f.chosen.set(true);
        f.method.set(next);
        f.signed.replace(None);
        f.fill_servers();
        self.form_page(Page::SignIn);
    }

    fn signin_error(&self, text: &str) {
        self.form.signin_note.set_text(text);
        self.form.signin_note.set_css_classes(&["form-error"]);
    }

    /// Enter on the sign-in page (or on a failed check): the check page runs the browser sign-in
    /// when one is needed, then IMAP and SMTP.
    fn form_run(self: &Rc<Self>) {
        let f = &self.form;
        if f.busy.get() {
            return;
        }
        let again = f.again.borrow().clone();
        let acc = match &again {
            Some(name) => match self.accounts.borrow().iter().find(|a| &a.name == name) {
                Some(a) => a.clone(),
                None => return self.form_close(),
            },
            None => match f.account() {
                Ok(a) => resolved(a),
                Err(e) => return self.signin_error(&e),
            },
        };
        let p = f.method.get();
        let pass = f.pass.text().to_string();
        if p.is_none() && pass.is_empty() {
            return self.signin_error("Enter the password first.");
        }
        if again.is_none() && self.accounts.borrow().iter().any(|a| a.email == acc.email) {
            return self.signin_error(&format!("{} is already in brief.", acc.email));
        }
        f.signin_note.set_text("");
        let signed = f.signed.borrow().clone().filter(|s| s.email == acc.email && Some(s.provider.as_str()) == p.map(Provider::name) && again.is_none());
        let browser = p.is_some() && signed.is_none();
        if browser && oauth::check(&acc.oauth).is_err() {
            let who = p.map_or("", Provider::label);
            return self.signin_error(&format!("This build of brief has no {who} sign-in, see {}. Ctrl+O uses a password.", oauth::README_SECTION));
        }
        let mut items = Vec::new();
        if let Some(p) = p {
            items.push((Item::SignIn, format!("Sign in with {} in the browser", p.label())));
        }
        if again.is_none() {
            items.push((Item::Imap, "Log in to IMAP".to_owned()));
            items.push((Item::Smtp, "Log in to SMTP".to_owned()));
        }
        f.set_items(&items);
        f.check_title.set_text(if again.is_some() { "Signing in again" } else { "Checking" });
        f.failed.set(false);
        f.server_trouble.set(false);
        f.busy.set(true);
        self.form_page(Page::Check);
        if let Some(s) = &signed {
            f.mark(Item::SignIn, "ok", &s.email);
        }
        let cancel = Arc::new(AtomicBool::new(false));
        if browser {
            f.mark(Item::SignIn, "run", "");
            f.cancel.replace(Some(cancel.clone()));
            self.update_hints();
        }
        let (tx, rx) = async_channel::unbounded::<Progress>();
        let check = again.is_none();
        std::thread::spawn(move || {
            let send = |s: Progress| {
                let _ = tx.send_blocking(s);
            };
            let secret = if !acc.is_oauth() {
                pass
            } else if !browser {
                match oauth::access_token(&acc) {
                    Ok(t) => t,
                    Err(e) => return send(Progress::Fail(Item::SignIn, e)),
                }
            } else {
                let open = |url: &str| {
                    eprintln!("brief: sign in at {url}");
                    super::open_uri(url);
                    send(Progress::Waiting(url.to_owned()));
                };
                let t = match oauth::sign_in(&acc, open, &cancel) {
                    Ok(t) => t,
                    Err(e) => return send(Progress::Fail(Item::SignIn, e)),
                };
                if acc.oauth.provider == "google"
                    && let Some(other) = oauth::other_account(&acc.email, &t.id)
                {
                    return send(Progress::Fail(Item::SignIn, format!("the browser signed in as {other}, not {}", acc.email)));
                }
                let refresh = t.refresh.clone().unwrap_or_default();
                send(Progress::Signed(SignedIn { email: acc.email.clone(), provider: acc.oauth.provider.clone(), refresh, name: t.id.name.clone() }));
                t.access
            };
            if check {
                run_checks(&acc, &secret, &tx);
            }
        });
        self.follow(rx);
    }

    fn form_failed(self: &Rc<Self>, item: Item, err: &str) {
        let f = &self.form;
        f.busy.set(false);
        f.cancel.replace(None);
        f.failed.set(true);
        f.mark(item, "fail", "");
        let acc = f.account().ok();
        let server = match item {
            Item::Smtp => acc.as_ref().map(|a| a.smtp.clone()),
            _ => acc.as_ref().map(|a| a.imap.clone()),
        }
        .unwrap_or(Server { host: String::new(), port: 0, security: Security::Tls });
        let svc = service(&f.email.text(), f.method.get(), f.lookup().and_then(|l| l.mx));
        let (what, next, servers) = explain(item, err, &server, f.method.get(), &svc);
        f.server_trouble.set(servers);
        f.check_error.set_text(&what);
        f.check_error.set_tooltip_text(Some(err));
        f.check_hint.set_text(&next);
        crate::log::warn(&format!("setup {}: {err}", f.email.text().trim()));
        self.update_hints();
    }

    /// Shows the progress as it comes; all green moves on to the done page.
    fn follow(self: &Rc<Self>, rx: async_channel::Receiver<Progress>) {
        let app = self.clone();
        let run = self.form.run.get();
        glib::spawn_future_local(async move {
            let f = &app.form;
            while let Ok(p) = rx.recv().await {
                if f.run.get() != run {
                    return;
                }
                match p {
                    Progress::Start(item) => f.mark(item, "run", ""),
                    Progress::Ok(item, detail) => f.mark(item, "ok", &detail),
                    Progress::Waiting(url) => {
                        f.link.replace(url);
                        f.check_hint.set_text("Approve brief in the browser tab that just opened. Esc cancels, Ctrl+L copies the link if no tab opened.");
                    }
                    Progress::Signed(s) => {
                        f.cancel.replace(None);
                        f.check_hint.set_text("");
                        f.mark(Item::SignIn, "ok", &s.email);
                        if f.display.text().trim().is_empty() && !s.name.is_empty() {
                            f.display.set_text(&s.name);
                        }
                        f.signed.replace(Some(s));
                        app.update_hints();
                    }
                    Progress::Fail(item, e) => return app.form_failed(item, &e),
                }
            }
            if f.run.get() != run {
                return;
            }
            f.busy.set(false);
            let again = f.again.borrow().clone();
            if let Some(name) = again {
                let Some(s) = f.signed.borrow().clone() else { return };
                if let Err(e) = config::store_refresh_token(&s.email, &s.refresh) {
                    return app.form_failed(Item::SignIn, &e);
                }
                app.form_close();
                app.signed_in_again(&name);
                return;
            }
            // a moment to see the last tick
            glib::timeout_add_local_once(Duration::from_millis(450), move || {
                if app.form.run.get() == run && app.form.page.get() == Page::Check {
                    app.form_done_page();
                }
            });
        });
    }

    fn form_done_page(self: &Rc<Self>) {
        let f = &self.form;
        let svc = service(&f.email.text(), f.method.get(), f.lookup().and_then(|l| l.mx));
        f.done_title.set_text(&format!("{} is ready", svc.name));
        let display = f.display.text().trim().to_owned();
        let email = f.email.text().trim().to_owned();
        f.done_from.set_text(&if display.is_empty() { format!("Sends as {email}") } else { format!("Sends as {display} <{email}>") });
        // the theme's color for the next account
        let n = self.accounts.borrow().len() % 4;
        f.dots[0].set_css_classes(&["color-dot", &format!("dot-auto-{n}")]);
        f.show_dots();
        self.form_page(Page::Done);
    }

    /// Saves the secret and the account and starts its sync. `another` opens a fresh address page.
    fn form_finish(self: &Rc<Self>, another: bool) {
        let f = &self.form;
        let mut acc = match f.account() {
            Ok(a) => a,
            Err(e) => return self.toast(&e, true),
        };
        if self.accounts.borrow().iter().any(|a| a.name == acc.name) {
            self.toast(&format!("there is already an account called {}, pick another label", acc.name), true);
            return;
        }
        let stored = if acc.is_oauth() {
            match f.signed.borrow().clone().filter(|s| s.email == acc.email) {
                Some(s) => config::store_refresh_token(&acc.email, &s.refresh),
                None => Err("sign in first".into()),
            }
        } else {
            let pass = f.pass.text().to_string();
            if config::keyring_store(&acc.email, &pass) {
                acc.password_cmd = crate::platform::keyring_cmd(&acc.email);
                Ok(())
            } else {
                config::secrets_file_store(&acc.email, &pass).map_err(|e| format!("could not store the password: {e}"))
            }
        };
        if let Err(e) = stored {
            return self.toast(&e, true);
        }
        if let Err(e) = config::append_account(&acc) {
            return self.toast(&format!("could not write the config: {e}"), true);
        }
        self.add_account(resolved(acc));
        if another {
            f.reset();
            for e in [&f.email, &f.display, &f.name] {
                e.set_text("");
            }
            f.first_run.set(false);
            self.overlay_scrim.remove_css_class("solid");
            self.overlay_scrim.set_margin_bottom(0);
            self.form_page(Page::Address);
        } else {
            self.form_close();
        }
    }

    /// The check page for an account whose sign-in ran out; the browser opens right away.
    pub fn sign_in_again(self: &Rc<Self>, name: &str) {
        let Some(acc) = self.accounts.borrow().iter().find(|a| a.name == name).cloned() else { return };
        let Some(p) = Provider::from_name(&acc.oauth.provider) else {
            self.toast(&format!("{name} has no Google or Microsoft sign-in, run `brief login {name}`"), true);
            return;
        };
        let f = &self.form;
        f.reset();
        f.again.replace(Some(acc.name.clone()));
        f.chosen.set(true);
        f.email.set_text(&acc.email);
        f.method.set(Some(p));
        self.show_overlay(f.root.upcast_ref());
        self.set_mode(Mode::Form);
        self.form_run();
    }

    fn signed_in_again(self: &Rc<Self>, name: &str) {
        let Some(i) = self.acct_idx(name) else { return };
        self.send(i, Cmd::SyncAll { manual: true });
        self.rebuild_side();
        self.update_sync();
        self.toast(&format!("{name} signed in again, syncing…"), false);
    }
}

/// The account with its OAuth settings completed from the current config file.
fn resolved(acc: Account) -> Account {
    config::load().unwrap_or_default().resolve_oauth(acc)
}

pub(super) enum Progress {
    Start(Item),
    Ok(Item, String),
    Fail(Item, String),
    /// The browser opened this link.
    Waiting(String),
    Signed(SignedIn),
}

pub(super) fn run_checks(acc: &Account, secret: &str, tx: &async_channel::Sender<Progress>) {
    let send = |s: Progress| {
        let _ = tx.send_blocking(s);
    };
    send(Progress::Start(Item::Imap));
    match imap::connect(acc, secret) {
        Ok(mut session) => {
            let folders = session.list(Some(""), Some("*")).map(|n| n.len()).unwrap_or(0);
            let _ = session.logout();
            let s = if folders == 1 { "" } else { "s" };
            send(Progress::Ok(Item::Imap, format!("{} · {folders} folder{s}", acc.imap.host)));
        }
        Err(e) => return send(Progress::Fail(Item::Imap, e)),
    }
    send(Progress::Start(Item::Smtp));
    match smtp::test(acc, secret) {
        Ok(()) => send(Progress::Ok(Item::Smtp, acc.smtp.host.clone())),
        Err(e) => send(Progress::Fail(Item::Smtp, e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookups_only_for_unknown_domains() {
        assert_eq!(lookup_domain("me@firma.ch").as_deref(), Some("firma.ch"));
        assert_eq!(lookup_domain("Me@Firma.CH").as_deref(), Some("firma.ch"));
        assert_eq!(lookup_domain("me@gmail.com"), None);
        assert_eq!(lookup_domain("me@icloud.com"), None);
        assert_eq!(lookup_domain("me@firma.c"), None);
        assert_eq!(lookup_domain("me@firma"), None);
        assert_eq!(lookup_domain("@firma.ch"), None);
    }

    #[test]
    fn services_by_address() {
        assert_eq!(service("me@gmail.com", None, None).name, "Gmail");
        assert!(service("me@gmail.com", None, None).app_password.contains("google.com"));
        assert_eq!(service("me@hotmail.ch", None, None).name, "Outlook");
        assert_eq!(service("me@icloud.com", None, None).name, "iCloud");
        assert_eq!(service("me@gmx.ch", None, None).name, "GMX");
        assert_eq!(service("me@pm.me", None, None).name, "Proton Mail Bridge");
        assert_eq!(service("me@firma.ch", None, Some(Provider::Google)).name, "Google Workspace");
        assert_eq!(service("me@firma.ch", Some(Provider::Microsoft), None).name, "Microsoft 365");
        assert_eq!(service("me@firma.ch", None, None).name, "firma.ch");
    }

    #[test]
    fn errors_in_plain_words() {
        let s = |h: &str| Server { host: h.into(), port: 993, security: Security::Tls };
        let gmail = service("me@gmail.com", None, None);
        let (what, next, servers) =
            explain(Item::Imap, "login refused: [AUTHENTICATIONFAILED] Invalid credentials", &s("imap.gmail.com"), None, &gmail);
        assert_eq!(what, "IMAP did not accept the login.");
        assert!(next.contains("app password") && !servers);
        let (what, _, servers) =
            explain(Item::Imap, "mail.firma.ch: failed to lookup address information: Name or service not known", &s("mail.firma.ch"), None, &gmail);
        assert_eq!(what, "mail.firma.ch does not exist.");
        assert!(servers);
        let (what, _, _) = explain(Item::Smtp, "x:587: Connection refused (os error 111)", &s("x"), None, &gmail);
        assert_eq!(what, "Could not reach x:993.");
        let (what, next, _) = explain(Item::Imap, "invalid peer certificate: UnknownIssuer", &s("x"), None, &gmail);
        assert!(what.contains("secure connection") && next.contains("starttls"));
        let google = Some(Provider::Google);
        assert_eq!(explain(Item::SignIn, "sign-in cancelled", &s("x"), google, &gmail).0, "Sign-in cancelled.");
        assert_eq!(explain(Item::SignIn, "sign-in refused (access_denied)", &s("x"), google, &gmail).0, "Google did not let brief in.");
        assert_eq!(
            explain(Item::SignIn, "the browser signed in as a@gmail.com, not b@gmail.com", &s("x"), google, &gmail).0,
            "The browser signed in as a@gmail.com, not b@gmail.com."
        );
    }
}
