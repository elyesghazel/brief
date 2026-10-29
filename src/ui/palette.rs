//! Ctrl+K command palette (also the theme, folder, link, attachment and account pickers), the
//! confirm dialog for destructive commands and the `?` help.

use super::{App, Mode, display_folder, label, open_uri};
use crate::config;
use crate::store::View;
use crate::theme;
use gtk::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;

type Action = Rc<dyn Fn(&Rc<App>)>;

pub struct Item {
    pub title: String,
    pub sub: String,
    pub run: Action,
    /// Runs while the item is highlighted (theme preview).
    pub preview: Option<Action>,
}

fn item(title: impl Into<String>, sub: impl Into<String>, run: impl Fn(&Rc<App>) + 'static) -> Item {
    Item { title: title.into(), sub: sub.into(), run: Rc::new(run), preview: None }
}

/// A question before a destructive command: Enter or y does it, Esc or n backs out.
pub struct Confirm {
    pub root: gtk::Box,
    title: gtk::Label,
    text: gtk::Label,
    yes: gtk::Label,
    run: RefCell<Option<Action>>,
}

impl Confirm {
    pub fn new() -> Confirm {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.add_css_class("dialog");
        root.add_css_class("confirm");
        root.set_halign(gtk::Align::Center);
        root.set_valign(gtk::Align::Start);
        root.set_margin_top(110);
        root.set_size_request(520, -1);
        root.set_focusable(true);
        let title = label("", "dialog-title");
        root.append(&title);
        let text = label("", "confirm-text");
        text.set_wrap(true);
        text.set_max_width_chars(60);
        root.append(&text);
        let keys = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        keys.add_css_class("confirm-keys");
        let yes = label("", "confirm-yes");
        keys.append(&label("Enter / y", "help-key"));
        keys.append(&yes);
        keys.append(&label("Esc / n", "help-key"));
        keys.append(&label("cancel", "help-desc"));
        root.append(&keys);
        Confirm { root, title, text, yes, run: RefCell::new(None) }
    }
}

pub struct Palette {
    pub root: gtk::Box,
    title: gtk::Label,
    entry: gtk::Entry,
    list: gtk::ListBox,
    items: RefCell<Vec<Item>>,
    shown: RefCell<Vec<usize>>,
    on_cancel: RefCell<Option<Action>>,
}

/// Subsequence match; higher is better, None = no match. Word starts and runs score extra.
pub fn fuzzy(query: &str, text: &str) -> Option<i32> {
    if query.is_empty() {
        return Some(0);
    }
    let t: Vec<char> = text.to_lowercase().chars().collect();
    let mut score = 0;
    let mut ti = 0;
    let mut last: Option<usize> = None;
    for qc in query.to_lowercase().chars().filter(|c| !c.is_whitespace()) {
        let pos = (ti..t.len()).find(|&i| t[i] == qc)?;
        score += 1;
        if pos == 0 || !t[pos - 1].is_alphanumeric() {
            score += 6;
        }
        if last.is_some_and(|l| l + 1 == pos) {
            score += 4;
        }
        score -= (pos - ti).min(8) as i32 / 2;
        last = Some(pos);
        ti = pos + 1;
    }
    Some(score)
}

impl Palette {
    pub fn new() -> Palette {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.add_css_class("dialog");
        root.set_halign(gtk::Align::Center);
        root.set_valign(gtk::Align::Start);
        root.set_margin_top(110);
        root.set_size_request(640, -1);
        let title = label("", "dialog-title");
        root.append(&title);
        let prompt = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        prompt.add_css_class("prompt");
        prompt.append(&label("❯", "caret"));
        let entry = gtk::Entry::new();
        entry.add_css_class("query");
        entry.set_hexpand(true);
        prompt.append(&entry);
        root.append(&prompt);
        root.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        let list = gtk::ListBox::new();
        list.add_css_class("results");
        list.set_selection_mode(gtk::SelectionMode::Single);
        let scroll = gtk::ScrolledWindow::builder()
            .child(&list)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .max_content_height(420)
            .build();
        root.append(&scroll);
        Palette {
            root,
            title,
            entry,
            list,
            items: RefCell::new(Vec::new()),
            shown: RefCell::new(Vec::new()),
            on_cancel: RefCell::new(None),
        }
    }

