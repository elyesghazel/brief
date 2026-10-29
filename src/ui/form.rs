//! `A`: add an account. Servers are guessed from the address, the login is tested over IMAP and
//! SMTP before anything is saved, the password goes to the keyring (or secrets.toml, 0600).
//!
//! Gmail and Microsoft addresses (and Workspace / Microsoft 365 domains, found by their MX) switch
//! the form to sign-in mode: no password row, the servers folded away, and Enter runs the browser
//! sign-in, tests IMAP and SMTP, stores the refresh token and adds the account. The same form
//! signs an existing account in again when its token ran out.

use super::{App, Mode, label};
use crate::config::{self, Account, Auth, OAuth, Security, Server};
use crate::imap::Cmd;
use crate::oauth::{self, Provider};
use crate::{imap, smtp};
use gtk::glib;
use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const HINT: &str = "iCloud / Yahoo need an app password. Ctrl+O signs in with Google or Microsoft instead. Ctrl+T tests the servers, Ctrl+Enter tests and adds the account.";

/// A finished browser sign-in.
#[derive(Clone)]
struct SignedIn {
    email: String,
    provider: String,
    refresh: String,
    /// The name the provider knows, for the From line.
    name: String,
}

pub struct Form {
    pub root: gtk::Box,
    title: gtk::Label,
    email: gtk::Entry,
    pass: gtk::PasswordEntry,
    name: gtk::Entry,
    display: gtk::Entry,
    user: gtk::Entry,
    imap_host: gtk::Entry,
    imap_port: gtk::Entry,
    imap_sec: gtk::Entry,
    smtp_host: gtk::Entry,
    smtp_port: gtk::Entry,
    smtp_sec: gtk::Entry,
    /// Email, Password, Label, Your name, then Username and the six server rows.
    rows: Vec<gtk::Box>,
    signin_row: gtk::Box,
    signin_text: gtk::Label,
    signin_key: gtk::Label,
    servers_row: gtk::Box,
    servers_text: gtk::Label,
    status: gtk::Label,
    /// The user changed a server field; stop overwriting them from the address.
    touched: Rc<Cell<bool>>,
    /// Set while brief itself fills the server fields.
    filling: Cell<bool>,
    busy: Rc<Cell<bool>>,
    signed: Rc<RefCell<Option<SignedIn>>>,
    /// Set while the browser sign-in waits; Esc cancels it.
    cancel: RefCell<Option<Arc<AtomicBool>>>,
    /// The sign-in link while the browser is waiting, for Ctrl+L.
    link: RefCell<String>,
    /// Browser sign-in with this provider instead of a password.
    method: Cell<Option<Provider>>,
    /// Ctrl+O picked the method; the address no longer switches it.
    chosen: Cell<bool>,
    /// Server rows shown in sign-in mode (Ctrl+Shift+S).
    expanded: Cell<bool>,
    /// MX answers per domain: who receives its mail.
    mx: RefCell<HashMap<String, Option<Provider>>>,
    mx_gen: Cell<u64>,
    /// Signing in this existing account again instead of adding one.
    again: RefCell<Option<String>>,
}

/// How many rows come before Username and the servers.
const MAIN_ROWS: usize = 4;

