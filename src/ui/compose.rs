//! The composer page. Sending runs on a thread; on failure the draft comes back with the error.

use super::{App, Mode, label};
use crate::imap::Cmd;
use crate::message;
use crate::smtp::{self, Draft};
use gtk::prelude::*;
use gtk::{gio, glib};
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;

pub struct Compose {
    pub root: gtk::Box,
    title: gtk::Label,
    from: gtk::DropDown,
    from_model: gtk::StringList,
    to: gtk::Entry,
    cc: gtk::Entry,
    bcc: gtk::Entry,
    subject: gtk::Entry,
    files_label: gtk::Label,
    suggest: gtk::Label,
    body: gtk::TextView,
    files: RefCell<Vec<PathBuf>>,
    in_reply_to: RefCell<String>,
    references: RefCell<String>,
    /// The message replied to, to flag it \Answered after sending.
    reply_of: RefCell<Option<(usize, String, u32)>>,
    esc_armed: Cell<bool>,
    pristine: RefCell<String>,
}

fn field(grid: &gtk::Grid, row: i32, name: &str, w: &impl IsA<gtk::Widget>) {
    let l = label(name, "form-label");
    l.set_width_chars(8);
    grid.attach(&l, 0, row, 1, 1);
    w.as_ref().set_hexpand(true);
    grid.attach(w, 1, row, 1, 1);
}

impl Compose {
    pub fn new() -> Compose {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.add_css_class("compose");
        let head = gtk::Box::new(gtk::Orientation::Vertical, 0);
        head.add_css_class("compose-head");
        let title = label("new message", "compose-title");
        head.append(&title);
        let grid = gtk::Grid::new();
        grid.set_row_spacing(6);
        let from_model = gtk::StringList::new(&[]);
        let from = gtk::DropDown::new(Some(from_model.clone()), gtk::Expression::NONE);
        from.set_halign(gtk::Align::Start);
        let entry = |ph: &str| {
            let e = gtk::Entry::new();
            e.add_css_class("field");
            e.set_placeholder_text(Some(ph));
            e
        };
        let to = entry("name@example.com, …");
        let cc = entry("");
        let bcc = entry("");
        let subject = entry("");
        field(&grid, 0, "From", &from);
        field(&grid, 1, "To", &to);
        field(&grid, 2, "Cc", &cc);
        field(&grid, 3, "Bcc", &bcc);
        field(&grid, 4, "Subject", &subject);
        head.append(&grid);
        let suggest = label("", "suggest");
        suggest.set_margin_top(4);
        head.append(&suggest);
        let files_label = label("", "r-attach");
        head.append(&files_label);
        root.append(&head);
        let body = gtk::TextView::new();
        body.add_css_class("editor");
        body.set_wrap_mode(gtk::WrapMode::WordChar);
        body.set_accepts_tab(false);
        let scroll = gtk::ScrolledWindow::builder().child(&body).vexpand(true).hscrollbar_policy(gtk::PolicyType::Never).build();
        root.append(&scroll);
        Compose {
            root,
            title,
            from,
            from_model,
            to,
            cc,
            bcc,
            subject,
            files_label,
            suggest,
            body,
            files: RefCell::new(Vec::new()),
            in_reply_to: RefCell::new(String::new()),
            references: RefCell::new(String::new()),
            reply_of: RefCell::new(None),
            esc_armed: Cell::new(false),
            pristine: RefCell::new(String::new()),
        }
    }

    pub fn wire(&self, app: &Rc<App>) {
        for e in [&self.to, &self.cc, &self.bcc] {
            let me = Rc::downgrade(app);
            e.connect_changed(move |e| {
                if let Some(app) = me.upgrade() {
                    app.update_suggest(e);
                }
            });
            let me = Rc::downgrade(app);
            let focus = gtk::EventControllerFocus::new();
            focus.connect_leave(move |_| {
                if let Some(app) = me.upgrade() {
                    app.compose.suggest.set_text("");
                }
            });
            e.add_controller(focus);
        }
    }

    fn state(&self) -> String {
        let b = self.body.buffer();
        format!("{}|{}|{}|{}|{}|{}", self.to.text(), self.cc.text(), self.bcc.text(), self.subject.text(), b.text(&b.start_iter(), &b.end_iter(), false), self.files.borrow().len())
    }