    pub fn wire(&self, app: &Rc<App>) {
        let me = Rc::downgrade(app);
        self.entry.connect_changed(move |_| {
            if let Some(app) = me.upgrade() {
                app.pal.filter(&app);
            }
        });
        let me = Rc::downgrade(app);
        self.list.connect_row_activated(move |_, row| {
            if let Some(app) = me.upgrade() {
                app.palette_run(row.index() as usize);
            }
        });
    }

    fn filter(&self, app: &Rc<App>) {
        let q = self.entry.text().to_string();
        let items = self.items.borrow();
        let mut scored: Vec<(i32, usize)> =
            items.iter().enumerate().filter_map(|(i, it)| fuzzy(&q, &format!("{} {}", it.title, it.sub)).map(|s| (s, i))).collect();
        if !q.is_empty() {
            scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        }
        while let Some(c) = self.list.first_child() {
            self.list.remove(&c);
        }
        for (_, i) in scored.iter().take(80) {
            let it = &items[*i];
            let b = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            b.add_css_class("item");
            b.append(&label("›", "marker"));
            let t = label(&it.title, "title");
            t.set_ellipsize(gtk::pango::EllipsizeMode::End);
            b.append(&t);
            let s = label(&it.sub, "sub");
            s.set_hexpand(true);
            s.set_xalign(1.0);
            s.set_ellipsize(gtk::pango::EllipsizeMode::Start);
            b.append(&s);
            let row = gtk::ListBoxRow::new();
            row.set_child(Some(&b));
            row.set_focusable(false);
            self.list.append(&row);
        }
        *self.shown.borrow_mut() = scored.iter().take(80).map(|(_, i)| *i).collect();
        drop(items);
        // typing filters without previewing; moving previews
        self.select(app, 0, false);
    }

    fn select(&self, app: &Rc<App>, i: usize, preview: bool) {
        if let Some(row) = self.list.row_at_index(i as i32) {
            self.list.select_row(Some(&row));
            row.grab_focus();
            self.entry.grab_focus_without_selecting();
            let idx = self.shown.borrow().get(i).copied();
            let preview_on = preview;
            let preview = idx.and_then(|i| self.items.borrow().get(i).and_then(|it| it.preview.clone()));
            if let Some(p) = preview.filter(|_| preview_on) {
                p(app);
            }
        }
    }
}

impl App {
    fn open_palette(self: &Rc<Self>, title: &str, items: Vec<Item>, query: &str, on_cancel: Option<Action>) {
        self.pal.title.set_text(title);
        *self.pal.items.borrow_mut() = items;
        *self.pal.on_cancel.borrow_mut() = on_cancel;
        self.show_overlay(self.pal.root.upcast_ref());
        self.set_mode(Mode::Palette);
        self.pal.entry.set_text(query);
        self.pal.filter(self);
        self.pal.entry.grab_focus();
        self.pal.entry.set_position(-1);
    }

    fn palette_run(self: &Rc<Self>, row: usize) {
        let idx = self.pal.shown.borrow().get(row).copied();
        let Some(idx) = idx else { return };
        let run = self.pal.items.borrow()[idx].run.clone();
        *self.pal.on_cancel.borrow_mut() = None;
        self.hide_overlay();
        run(self);
    }

    pub fn palette_key(self: &Rc<Self>, k: &str) -> bool {
        let cur = self.pal.list.selected_row().map(|r| r.index()).unwrap_or(0).max(0) as usize;
        let n = self.pal.shown.borrow().len();
        match k {
            "Escape" | "C-c" | "C-g" => {
                let cancel = self.pal.on_cancel.borrow_mut().take();
                self.hide_overlay();
                if let Some(c) = cancel {
                    c(self);
                }
            }
            "Return" => self.palette_run(cur),
            "Down" | "C-j" | "C-n" | "Tab" => self.pal.select(self, if n == 0 { 0 } else { (cur + 1) % n }, true),
            "Up" | "C-k" | "C-p" | "S-Tab" => self.pal.select(self, if n == 0 { 0 } else { (cur + n - 1) % n }, true),
            "Page_Down" => self.pal.select(self, (cur + 8).min(n.saturating_sub(1)), true),
            "Page_Up" => self.pal.select(self, cur.saturating_sub(8), true),
            _ => return false,
        }
        true
    }