impl Form {
    pub fn new() -> Form {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.add_css_class("dialog");
        root.set_halign(gtk::Align::Center);
        root.set_valign(gtk::Align::Center);
        root.set_vexpand(true);
        root.set_size_request(620, -1);
        let title = label("add account", "dialog-title");
        root.append(&title);
        let grid = gtk::Box::new(gtk::Orientation::Vertical, 0);
        grid.add_css_class("form");
        let entry = |ph: &str| {
            let e = gtk::Entry::new();
            e.add_css_class("field");
            e.set_placeholder_text(Some(ph));
            e.set_hexpand(true);
            e
        };
        let email = entry("you@example.com");
        let pass = gtk::PasswordEntry::new();
        pass.add_css_class("field");
        pass.set_show_peek_icon(true);
        pass.set_hexpand(true);
        let name = entry("short label, e.g. Work");
        let display = entry("name in the From line");
        let user = entry("login, if not the address");
        let imap_host = entry("imap.example.com");
        let imap_port = entry("993");
        let imap_sec = entry("tls | starttls | none");
        let smtp_host = entry("smtp.example.com");
        let smtp_port = entry("587");
        let smtp_sec = entry("tls | starttls | none");
        let row = |l: &str, w: &gtk::Widget| {
            let rowbox = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            rowbox.add_css_class("form-row");
            let lab = label(l, "form-label");
            lab.set_width_chars(11);
            rowbox.append(&lab);
            rowbox.append(w);
            rowbox
        };
        let chip = |text: &str| {
            let k = label(text, "form-key");
            k.set_valign(gtk::Align::Center);
            k
        };

        let signin_text = label("", "signin-text");
        signin_text.set_hexpand(true);
        super::ellipsize(&signin_text);
        let signin_key = chip("Enter");
        let signin_box = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        signin_box.append(&signin_text);
        signin_box.append(&signin_key);
        signin_box.set_hexpand(true);
        let signin_row = row("Sign-in", signin_box.upcast_ref());
        signin_row.add_css_class("signin-row");

        let servers_text = label("", "servers-text");
        servers_text.set_hexpand(true);
        super::ellipsize(&servers_text);
        let servers_box = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        servers_box.append(&servers_text);
        servers_box.append(&chip("Ctrl+Shift+S"));
        servers_box.set_hexpand(true);
        let servers_row = row("Servers", servers_box.upcast_ref());

        let fields: [(&str, &gtk::Widget); 11] = [
            ("Email", email.upcast_ref()),
            ("Password", pass.upcast_ref()),
            ("Label", name.upcast_ref()),
            ("Your name", display.upcast_ref()),
            ("Username", user.upcast_ref()),
            ("IMAP host", imap_host.upcast_ref()),
            ("IMAP port", imap_port.upcast_ref()),
            ("IMAP sec", imap_sec.upcast_ref()),
            ("SMTP host", smtp_host.upcast_ref()),
            ("SMTP port", smtp_port.upcast_ref()),
            ("SMTP sec", smtp_sec.upcast_ref()),
        ];
        let rows: Vec<gtk::Box> = fields.iter().map(|(l, w)| row(l, w)).collect();
        for (i, r) in rows.iter().enumerate() {
            if i == 2 {
                grid.append(&signin_row);
            }
            grid.append(r);
        }
        grid.append(&servers_row);
        root.append(&grid);
        let status = label("", "form-hint");
        status.set_wrap(true);
        status.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        status.set_max_width_chars(60);
        root.append(&status);
        Form {
            root,
            title,
            email,
            pass,
            name,
            display,
            user,
            imap_host,
            imap_port,
            imap_sec,
            smtp_host,
            smtp_port,
            smtp_sec,
            rows,
            signin_row,
            signin_text,
            signin_key,
            servers_row,
            servers_text,
            status,
            touched: Rc::new(Cell::new(false)),
            filling: Cell::new(false),
            busy: Rc::new(Cell::new(false)),
            signed: Rc::new(RefCell::new(None)),
            cancel: RefCell::new(None),
            link: RefCell::new(String::new()),
            method: Cell::new(None),
            chosen: Cell::new(false),
            expanded: Cell::new(false),
            mx: RefCell::new(HashMap::new()),
            mx_gen: Cell::new(0),
            again: RefCell::new(None),
        }
    }