    fn draft(&self) -> Draft {
        let b = self.body.buffer();
        Draft {
            to: self.to.text().into(),
            cc: self.cc.text().into(),
            bcc: self.bcc.text().into(),
            subject: self.subject.text().into(),
            body: b.text(&b.start_iter(), &b.end_iter(), false).into(),
            in_reply_to: self.in_reply_to.borrow().clone(),
            references: self.references.borrow().clone(),
            attachments: self.files.borrow().clone(),
        }
    }

    fn update_files(&self) {
        let files = self.files.borrow();
        if files.is_empty() {
            self.files_label.set_visible(false);
            return;
        }
        let names: Vec<String> = files
            .iter()
            .map(|p| {
                let size = std::fs::metadata(p).map(|m| super::human_size(m.len() as usize)).unwrap_or_default();
                format!("📎 {} ({size})", p.file_name().unwrap_or_default().to_string_lossy())
            })
            .collect();
        self.files_label.set_text(&(names.join("   ") + "    Ctrl+Shift+O removes the last"));
        self.files_label.set_visible(true);
    }
}

/// The part after the last comma, which the suggestion completes.
fn last_part(text: &str) -> (&str, &str) {
    match text.rfind([',', ';']) {
        Some(i) => (&text[..=i], text[i + 1..].trim_start()),
        None => ("", text.trim_start()),
    }
}

impl App {
    fn open_compose(self: &Rc<Self>, acct: usize, d: Draft, title: &str) {
        let c = &self.compose;
        let accounts = self.accounts.borrow();
        if accounts.is_empty() {
            drop(accounts);
            self.toast("add an account first (A)", true);
            return;
        }
        let names: Vec<String> = accounts.iter().map(|a| format!("{} <{}>", a.from_name(), a.email)).collect();
        c.from_model.splice(0, c.from_model.n_items(), &names.iter().map(String::as_str).collect::<Vec<_>>());
        c.from.set_selected(acct.min(accounts.len() - 1) as u32);
        drop(accounts);
        c.title.set_text(title);
        c.to.set_text(&d.to);
        c.cc.set_text(&d.cc);
        c.bcc.set_text(&d.bcc);
        c.subject.set_text(&d.subject);
        c.body.buffer().set_text(&d.body);
        *c.files.borrow_mut() = d.attachments.clone();
        *c.in_reply_to.borrow_mut() = d.in_reply_to.clone();
        *c.references.borrow_mut() = d.references.clone();
        c.update_files();
        c.suggest.set_text("");
        c.esc_armed.set(false);
        *c.pristine.borrow_mut() = c.state();
        self.stack.set_visible_child_name("compose");
        self.set_mode(Mode::Compose);
        // write above the signature and the quote
        let buf = c.body.buffer();
        buf.place_cursor(&buf.start_iter());
        if d.to.is_empty() {
            c.to.grab_focus();
        } else {
            c.body.grab_focus();
        }
    }

    fn close_compose(self: &Rc<Self>) {
        self.toast("", false);
        self.stack.set_visible_child_name("main");
        self.set_mode(Mode::Normal);
        self.set_focus(self.focus.get());
    }

    fn current_acct(&self) -> usize {
        let from_row = self.current.borrow().as_ref().map(|(r, _)| r.acct.clone());
        let from_view = match &*self.view.borrow() {
            crate::store::View::Folder { acct, .. } => Some(acct.clone()),
            _ => None,
        };
        from_row.or(from_view).and_then(|a| self.acct_idx(&a)).unwrap_or(0)
    }

    fn signature(&self, acct: usize) -> String {
        let sig = self.accounts.borrow().get(acct).map(|a| a.signature.clone()).unwrap_or_default();
        if sig.is_empty() { String::new() } else { format!("\n\n{}", sig.replace("\\n", "\n")) }
    }

    pub fn compose_new(self: &Rc<Self>) {
        let acct = self.current_acct();
        *self.compose.reply_of.borrow_mut() = None;
        self.open_compose(acct, Draft { body: self.signature(acct), ..Default::default() }, "new message");
    }

