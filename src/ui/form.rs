//! `A`: add an account. Servers are guessed from the address, the login is tested over IMAP and
//! SMTP before anything is saved, the password goes to the keyring (or secrets.toml, 0600).

use super::{App, Mode, label};
use crate::config::{self, Account, Auth, OAuth, Security, Server};
use crate::{imap, smtp};
use gtk::glib;
use gtk::prelude::*;
use std::cell::Cell;
use std::rc::Rc;

pub struct Form {
    pub root: gtk::Box,
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
    status: gtk::Label,
    /// The user changed a server field; stop overwriting them from the address.
    touched: Rc<Cell<bool>>,
    busy: Cell<bool>,
}

impl Form {
    pub fn new() -> Form {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.add_css_class("dialog");
        root.set_halign(gtk::Align::Center);
        root.set_valign(gtk::Align::Center);
        root.set_size_request(620, -1);
        root.append(&label("add account", "dialog-title"));
        let grid = gtk::Grid::new();
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
        let rows: [(&str, &gtk::Widget); 11] = [
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
        for (i, (l, w)) in rows.iter().enumerate() {
            let rowbox = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            rowbox.add_css_class("form-row");
            let lab = label(l, "form-label");
            lab.set_width_chars(11);
            rowbox.append(&lab);
            rowbox.append(*w);
            grid.attach(&rowbox, 0, i as i32, 1, 1);
        }
        root.append(&grid);
        let status = label("", "form-hint");
        status.set_wrap(true);
        root.append(&status);
        Form {
            root,
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
            status,
            touched: Rc::new(Cell::new(false)),
            busy: Cell::new(false),
        }
    }

    pub fn wire(&self, _app: &Rc<App>) {
        let (ih, ip, is, sh, sp, ss, name, touched) = (
            self.imap_host.clone(),
            self.imap_port.clone(),
            self.imap_sec.clone(),
            self.smtp_host.clone(),
            self.smtp_port.clone(),
            self.smtp_sec.clone(),
            self.name.clone(),
            self.touched.clone(),
        );
        let filling = Rc::new(Cell::new(false));
        let f2 = filling.clone();
        self.email.connect_changed(move |e| {
            let text = e.text();
            let domain = text.rsplit('@').next().unwrap_or("").to_owned();
            if !name.has_focus() && text.contains('@') {
                let label = domain.split('.').next().unwrap_or("").to_owned();
                let mut c = label.chars();
                let cap = c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default();
                if name.text().is_empty() || name.widget_name() == "auto" {
                    name.set_text(&cap);
                    name.set_widget_name("auto");
                }
            }
            if touched.get() || !text.contains('@') {
                return;
            }
            let (i, s) = config::guess_servers(&text);
            f2.set(true);
            ih.set_text(&i.host);
            ip.set_text(&i.port.to_string());
            is.set_text(i.security.as_str());
            sh.set_text(&s.host);
            sp.set_text(&s.port.to_string());
            ss.set_text(s.security.as_str());
            f2.set(false);
        });
        let n2 = self.name.clone();
        self.name.connect_changed(move |e| {
            if e.has_focus() {
                n2.set_widget_name("");
            }
        });
        for e in [&self.imap_host, &self.imap_port, &self.imap_sec, &self.smtp_host, &self.smtp_port, &self.smtp_sec] {
            let touched = self.touched.clone();
            let filling = filling.clone();
            e.connect_changed(move |_| {
                if !filling.get() {
                    touched.set(true);
                }
            });
        }
    }

    pub fn open(&self, app: &Rc<App>) {
        for e in [&self.email, &self.name, &self.display, &self.user, &self.imap_host, &self.imap_port, &self.imap_sec, &self.smtp_host, &self.smtp_port, &self.smtp_sec] {
            e.set_text("");
        }
        self.pass.set_text("");
        self.touched.set(false);
        self.status.set_text("Gmail / iCloud / Outlook / Yahoo need an app password. Ctrl+T tests the servers, Ctrl+Enter tests and adds the account.");
        self.status.set_css_classes(&["form-hint"]);
        app.show_overlay(self.root.upcast_ref());
        app.set_mode(Mode::Form);
        self.email.grab_focus();
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
}

impl App {
    pub fn form_key(self: &Rc<Self>, k: &str) -> bool {
        match k {
            "Escape" => {
                if !self.form.busy.get() {
                    self.hide_overlay();
                }
            }
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
            return self.form.set_status("enter the password", "form-error");
        }
        self.form.busy.set(true);
        self.form.set_status(&format!("IMAP {}:{}…", acc.imap.host, acc.imap.port), "form-hint");
        let (tx, rx) = async_channel::unbounded::<Step>();
        let (a2, p2) = (acc.clone(), pass.clone());
        std::thread::spawn(move || run_checks(&a2, &p2, &tx));
        let app = self.clone();
        glib::spawn_future_local(async move {
            let mut lines: Vec<String> = Vec::new();
            let mut failed = false;
            while let Ok(step) = rx.recv().await {
                match step {
                    Step::Ok(l) => lines.push(format!("✓ {l}")),
                    Step::Next(l) => {
                        let mut text = lines.join("\n");
                        text += &format!("\n{l}…");
                        app.form.set_status(text.trim_start(), "form-hint");
                        continue;
                    }
                    Step::Fail(l) => {
                        lines.push(format!("✗ {l}"));
                        failed = true;
                    }
                }
                app.form.set_status(&lines.join("\n"), if failed { "form-error" } else { "form-hint" });
            }
            app.form.busy.set(false);
            if failed {
                return;
            }
            if !save {
                lines.push("Ctrl+Enter adds the account".into());
                app.form.set_status(&lines.join("\n"), "form-ok");
                return;
            }
            let mut acc = acc;
            if config::keyring_store(&acc.email, &pass) {
                acc.password_cmd = crate::platform::keyring_cmd(&acc.email);
            } else if let Err(e) = config::secrets_file_store(&acc.email, &pass) {
                app.form.set_status(&format!("could not store the password: {e}"), "form-error");
                return;
            }
            if let Err(e) = config::append_account(&acc) {
                app.form.set_status(&format!("could not write the config: {e}"), "form-error");
                return;
            }
            app.form.set_status("✓ connected", "form-ok");
            app.hide_overlay();
            app.add_account(acc);
        });
    }
}

enum Step {
    Next(String),
    Ok(String),
    Fail(String),
}

fn run_checks(acc: &Account, pass: &str, tx: &async_channel::Sender<Step>) {
    let send = |s: Step| {
        let _ = tx.send_blocking(s);
    };
    let (i, s) = (&acc.imap, &acc.smtp);
    let imap_at = format!("IMAP {}:{} ({})", i.host, i.port, i.security.as_str());
    match imap::connect(acc, pass) {
        Ok(mut session) => {
            let folders = session.list(Some(""), Some("*")).map(|n| n.len()).unwrap_or(0);
            let _ = session.logout();
            send(Step::Ok(format!("{imap_at}: logged in, {folders} folders")));
        }
        Err(e) => return send(Step::Fail(format!("{imap_at}: {e}"))),
    }
    let smtp_at = format!("SMTP {}:{} ({})", s.host, s.port, s.security.as_str());
    send(Step::Next(smtp_at.clone()));
    match smtp::test(acc, pass) {
        Ok(()) => send(Step::Ok(format!("{smtp_at}: logged in"))),
        Err(e) => send(Step::Fail(e)),
    }
}