    pub fn wire(&self, app: &Rc<App>) {
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
            let touched = self.touched.clone();
            let weak = Rc::downgrade(app);
            e.connect_changed(move |_| {
                let Some(app) = weak.upgrade() else { return };
                if !app.form.filling.get() {
                    touched.set(true);
                }
                app.form.show_servers();
            });
        }
    }

    fn server_entries(&self) -> [&gtk::Entry; 6] {
        [&self.imap_host, &self.imap_port, &self.imap_sec, &self.smtp_host, &self.smtp_port, &self.smtp_sec]
    }

    pub fn open(&self, app: &Rc<App>) {
        for e in [&self.email, &self.name, &self.display, &self.user, &self.imap_host, &self.imap_port, &self.imap_sec, &self.smtp_host, &self.smtp_port, &self.smtp_sec] {
            e.set_text("");
        }
        self.reset();
        self.set_status(HINT, "form-hint");
        app.show_overlay(self.root.upcast_ref());
        app.set_mode(Mode::Form);
        self.email.grab_focus();
    }

    /// Back to an empty add-account form.
    fn reset(&self) {
        self.again.replace(None);
        self.title.set_text("add account");
        self.email.set_editable(true);
        self.email.set_can_focus(true);
        self.pass.set_text("");
        self.touched.set(false);
        self.signed.replace(None);
        self.method.set(None);
        self.chosen.set(false);
        self.expanded.set(false);
        self.link.replace(String::new());
        self.apply_method();
    }

    fn account(&self) -> Result<Account, String> {
        let sec = |e: &gtk::Entry| match e.text().trim().to_lowercase().as_str() {
            "" | "tls" | "ssl" => Ok(Security::Tls),
            "starttls" => Ok(Security::Starttls),
            "none" | "plain" => Ok(Security::None),
            other => Err(format!("security must be tls, starttls or none, not {other}")),
        };
        let port = |e: &gtk::Entry, what: &str| e.text().trim().parse::<u16>().map_err(|_| format!("{what} port is not a number"));
        let email = self.email.text().trim().to_owned();
        if !email.contains('@') {
            return Err("enter an email address".into());
        }
        let name = match self.name.text().trim() {
            "" => email.clone(),
            n => n.to_owned(),
        };
        Ok(Account {
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
            color: String::new(),
        })
    }

    fn set_status(&self, text: &str, class: &str) {
        self.status.set_text(text);
        self.status.set_css_classes(&[class]);
    }

    fn waiting(&self) -> bool {
        self.cancel.borrow().is_some()
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
        let (i, s) = match self.method.get() {
            Some(p) if oauth::provider_for_address(&text).is_none() => p.servers(),
            _ => config::guess_servers(&text),
        };
        self.filling.set(true);
        self.imap_host.set_text(&i.host);
        self.imap_port.set_text(&i.port.to_string());
        self.imap_sec.set_text(i.security.as_str());
        self.smtp_host.set_text(&s.host);
        self.smtp_port.set_text(&s.port.to_string());
        self.smtp_sec.set_text(s.security.as_str());
        self.filling.set(false);
    }

    /// The folded "Servers" row's summary.
    fn show_servers(&self) {
        let (i, s) = (self.imap_host.text(), self.smtp_host.text());
        let text = if i == s { i.to_string() } else { format!("{i} · {s}") };
        self.servers_text.set_text(&text);
    }

    /// Shows the rows for the current method: the password, or the sign-in line with the servers
    /// folded away.
    fn apply_method(&self) {
        let p = self.method.get();
        let again = self.again.borrow().is_some();
        self.rows[1].set_visible(p.is_none());
        self.signin_row.set_visible(p.is_some());
        for r in &self.rows[2..MAIN_ROWS] {
            r.set_visible(!again);
        }
        let open = p.is_none() || self.expanded.get();
        for r in &self.rows[MAIN_ROWS..] {
            r.set_visible(open && !again);
        }
        self.servers_row.set_visible(!open && !again);
        self.display.set_placeholder_text(Some(match p {
            Some(Provider::Google) => "name in the From line, Google fills it in",
            Some(Provider::Microsoft) => "name in the From line, Microsoft fills it in",
            None => "name in the From line",
        }));
        self.show_signin();
        self.show_servers();
    }

    /// The sign-in row: what Enter does, the wait, or who signed in.
    fn show_signin(&self) {
        let Some(p) = self.method.get() else { return };
        let signed = self.signed.borrow().clone();
        let (text, class, key) = if self.waiting() {
            (format!("Waiting for {} in your browser…", p.label()), "wait", Some("Esc"))
        } else if let Some(s) = signed.filter(|s| s.provider == p.name()) {
            (format!("✓ Signed in as {}", s.email), "ok", None)
        } else {
            (format!("Sign in with {}", p.label()), "", Some("Enter"))
        };
        self.signin_text.set_text(&text);
        self.signin_text.set_css_classes(&["signin-text", class]);
        self.signin_key.set_visible(key.is_some());
        self.signin_key.set_text(key.unwrap_or_default());
    }

    /// The status line under the rows when nothing runs.
    fn idle_hint(&self) -> String {
        let email = self.email.text();
        let domain = email.rsplit('@').next().unwrap_or_default().to_lowercase();
        match self.method.get() {
            None => HINT.into(),
            Some(p) if oauth::provider_for_address(&email).is_none() && !self.chosen.get() => {
                let service = if p == Provider::Google { "Google Workspace" } else { "Microsoft 365" };
                format!("{domain} gets its mail through {service}. Ctrl+O uses a password instead.")
            }
            Some(p) => format!("{} signs you in; brief never sees your password and keeps only a token.", p.label()),
        }
    }

    /// Keyboard hints for the status bar.
    pub fn hints(&self) -> &'static [(&'static str, &'static str)] {
        if self.waiting() {
            &[("Esc", "cancel"), ("Ctrl+L", "copy the link")]
        } else if self.again.borrow().is_some() {
            &[("Enter", "sign in"), ("Esc", "cancel")]
        } else if self.method.get().is_some() {
            &[("Enter", "sign in"), ("Ctrl+Shift+S", "servers"), ("Ctrl+O", "use a password"), ("Esc", "cancel")]
        } else {
            &[("Tab", "next"), ("Ctrl+O", "browser sign-in"), ("Ctrl+T", "test"), ("Ctrl+Enter", "test & add"), ("Esc", "cancel")]
        }
    }
}