    pub fn compose_reply(self: &Rc<Self>, all: bool) {
        let Some((row, Some(p))) = self.current.borrow().clone() else {
            self.toast("open a message first", true);
            return;
        };
        let acct = self.acct_idx(&row.acct).unwrap_or(0);
        let me = self.accounts.borrow().get(acct).map(|a| a.email.to_lowercase()).unwrap_or_default();
        let sender = if p.from.is_empty() || p.from == p.from_addr { p.from_addr.clone() } else { format!("{} <{}>", p.from, p.from_addr) };
        let not_me = |a: &String| !a.to_lowercase().contains(&me);
        let mut to: Vec<String> = if !p.reply_to.is_empty() { p.reply_to.clone() } else { vec![sender] };
        // replying to your own sent mail goes to its recipients
        if p.from_addr.to_lowercase() == me {
            to = p.to.clone();
        }
        let mut cc = Vec::new();
        if all {
            for a in p.to.iter().chain(&p.cc) {
                if not_me(a) && !to.contains(a) {
                    cc.push(a.clone());
                }
            }
        }
        let d = Draft {
            to: to.join(", "),
            cc: cc.join(", "),
            subject: message::prefixed("Re:", &p.subject),
            body: self.signature(acct) + &message::quote(&p),
            in_reply_to: p.msgid.clone(),
            references: p.references.clone(),
            ..Default::default()
        };
        *self.compose.reply_of.borrow_mut() = Some((acct, row.folder.clone(), row.uid));
        self.open_compose(acct, d, if all { "reply all" } else { "reply" });
    }