    /// Asks before `run`; `verb` names what Enter does.
    pub fn confirm(self: &Rc<Self>, title: &str, text: &str, verb: &str, run: impl Fn(&Rc<App>) + 'static) {
        let c = &self.confirm;
        c.title.set_text(title);
        c.text.set_text(text);
        c.yes.set_text(verb);
        *c.run.borrow_mut() = Some(Rc::new(run));
        self.show_overlay(c.root.upcast_ref());
        self.set_mode(Mode::Confirm);
        c.root.grab_focus();
    }

    pub fn confirm_key(self: &Rc<Self>, k: &str) -> bool {
        match k {
            "Return" | "y" | "Y" => {
                let run = self.confirm.run.borrow_mut().take();
                self.hide_overlay();
                if let Some(run) = run {
                    run(self);
                }
            }
            "Escape" | "n" | "N" | "q" | "C-c" | "C-g" => {
                self.confirm.run.borrow_mut().take();
                self.hide_overlay();
                self.toast("cancelled", false);
            }
            // nothing else gets through while the question is up
            _ => {}
        }
        true
    }

    pub fn open_palette_main(self: &Rc<Self>) {
        let mut items = vec![
            item("Compose", "c", |a| a.compose_new()),
            item("Reply", "r", |a| a.compose_reply(false)),
            item("Reply all", "R", |a| a.compose_reply(true)),
            item("Forward", "f", |a| a.compose_forward()),
            item("Refresh all accounts", "Ctrl+R", |a| a.refresh_all()),
            item("Archive", "e", |a| a.move_to_role("archive", "archived")),
            item("Delete", "d", |a| a.delete()),
            item("Mark as spam", "!", |a| a.move_to_role("junk", "marked as spam")),
            item("Star / unstar", "s", |a| a.toggle_star()),
            item("Read / unread", "u", |a| a.toggle_read()),
            item("Mark all as read", "U", |a| a.mark_all_read()),
            item("Move to…", "m", |a| a.open_palette_move()),
            item("Select all", "*", |a| a.mark_all()),
            item("Search", "/", |a| {
                a.set_mode(Mode::Search);
                a.search.grab_focus();
            }),
            item("Open in browser", "o", |a| a.open_in_browser(false)),
            item("Open in browser with remote images", "O", |a| a.open_in_browser(true)),
            item("Links in this message…", "L", |a| a.open_palette_links()),
            item("Attachments…", "a", |a| a.open_palette_attachments()),
            item("Switch HTML / plain text", "H", |a| {
                a.html_as_text.set(!a.html_as_text.get());
                a.rerender();
            }),
            item(
                if self.threads() { "Conversations off (one row per message)" } else { "Conversations on (group replies)" },
                "",
                |a| a.toggle_threads(),
            ),
            item("Theme…", "t", |a| a.open_palette_themes()),
            item("Add account…", "A", |a| a.open_form()),
            item("Remove account…", "", |a| a.open_palette_remove_account()),
            item("Open config file", "", |_| open_uri(&config::path().to_string_lossy())),
            item("All inboxes", "0", |a| a.open_view(View::Unified)),
            item("Keyboard shortcuts", "?", |a| a.show_help()),
            item("Quit", "q", |a| a.win.close()),
        ];
        items.extend(self.folder_items(|a, acct, folder| a.open_view(View::Folder { acct, folder }), "Go to "));
        self.open_palette("commands", items, "", None);
    }

    fn folder_items(self: &Rc<Self>, run: impl Fn(&Rc<App>, String, String) + Clone + 'static, prefix: &str) -> Vec<Item> {
        let mut out = Vec::new();
        for acc in self.accounts.borrow().iter() {
            for f in self.store.borrow().folders(&acc.name) {
                let (acct, folder) = (acc.name.clone(), f.name.clone());
                let run = run.clone();
                out.push(item(format!("{prefix}{}", display_folder(&f)), acc.name.clone(), move |a| run(a, acct.clone(), folder.clone())));
            }
        }
        out
    }