/// The domain to ask DNS about: a complete-looking address that no built-in rule knows.
fn mx_domain(email: &str) -> Option<String> {
    let (local, domain) = email.trim().rsplit_once('@')?;
    let domain = domain.to_lowercase();
    let tld = domain.rsplit_once('.')?.1;
    let known = oauth::provider_for_address(email).is_some() || !config::guess_servers(email).0.host.starts_with("mail.");
    (!local.is_empty() && tld.len() >= 2 && tld.chars().all(|c| c.is_ascii_alphabetic()) && !known).then_some(domain)
}

impl App {
    fn form_email_changed(self: &Rc<Self>) {
        let f = &self.form;
        let text = f.email.text().trim().to_owned();
        if f.signed.borrow().as_ref().is_some_and(|s| s.email != text) {
            f.signed.replace(None);
        }
        if f.again.borrow().is_none() {
            let domain = text.rsplit('@').next().unwrap_or("").to_owned();
            if !f.name.has_focus() && text.contains('@') {
                let label = domain.split('.').next().unwrap_or("").to_owned();
                let mut c = label.chars();
                let cap = c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default();
                if f.name.text().is_empty() || f.name.widget_name() == "auto" {
                    f.name.set_text(&cap);
                    f.name.set_widget_name("auto");
                }
            }
        }
        if !f.chosen.get() {
            // a typed password wins over the MX guess
            let mx = mx_domain(&text).filter(|_| f.pass.text().is_empty()).and_then(|d| f.mx.borrow().get(&d).copied().flatten());
            f.method.set(oauth::provider_for_address(&text).or(mx));
            self.schedule_mx(&text);
        }
        f.fill_servers();
        f.apply_method();
        if !f.busy.get() {
            f.set_status(&f.idle_hint(), "form-hint");
        }
        self.update_hints();
    }