    pub fn compose_forward(self: &Rc<Self>) {
        let Some((row, Some(p))) = self.current.borrow().clone() else {
            self.toast("open a message first", true);
            return;
        };
        let acct = self.acct_idx(&row.acct).unwrap_or(0);
        // the attachments travel along, written to a private temp dir
        let dir = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir).join(format!("brief/fwd-{}", row.uid));
        let _ = std::fs::create_dir_all(&dir);
        let files: Vec<PathBuf> = p
            .attachments
            .iter()
            .filter_map(|a| {
                let path = dir.join(a.name.replace(['/', '\\'], "_"));
                std::fs::write(&path, &a.data).ok().map(|_| path)
            })
            .collect();
        let when = super::long_date(p.date);
        let body = format!(
            "{}\n\n---------- Forwarded message ----------\nFrom: {} <{}>\nDate: {when}\nSubject: {}\nTo: {}\n\n{}",
            self.signature(acct),
            p.from,
            p.from_addr,
            p.subject,
            p.to.join(", "),
            p.text
        );
        let d = Draft { subject: message::prefixed("Fwd:", &p.subject), body, attachments: files, ..Default::default() };
        *self.compose.reply_of.borrow_mut() = None;
        self.open_compose(acct, d, "forward");
    }

    fn update_suggest(&self, e: &gtk::Entry) {
        let text = e.text();
        let (_, part) = last_part(&text);
        if part.len() < 2 || part.contains('<') {
            self.compose.suggest.set_text("");
            return;
        }
        let found = self.store.borrow().contacts(part, 3);
        if found.is_empty() {
            self.compose.suggest.set_text("");
        } else {
            self.compose.suggest.set_text(&format!("Tab ⇥ {}", found.join("   ")));
        }
    }

    /// Tab in an address field with a suggestion completes it.
    fn accept_suggest(&self) -> bool {
        let focus = gtk::prelude::GtkWindowExt::focus(&self.win);
        let c = &self.compose;
        let Some(entry) = [&c.to, &c.cc, &c.bcc].into_iter().find(|e| focus.as_ref().is_some_and(|f| f.is_ancestor(*e))) else {
            return false;
        };
        let text = entry.text().to_string();
        let (head, part) = last_part(&text);
        if part.len() < 2 {
            return false;
        }
        let Some(first) = self.store.borrow().contacts(part, 1).into_iter().next() else { return false };
        let sep = if head.is_empty() { "" } else { " " };
        entry.set_text(&format!("{head}{sep}{first}, "));
        entry.set_position(-1);
        c.suggest.set_text("");
        true
    }

    pub fn compose_key(self: &Rc<Self>, k: &str) -> bool {
        let c = &self.compose;
        if k != "Escape" {
            c.esc_armed.set(false);
        }
        match k {
            "C-Return" | "C-KP_Enter" => self.send_draft(),
            "Escape" => {
                if c.state() == *c.pristine.borrow() || c.esc_armed.get() {
                    self.close_compose();
                } else {
                    c.esc_armed.set(true);
                    self.toast("Esc again to discard · Ctrl+S saves a draft", true);
                }
            }
            "C-s" => self.save_draft(),
            "C-e" => self.external_editor(),
            "C-o" => self.pick_files(),
            "C-S-o" => {
                c.files.borrow_mut().pop();
                c.update_files();
            }
            "Tab" => return self.accept_suggest(),
            "C-k" => self.open_palette_main(),
            _ => return false,
        }
        true
    }

    fn send_draft(self: &Rc<Self>) {
        let c = &self.compose;
        let d = c.draft();
        let acct_idx = c.from.selected() as usize;
        let Some(acc) = self.accounts.borrow().get(acct_idx).cloned() else { return };
        if let Err(e) = smtp::build(&acc, &d) {
            self.toast(&e, true);
            return;
        }
        let reply_of = c.reply_of.borrow().clone();
        self.close_compose();
        self.toast(&format!("sending to {}…", d.to), false);
        let (tx, rx) = async_channel::bounded(1);
        let send_d = d.clone();
        let send_acc = acc.clone();
        std::thread::spawn(move || {
            let _ = tx.send_blocking(smtp::send(&send_acc, &send_d));
        });
        let app = self.clone();
        glib::spawn_future_local(async move {
            match rx.recv().await {
                Ok(Ok(raw)) => {
                    app.toast(&format!("sent ✓  {}", d.subject), false);
                    if !smtp::server_saves_sent(&acc) {
                        app.send(acct_idx, Cmd::Append { role: "sent", raw });
                    }
                    if let Some((i, folder, uid)) = reply_of {
                        app.store.borrow().set_flag(&app.accounts.borrow()[i].name, &folder, uid, "answered", true);
                        app.send(i, Cmd::Flag { folder, uids: vec![uid], flag: "\\Answered", on: true });
                        app.schedule_reload();
                    }
                }
                Ok(Err(e)) => {
                    let title = format!("not sent: {e}");
                    app.open_compose(acct_idx, d, &title);
                    *app.compose.reply_of.borrow_mut() = reply_of;
                    app.toast(&title, true);
                }
                Err(_) => {}
            }
        });
    }

    fn save_draft(self: &Rc<Self>) {
        let c = &self.compose;
        let mut d = c.draft();
        let acct = c.from.selected() as usize;
        let Some(acc) = self.accounts.borrow().get(acct).cloned() else { return };
        if d.to.trim().is_empty() {
            d.to = acc.email.clone();
        }
        match smtp::build(&acc, &d) {
            Ok(m) => {
                if self.folder_with_role(acct, "drafts").is_none() {
                    self.toast(&format!("{} has no drafts folder", acc.name), true);
                    return;
                }
                self.send(acct, Cmd::Append { role: "drafts", raw: m.formatted() });
                *c.pristine.borrow_mut() = c.state();
                self.toast("draft saved", false);
            }
            Err(e) => self.toast(&e, true),
        }
    }

    /// Ctrl+E: the body in $EDITOR, in a terminal window, read back when it closes.
    fn external_editor(self: &Rc<Self>) {
        let dir = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir).join("brief");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("compose.eml.txt");
        let b = self.compose.body.buffer();
        if std::fs::write(&path, b.text(&b.start_iter(), &b.end_iter(), false).as_str()).is_err() {
            return;
        }
        let mut cmd = crate::platform::editor(&self.cfg.borrow().editor, &path);
        self.toast("editing in $EDITOR… save and quit to come back", false);
        let (tx, rx) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let ok = cmd.status().is_ok_and(|s| s.success());
            let _ = tx.send_blocking((ok, std::fs::read_to_string(&path).ok()));
        });
        let app = self.clone();
        glib::spawn_future_local(async move {
            if let Ok((ok, text)) = rx.recv().await {
                match text {
                    Some(t) if ok => {
                        app.compose.body.buffer().set_text(&t);
                        app.toast("", false);
                    }
                    _ => app.toast("editor failed; set `editor` in the config", true),
                }
                app.win.present();
                app.compose.body.grab_focus();
            }
        });
    }

    fn pick_files(self: &Rc<Self>) {
        let dialog = gtk::FileDialog::builder().title("Attach").modal(true).build();
        let app = self.clone();
        dialog.open_multiple(Some(&self.win), None::<&gio::Cancellable>, move |res| {
            if let Ok(list) = res {
                for i in 0..list.n_items() {
                    if let Some(path) = list.item(i).and_downcast::<gio::File>().and_then(|f| f.path()) {
                        app.compose.files.borrow_mut().push(path);
                    }
                }
                app.compose.update_files();
            }
        });
    }
}