    pub fn open_palette_move(self: &Rc<Self>) {
        let rows = self.targets();
        let Some(first) = rows.first() else { return };
        let acct = first.acct.clone();
        let Some(i) = self.acct_idx(&acct) else { return };
        let name = self.accounts.borrow()[i].name.clone();
        let items: Vec<Item> = self
            .store
            .borrow()
            .folders(&name)
            .into_iter()
            .filter(|f| f.name != first.folder)
            .map(|f| {
                let rows: Vec<_> = rows.iter().filter(|r| r.acct == acct).cloned().collect();
                let dest = f.name.clone();
                item(display_folder(&f), crate::imap::decode_utf7(&f.name), move |a| a.move_to(rows.clone(), dest.clone()))
            })
            .collect();
        self.open_palette(&format!("move {} message(s) to", rows.len()), items, "", None);
    }

    pub fn open_palette_themes(self: &Rc<Self>) {
        let before = self.theme_name.borrow().clone();
        let items: Vec<Item> = theme::names()
            .into_iter()
            .map(|name| {
                let n1 = name.clone();
                let n2 = name.clone();
                let sub = if name == before { "current" } else if name == "zunder" { "follow zunder" } else { "" };
                Item {
                    title: name.clone(),
                    sub: sub.into(),
                    run: Rc::new(move |a: &Rc<App>| {
                        a.apply_theme(&n1);
                        config::save_theme(&n1);
                        a.toast(&format!("theme: {n1}"), false);
                    }),
                    preview: Some(Rc::new(move |a: &Rc<App>| a.apply_theme(&n2))),
                }
            })
            .collect();
        let revert: Action = Rc::new(move |a: &Rc<App>| a.apply_theme(&before));
        self.open_palette("theme (live preview, Enter keeps it)", items, "", Some(revert));
        // start on the current theme
        let cur = self.theme_name.borrow().clone();
        let pos = self.pal.shown.borrow().iter().position(|&i| self.pal.items.borrow()[i].title == cur);
        if let Some(p) = pos {
            self.pal.select(self, p, false);
        }
    }

    pub fn open_palette_remove_account(self: &Rc<Self>) {
        if self.stack.visible_child_name().as_deref() == Some("compose") {
            self.toast("close the composer first", true);
            return;
        }
        let items: Vec<Item> = self
            .accounts
            .borrow()
            .iter()
            .enumerate()
            .map(|(i, acc)| {
                let (name, email) = (acc.name.clone(), acc.email.clone());
                item(format!("Remove {name}"), email.clone(), move |a| {
                    let text = format!(
                        "Takes {email} out of brief: its block in config.toml, its saved password and its cached mail. \
                         The mail on the server is not touched; add the account again with A."
                    );
                    a.confirm(&format!("remove {name}?"), &text, "remove", move |a| a.remove_account(i));
                })
            })
            .collect();
        if items.is_empty() {
            self.toast("no accounts", false);
            return;
        }
        self.open_palette("remove which account", items, "", None);
    }

    pub fn open_palette_links(self: &Rc<Self>) {
        let links = self.links.borrow().clone();
        if links.is_empty() {
            self.toast("no links in this message", false);
            return;
        }
        let items = links
            .into_iter()
            .enumerate()
            .map(|(i, l)| {
                let u = l.clone();
                item(l, format!("{}", i + 1), move |a| a.open_link(&u))
            })
            .collect();
        self.open_palette("open link", items, "", None);
    }