    /// Asks DNS who receives the domain's mail once the address stops changing.
    fn schedule_mx(self: &Rc<Self>, email: &str) {
        let f = &self.form;
        let g = f.mx_gen.get() + 1;
        f.mx_gen.set(g);
        let Some(domain) = mx_domain(email) else { return };
        if f.mx.borrow().contains_key(&domain) {
            return;
        }
        let me = Rc::downgrade(self);
        glib::timeout_add_local_once(Duration::from_millis(600), move || {
            let Some(app) = me.upgrade() else { return };
            if app.form.mx_gen.get() != g {
                return;
            }
            let (tx, rx) = async_channel::bounded(1);
            let d = domain.clone();
            std::thread::spawn(move || {
                let hosts = oauth::lookup_mx(&d, Duration::from_secs(3)).unwrap_or_default();
                let _ = tx.send_blocking(oauth::provider_for_mx(hosts.iter().map(String::as_str)));
            });
            glib::spawn_future_local(async move {
                let Ok(found) = rx.recv().await else { return };
                app.form.mx.borrow_mut().insert(domain.clone(), found);
                let f = &app.form;
                if found.is_some() && !f.chosen.get() && mx_domain(&f.email.text()).as_deref() == Some(domain.as_str()) {
                    app.form_email_changed();
                }
            });
        });
    }

    pub fn form_key(self: &Rc<Self>, k: &str) -> bool {
        let signin = self.form.method.get().is_some();
        match k {
            "Escape" => {
                if let Some(cancel) = self.form.cancel.borrow().as_ref() {
                    cancel.store(true, Ordering::Relaxed);
                } else if !self.form.busy.get() {
                    self.form.reset();
                    self.hide_overlay();
                }
            }
            "C-o" => self.switch_method(),
            "C-S-s" if signin => {
                let f = &self.form;
                f.expanded.set(!f.expanded.get());
                f.apply_method();
                if f.expanded.get() {
                    f.imap_host.grab_focus();
                } else {
                    f.email.grab_focus();
                }
            }
            "C-l" if self.form.waiting() => {
                self.win.clipboard().set_text(&self.form.link.borrow());
                self.toast("sign-in link copied", false);
            }
            "Return" | "C-Return" | "C-KP_Enter" if signin => self.sign_in_flow(true),
            "C-t" if signin => self.sign_in_flow(false),
            "C-Return" | "C-KP_Enter" => self.check_form(true),
            "C-t" => self.check_form(false),
            "Return" => {
                // Enter moves on, and submits from the last field
                if self.form.smtp_sec.has_focus() || self.form.smtp_sec.first_child().is_some_and(|c| c.has_focus()) {
                    self.check_form(true);
                } else {
                    self.win.child_focus(gtk::DirectionType::TabForward);
                }
            }
            "Down" => {
                self.win.child_focus(gtk::DirectionType::TabForward);
            }
            "Up" => {
                self.win.child_focus(gtk::DirectionType::TabBackward);
            }
            _ => return false,
        }
        true
    }

    /// Ctrl+O: password, or browser sign-in. Free addresses toggle between their provider and a
    /// password, other domains go password -> Google -> Microsoft.
    fn switch_method(self: &Rc<Self>) {
        let f = &self.form;
        if f.busy.get() || f.again.borrow().is_some() {
            return;
        }
        let own = oauth::provider_for_address(&f.email.text());
        let detected = mx_domain(&f.email.text()).and_then(|d| f.mx.borrow().get(&d).copied().flatten());
        let next = match (f.method.get(), own) {
            (None, Some(p)) => Some(p),
            (Some(_), Some(_)) => None,
            (None, None) => Some(detected.unwrap_or(Provider::Google)),
            (Some(Provider::Google), None) if detected != Some(Provider::Microsoft) => Some(Provider::Microsoft),
            (Some(Provider::Microsoft), None) if detected == Some(Provider::Microsoft) => Some(Provider::Google),
            (Some(_), None) => None,
        };
        f.chosen.set(true);
        f.method.set(next);
        f.signed.replace(None);
        f.fill_servers();
        f.apply_method();
        f.set_status(&f.idle_hint(), "form-hint");
        if next.is_none() {
            f.pass.grab_focus();
        } else if f.pass.has_focus() || f.pass.focus_child().is_some() {
            f.email.grab_focus();
        }
        self.update_hints();
    }

    /// Enter in sign-in mode: browser sign-in (skipped when it already worked), IMAP and SMTP
    /// check, then the account is added. `save` false only tests.
    fn sign_in_flow(self: &Rc<Self>, save: bool) {
        let f = &self.form;
        let Some(p) = f.method.get() else { return };
        if f.busy.get() {
            return;
        }
        let again = f.again.borrow().clone();
        let acc = match &again {
            Some(name) => match self.accounts.borrow().iter().find(|a| &a.name == name) {
                Some(a) => a.clone(),
                None => return f.set_status("that account is gone", "form-error"),
            },
            None => match f.account() {
                Ok(a) => resolved(oauth_account(a, p.name())),
                Err(e) => return f.set_status(&e, "form-error"),
            },
        };
        if save && again.is_none() && self.accounts.borrow().iter().any(|a| a.name == acc.name) {
            return f.set_status(&format!("there is already an account called {}", acc.name), "form-error");
        }
        if !oauth::has_client(&acc.oauth) {
            return f.set_status(
                &format!("This build of brief has no {} sign-in yet: see {}.", p.label(), oauth::README_SECTION),
                "form-error",
            );
        }
        if let Err(e) = oauth::check(&acc.oauth) {
            return f.set_status(&e, "form-error");
        }
        let signed = f.signed.borrow().clone().filter(|s| s.email == acc.email && s.provider == p.name() && again.is_none());
        let cancel = Arc::new(AtomicBool::new(false));
        if signed.is_none() {
            f.cancel.replace(Some(cancel.clone()));
        }
        let (tx, rx) = async_channel::unbounded::<Step>();
        let check = again.is_none();
        std::thread::spawn(move || {
            let send = |s: Step| {
                let _ = tx.send_blocking(s);
            };
            let access = if signed.is_some() {
                match oauth::access_token(&acc) {
                    Ok(t) => t,
                    Err(e) => return send(Step::Fail(e)),
                }
            } else {
                let open = |url: &str| {
                    eprintln!("brief: sign in at {url}");
                    super::open_uri(url);
                    send(Step::Waiting(url.to_owned()));
                };
                let t = match oauth::sign_in(&acc, open, &cancel) {
                    Ok(t) => t,
                    Err(e) => return send(Step::Fail(e)),
                };
                if p == Provider::Google
                    && let Some(other) = oauth::other_account(&acc.email, &t.id)
                {
                    return send(Step::Fail(format!("the browser signed in as {other}, not {}", acc.email)));
                }
                send(Step::Ok(format!("signed in with {} as {}", p.label(), acc.email)));
                let refresh = t.refresh.clone().unwrap_or_default();
                send(Step::Signed(SignedIn { email: acc.email.clone(), provider: p.name().into(), refresh, name: t.id.name.clone() }));
                t.access
            };
            if check {
                run_checks(&acc, &access, &tx);
            }
        });
        let finish = match again {
            Some(name) => Finish::Again(name),
            None if save => match f.account() {
                Ok(a) => Finish::Add(oauth_account(a, p.name()), None),
                Err(e) => return f.set_status(&e, "form-error"),
            },
            None => Finish::Test,
        };
        self.follow_steps(rx, finish);
    }

    /// Logs in over IMAP and SMTP. `save` adds the account when both work, otherwise it only reports.
    fn check_form(self: &Rc<Self>, save: bool) {
        if self.form.busy.get() {
            return;
        }
        let acc = match self.form.account() {
            Ok(a) => a,
            Err(e) => return self.form.set_status(&e, "form-error"),
        };
        if save && self.accounts.borrow().iter().any(|a| a.name == acc.name) {
            return self.form.set_status(&format!("there is already an account called {}", acc.name), "form-error");
        }
        let pass = self.form.pass.text().to_string();
        if pass.is_empty() {
            return self.form.set_status("enter the password, or Ctrl+O to sign in with Google or Microsoft", "form-error");
        }
        let (tx, rx) = async_channel::unbounded::<Step>();
        let (checked, p2) = (acc.clone(), pass.clone());
        std::thread::spawn(move || run_checks(&checked, &p2, &tx));
        self.follow_steps(rx, if save { Finish::Add(acc, Some(pass)) } else { Finish::Test });
    }