    pub fn open_palette_attachments(self: &Rc<Self>) {
        let Some((_, Some(p))) = self.current.borrow().clone() else { return };
        if p.attachments.is_empty() {
            self.toast("no attachments", false);
            return;
        }
        let mut items = Vec::new();
        for (i, a) in p.attachments.iter().enumerate() {
            let size = super::human_size(a.size);
            items.push(item(format!("Open {}", a.name), format!("{} · {size}", a.mime), move |app| app.attachment_action(i, false)));
            items.push(item(format!("Save {}", a.name), "to ~/Downloads", move |app| app.attachment_action(i, true)));
        }
        if p.attachments.len() > 1 {
            let n = p.attachments.len();
            items.push(item("Save all", "to ~/Downloads", move |app| {
                for i in 0..n {
                    app.attachment_action(i, true);
                }
            }));
        }
        self.open_palette("attachments", items, "", None);
    }
}

pub fn help_dialog() -> gtk::Box {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.add_css_class("dialog");
    root.set_halign(gtk::Align::Center);
    root.set_valign(gtk::Align::Center);
    root.set_focusable(true);
    root.append(&label("keyboard", "dialog-title"));
    let cols = gtk::FlowBox::new();
    cols.set_selection_mode(gtk::SelectionMode::None);
    cols.set_max_children_per_line(3);
    cols.set_column_spacing(40);
    cols.set_row_spacing(10);
    cols.set_homogeneous(false);
    cols.set_can_focus(false);
    cols.set_margin_start(24);
    cols.set_margin_end(24);
    cols.set_margin_bottom(10);
    let groups: &[&[(&str, &str)]] = &[
        &[
            ("#Move", ""),
            ("j / k", "next / previous"),
            ("J / K", "next / previous row, any pane"),
            ("n / p", "next / previous in a conversation"),
            ("h / l, Tab", "switch pane"),
            ("Enter", "read message"),
            ("gg / G", "top / bottom"),
            ("Ctrl+d / Ctrl+u", "half page"),
            ("Space", "page down in the message"),
            ("[ / ]", "previous / next folder"),
            ("0, 1-9", "all inboxes, account inbox"),
            ("gi gs gd ga gt gj", "inbox sent drafts archive trash spam"),
            ("#Find", ""),
            ("/", "search (from: to: subject: is:unread has:attachment)"),
            ("Enter in search", "also search the server"),
        ],
        &[
            ("#Messages", ""),
            ("c", "compose"),
            ("r / R / f", "reply / reply all / forward"),
            ("e", "archive"),
            ("d", "delete"),
            ("!", "spam"),
            ("s", "star"),
            ("u", "read / unread"),
            ("U", "mark all as read"),
            ("m", "move to folder"),
            ("x / *", "select / select all"),
            ("o / O", "open in browser (O loads images)"),
            ("H", "HTML / plain text"),
            ("L / a", "links / attachments"),
        ],
        &[
            ("#App", ""),
            ("Ctrl+R", "refresh every account"),
            ("Ctrl+K or :", "all commands"),
            ("t", "theme"),
            ("A", "add account"),
            ("q / Ctrl+Q", "quit"),
            ("#Compose", ""),
            ("Ctrl+Enter", "send"),
            ("Ctrl+E", "edit in $EDITOR"),
            ("Ctrl+O", "attach file"),
            ("Ctrl+S", "save draft"),
            ("Tab", "accept address suggestion"),
            ("Esc", "close (twice to discard)"),
        ],
    ];
    for g in groups {
        let grid = gtk::Grid::new();
        grid.set_row_spacing(3);
        let mut r = 0;
        for (k, d) in g.iter() {
            if let Some(title) = k.strip_prefix('#') {
                let l = label(title, "help-group");
                grid.attach(&l, 0, r, 2, 1);
            } else {
                grid.attach(&label(k, "help-key"), 0, r, 1, 1);
                grid.attach(&label(d, "help-desc"), 1, r, 1, 1);
            }
            r += 1;
        }
        cols.insert(&grid, -1);
    }
    root.append(&cols);
    root
}

#[cfg(test)]
mod tests {
    use super::fuzzy;

    #[test]
    fn fuzzy_prefers_word_starts() {
        assert!(fuzzy("ra", "Reply all").unwrap() > fuzzy("ra", "Search archive").unwrap_or(-99));
        assert!(fuzzy("xyz", "Reply").is_none());
        assert_eq!(fuzzy("", "x"), Some(0));
    }
}