    /// Shows the steps as they come, then does what `finish` says once everything passed.
    fn follow_steps(self: &Rc<Self>, rx: async_channel::Receiver<Step>, finish: Finish) {
        self.form.busy.set(true);
        self.form.set_status("connecting…", "form-hint");
        self.form.show_signin();
        self.update_hints();
        let app = self.clone();
        glib::spawn_future_local(async move {
            let f = &app.form;
            let mut lines: Vec<String> = Vec::new();
            let mut failed = false;
            while let Ok(step) = rx.recv().await {
                match step {
                    Step::Ok(l) => lines.push(format!("✓ {l}")),
                    Step::Next(l) => {
                        let mut text = lines.join("\n");
                        text += &format!("\n{l}…");
                        f.set_status(text.trim_start(), "form-hint");
                        continue;
                    }
                    Step::Waiting(url) => {
                        f.link.replace(url);
                        f.show_signin();
                        app.update_hints();
                        f.set_status("Approve brief in the browser tab that just opened. Esc cancels, Ctrl+L copies the link if no tab opened.", "form-hint");
                        continue;
                    }
                    Step::Fail(l) => {
                        lines.push(format!("✗ {l}"));
                        failed = true;
                        f.cancel.replace(None);
                    }
                    Step::Signed(s) => {
                        f.cancel.replace(None);
                        if f.display.text().trim().is_empty() && !s.name.is_empty() {
                            f.display.set_text(&s.name);
                        }
                        f.signed.replace(Some(s));
                        f.show_signin();
                        app.update_hints();
                    }
                }
                f.set_status(&lines.join("\n"), if failed { "form-error" } else { "form-hint" });
            }
            f.busy.set(false);
            f.cancel.replace(None);
            f.show_signin();
            app.update_hints();
            if failed {
                return;
            }
            match finish {
                Finish::Test => {
                    let next = if f.method.get().is_some() { "Enter adds the account" } else { "Ctrl+Enter adds the account" };
                    lines.push(next.into());
                    f.set_status(&lines.join("\n"), "form-ok");
                }
                Finish::Add(mut acc, pass) => {
                    let stored = match pass {
                        Some(pass) => {
                            if config::keyring_store(&acc.email, &pass) {
                                acc.password_cmd = crate::platform::keyring_cmd(&acc.email);
                                Ok(())
                            } else {
                                config::secrets_file_store(&acc.email, &pass).map_err(|e| format!("could not store the password: {e}"))
                            }
                        }
                        None => {
                            let signed = f.signed.borrow().clone().filter(|s| s.email == acc.email);
                            match signed {
                                Some(s) => {
                                    if acc.display_name.is_empty() {
                                        acc.display_name = f.display.text().trim().to_owned();
                                    }
                                    config::store_refresh_token(&acc.email, &s.refresh)
                                }
                                None => Err("sign in first".into()),
                            }
                        }
                    };
                    if let Err(e) = stored {
                        return f.set_status(&e, "form-error");
                    }
                    if let Err(e) = config::append_account(&acc) {
                        return f.set_status(&format!("could not write the config: {e}"), "form-error");
                    }
                    f.reset();
                    app.hide_overlay();
                    app.add_account(resolved(acc));
                }
                Finish::Again(name) => {
                    let Some(s) = f.signed.borrow().clone() else { return };
                    if let Err(e) = config::store_refresh_token(&s.email, &s.refresh) {
                        return f.set_status(&e, "form-error");
                    }
                    f.reset();
                    app.hide_overlay();
                    app.signed_in_again(&name);
                }
            }
        });
    }

    /// The form for an account whose sign-in ran out: the browser opens right away.
    pub fn sign_in_again(self: &Rc<Self>, name: &str) {
        let Some(acc) = self.accounts.borrow().iter().find(|a| a.name == name).cloned() else { return };
        let Some(p) = Provider::from_name(&acc.oauth.provider) else {
            self.toast(&format!("{name} has no Google or Microsoft sign-in, run `brief login {name}`"), true);
            return;
        };
        let f = &self.form;
        f.reset();
        f.again.replace(Some(acc.name.clone()));
        f.title.set_text(&format!("sign in again · {}", acc.name));
        f.chosen.set(true);
        f.email.set_text(&acc.email);
        f.email.set_editable(false);
        f.method.set(Some(p));
        f.apply_method();
        f.set_status("", "form-hint");
        self.show_overlay(f.root.upcast_ref());
        self.set_mode(Mode::Form);
        f.root.grab_focus();
        self.sign_in_flow(true);
    }

    fn signed_in_again(self: &Rc<Self>, name: &str) {
        let Some(i) = self.acct_idx(name) else { return };
        self.send(i, Cmd::SyncAll { manual: true });
        self.rebuild_side();
        self.update_sync();
        self.toast(&format!("{name} signed in again, syncing…"), false);
    }
}

/// The form's account switched to OAuth. Only the provider goes into config.toml, the client
/// comes from `[oauth.<provider>]` or the built-in default.
fn oauth_account(mut acc: Account, provider: &str) -> Account {
    acc.auth = Auth::Oauth2;
    acc.oauth = OAuth { provider: provider.into(), ..OAuth::default() };
    acc
}

/// The account with its OAuth settings completed from the current config file.
fn resolved(acc: Account) -> Account {
    config::load().unwrap_or_default().resolve_oauth(acc)
}

/// What happens after the steps passed.
enum Finish {
    Test,
    /// Store the password (or the sign-in) and add the account.
    Add(Account, Option<String>),
    /// Store the new sign-in of this account and resume its sync.
    Again(String),
}

enum Step {
    Next(String),
    Ok(String),
    Fail(String),
    /// The browser opened this link.
    Waiting(String),
    Signed(SignedIn),
}

fn run_checks(acc: &Account, secret: &str, tx: &async_channel::Sender<Step>) {
    let send = |s: Step| {
        let _ = tx.send_blocking(s);
    };
    let (i, s) = (&acc.imap, &acc.smtp);
    let imap_at = format!("IMAP {}:{} ({})", i.host, i.port, i.security.as_str());
    send(Step::Next(imap_at.clone()));
    match imap::connect(acc, secret) {
        Ok(mut session) => {
            let folders = session.list(Some(""), Some("*")).map(|n| n.len()).unwrap_or(0);
            let _ = session.logout();
            send(Step::Ok(format!("{imap_at}: logged in, {folders} folders")));
        }
        Err(e) => return send(Step::Fail(format!("{imap_at}: {e}"))),
    }
    let smtp_at = format!("SMTP {}:{} ({})", s.host, s.port, s.security.as_str());
    send(Step::Next(smtp_at.clone()));
    match smtp::test(acc, secret) {
        Ok(()) => send(Step::Ok(format!("{smtp_at}: logged in"))),
        Err(e) => send(Step::Fail(e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mx_lookups_only_for_unknown_domains() {
        assert_eq!(mx_domain("me@firma.ch").as_deref(), Some("firma.ch"));
        assert_eq!(mx_domain("Me@Firma.CH").as_deref(), Some("firma.ch"));
        assert_eq!(mx_domain("me@gmail.com"), None);
        assert_eq!(mx_domain("me@icloud.com"), None);
        assert_eq!(mx_domain("me@firma.c"), None);
        assert_eq!(mx_domain("me@firma"), None);
        assert_eq!(mx_domain("@firma.ch"), None);
    }
}
