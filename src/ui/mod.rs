//! The window: top bar, sidebar (accounts + folders), message list, reader, status bar, and the
//! overlays (palette, help, account form) above them. The composer is a second page of the stack.

mod compose;
mod form;
mod keys;
mod palette;
mod reader;
mod web;
mod widgets;

use crate::config::{self, Account, Config};
use crate::imap::{self, Cmd, Event};
use crate::message::{self, Parsed};
use crate::store::{Folder, Row, Store, View};
use crate::theme;
use gtk::prelude::*;
use gtk::{gio, glib};
use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::mpsc::Sender;
use std::time::Duration;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Pane {
    Side,
    List,
    Reader,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Normal,
    Search,
    Palette,
    Help,
    Form,
    Compose,
}

#[derive(Clone, Debug, PartialEq)]
enum SideItem {
    Unified,
    Header(usize),
    Folder(usize, String),
}

/// Key of a message across accounts.
pub type Key = (String, String, u32);

fn key(r: &Row) -> Key {
    (r.acct.clone(), r.folder.clone(), r.uid)
}

pub struct App {
    pub win: gtk::ApplicationWindow,
    pub cfg: RefCell<Config>,
    pub accounts: RefCell<Vec<Account>>,
    pub workers: RefCell<Vec<Sender<Cmd>>>,
    pub store: RefCell<Store>,
    events: imap::EventTx,
    theme_css: gtk::CssProvider,
    font_css: gtk::CssProvider,
    pub theme_name: RefCell<String>,

    stack: gtk::Stack,
    // top bar
    path: gtk::Label,
    pub search: gtk::Entry,
    sync_label: gtk::Label,
    spinner: gtk::Spinner,
    refresh: gtk::Button,
    sync_state: RefCell<Vec<(bool, String, bool)>>,
    // panes
    side_pane: gtk::Box,
    list_pane: gtk::Box,
    reader_pane: gtk::Box,
    side: gtk::ListBox,
    side_items: RefCell<Vec<SideItem>>,
    list_model: gio::ListStore,
    list_sel: gtk::SingleSelection,
    list_view: gtk::ListView,
    list_stack: gtk::Stack,
    empty_title: gtk::Label,
    empty_text: gtk::Label,
    pub rows: RefCell<Vec<Row>>,
    pub view: RefCell<View>,
    pub marked: RefCell<HashSet<Key>>,
    reload_pending: Cell<bool>,
    /// Set while the model is rebuilt, so selection churn does not touch the reader.
    quiet: Cell<bool>,
    rebind_pending: Cell<bool>,
    pub rd: reader::Reader,
    pub icons: widgets::Icons,
    acct_css: gtk::CssProvider,
    pub current: RefCell<Option<(Row, Option<Rc<Parsed>>)>>,
    pub html_as_text: Cell<bool>,
    pub links: RefCell<Vec<String>>,
    // status bar
    mode_label: gtk::Label,
    hints: gtk::Box,
    toast: gtk::Label,
    toast_gen: Rc<Cell<u32>>,

    pub focus: Cell<Pane>,
    pub mode: Cell<Mode>,
    pub pending: RefCell<String>,

    overlay_scrim: gtk::Box,
    pal: palette::Palette,
    help: gtk::Box,
    form: form::Form,
    pub compose: compose::Compose,
}

fn label(text: &str, class: &str) -> gtk::Label {
    let l = gtk::Label::new(Some(text));
    l.set_xalign(0.0);
    if !class.is_empty() {
        for c in class.split(' ') {
            l.add_css_class(c);
        }
    }
    l
}

fn ellipsize(l: &gtk::Label) -> &gtk::Label {
    l.set_ellipsize(gtk::pango::EllipsizeMode::End);
    l.set_hexpand(true);
    l.set_width_chars(1);
    l
}

pub fn display_folder(f: &Folder) -> String {
    match f.role.as_str() {
        "inbox" => "Inbox".into(),
        "sent" => "Sent".into(),
        "drafts" => "Drafts".into(),
        "trash" => "Trash".into(),
        "junk" => "Spam".into(),
        "archive" => "Archive".into(),
        "all" => "All mail".into(),
        "flagged" => "Starred".into(),
        _ => {
            let leaf = if f.delim.is_empty() { f.name.as_str() } else { f.name.rsplit(f.delim.as_str()).next().unwrap_or(&f.name) };
            imap::decode_utf7(leaf)
        }
    }
}

/// "14:05" today, "Tue" this week, "27 Oct" this year, "27.10.25" before.
pub fn short_date(ts: i64) -> String {
    use chrono::{Datelike, Local, TimeZone};
    if ts <= 0 {
        return String::new();
    }
    let Some(d) = Local.timestamp_opt(ts, 0).single() else { return String::new() };
    let now = Local::now();
    if d.date_naive() == now.date_naive() {
        d.format("%H:%M").to_string()
    } else if (now - d).num_days() < 6 && d < now {
        d.format("%a").to_string()
    } else if d.year() == now.year() {
        d.format("%-d %b").to_string()
    } else {
        d.format("%d.%m.%y").to_string()
    }
}

fn long_date(ts: i64) -> String {
    use chrono::{Local, TimeZone};
    if ts <= 0 {
        return String::new();
    }
    Local.timestamp_opt(ts, 0).single().map(|d| d.format("%a, %-d %b %Y  %H:%M").to_string()).unwrap_or_default()
}

pub use crate::platform::{notify, open as open_uri};

/// Finds http(s) URLs and mailto: links in text, as byte ranges.
pub fn find_links(text: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < text.len() {
        let rest = &text[i..];
        let next = ["https://", "http://", "mailto:"].iter().filter_map(|p| rest.find(p)).min();
        let Some(start) = next else { break };
        let start = i + start;
        let end = text[start..]
            .find(|c: char| c.is_whitespace() || "<>\"'`]".contains(c))
            .map_or(text.len(), |e| start + e);
        let mut end = end;
        while end > start && text[..end].ends_with(['.', ',', ')', ';', ':', '!', '?']) {
            if text[..end].ends_with(')') && text[start..end].contains('(') {
                break;
            }
            end -= 1;
        }
        if end - start > 8 {
            out.push((start, end));
        }
        i = end.max(start + 1);
    }
    out
}

pub fn build(app: &gtk::Application) {
    let cfg = match config::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("brief: {e}");
            let mut c = Config::default();
            c.accounts.clear();
            notify("brief: config error", &e);
            c
        }
    };
    let store = Store::open().expect("open cache");
    let (tx, rx) = async_channel::unbounded::<Event>();

    let win = gtk::ApplicationWindow::builder().application(app).title("brief").default_width(1360).default_height(860).build();
    win.add_css_class("brief");
    win.set_decorated(cfg.decorations);

    let display = gtk::prelude::WidgetExt::display(&win);
    let base = gtk::CssProvider::new();
    base.load_from_string(include_str!("../style.css"));
    gtk::style_context_add_provider_for_display(&display, &base, gtk::STYLE_PROVIDER_PRIORITY_USER);
    let theme_css = gtk::CssProvider::new();
    gtk::style_context_add_provider_for_display(&display, &theme_css, gtk::STYLE_PROVIDER_PRIORITY_USER + 1);
    let font_css = gtk::CssProvider::new();
    gtk::style_context_add_provider_for_display(&display, &font_css, gtk::STYLE_PROVIDER_PRIORITY_USER + 2);
    let user_css = config::dir().join("style.css");
    if user_css.exists() {
        let p = gtk::CssProvider::new();
        p.load_from_path(&user_css);
        gtk::style_context_add_provider_for_display(&display, &p, gtk::STYLE_PROVIDER_PRIORITY_USER + 3);
    }

    // ── top bar
    let topbar = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    topbar.add_css_class("topbar");
    let brand = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    brand.add_css_class("brand-box");
    brand.append(&widgets::emoji("unified", 20));
    brand.append(&label("brief", "brand"));
    topbar.append(&brand);
    let path = label("", "path");
    path.set_use_markup(true);
    topbar.append(&path);
    let search = gtk::Entry::new();
    search.add_css_class("search");
    search.set_primary_icon_name(Some("system-search-symbolic"));
    search.set_placeholder_text(Some("Search   /"));
    search.set_hexpand(true);
    search.set_max_width_chars(48);
    search.set_tooltip_text(Some("from:  to:  subject:  is:unread  is:starred  has:attachment - Enter also searches the server"));
    let search_box = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    search_box.set_hexpand(true);
    search_box.set_halign(gtk::Align::Center);
    search_box.append(&search);
    topbar.append(&search_box);
    let spinner = gtk::Spinner::new();
    spinner.set_margin_end(6);
    topbar.append(&spinner);
    let sync_label = label("", "sync");
    topbar.append(&sync_label);
    let refresh = widgets::icon_button("view-refresh-symbolic", "Refresh", "Fetch new mail on every account (Ctrl+R)");
    refresh.add_css_class("bar");
    topbar.append(&refresh);
    let compose_btn = widgets::icon_button("mail-message-new-symbolic", "New message", "Write a new message (c)");
    compose_btn.add_css_class("bar");
    compose_btn.add_css_class("primary");
    topbar.append(&compose_btn);
    let menu_btn = widgets::icon_button("open-menu-symbolic", "", "All commands (Ctrl+K)");
    menu_btn.add_css_class("bar");
    menu_btn.add_css_class("square");
    topbar.append(&menu_btn);

    // ── sidebar
    let side = gtk::ListBox::new();
    side.add_css_class("side");
    side.set_selection_mode(gtk::SelectionMode::Single);
    let side_scroll = gtk::ScrolledWindow::builder().child(&side).hscrollbar_policy(gtk::PolicyType::Never).vexpand(true).build();
    let side_pane = gtk::Box::new(gtk::Orientation::Vertical, 0);
    side_pane.add_css_class("pane");
    side_pane.append(&side_scroll);
    side_pane.set_size_request(190, -1);

    // ── message list
    let list_model = gio::ListStore::new::<glib::BoxedAnyObject>();
    let list_sel = gtk::SingleSelection::new(Some(list_model.clone()));
    list_sel.set_autoselect(false);
    list_sel.set_can_unselect(true);
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().unwrap();
        // [dot] [avatar] [name ... flags date / subject / preview]
        let root = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        root.add_css_class("msg");
        root.append(&label("●", "msg-dot"));
        // the account shows as a small colored badge on the avatar in the unified inbox
        let av = widgets::Avatar::new(36);
        let face = gtk::Overlay::new();
        face.set_child(Some(&av.root));
        face.set_valign(gtk::Align::Start);
        let badge = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        badge.add_css_class("acct-badge");
        badge.set_halign(gtk::Align::End);
        badge.set_valign(gtk::Align::End);
        badge.set_visible(false);
        face.add_overlay(&badge);
        root.append(&face);
        let text = gtk::Box::new(gtk::Orientation::Vertical, 0);
        text.set_hexpand(true);
        text.add_css_class("msg-text");
        let top = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        top.append(ellipsize(&label("", "msg-from")));
        top.append(&label("", "msg-flags"));
        top.append(&label("", "msg-date"));
        text.append(&top);
        text.append(ellipsize(&label("", "msg-subject")));
        text.append(ellipsize(&label("", "msg-preview")));
        root.append(&text);
        item.set_child(Some(&root));
    });
    let list_view = gtk::ListView::new(Some(list_sel.clone()), Some(factory.clone()));
    list_view.add_css_class("msgs");
    list_view.set_single_click_activate(false);
    let list_scroll = gtk::ScrolledWindow::builder().child(&list_view).hscrollbar_policy(gtk::PolicyType::Never).vexpand(true).build();
    let empty = gtk::Box::new(gtk::Orientation::Vertical, 0);
    empty.add_css_class("empty");
    empty.set_valign(gtk::Align::Center);
    let empty_title = label("", "empty-title");
    let empty_text = label("", "");
    empty_title.set_xalign(0.5);
    empty_text.set_xalign(0.5);
    empty_text.set_justify(gtk::Justification::Center);
    empty_text.set_wrap(true);
    empty.append(&empty_title);
    empty.append(&empty_text);
    let list_stack = gtk::Stack::new();
    list_stack.add_named(&list_scroll, Some("list"));
    list_stack.add_named(&empty, Some("empty"));
    let list_pane = gtk::Box::new(gtk::Orientation::Vertical, 0);
    list_pane.add_css_class("pane");
    list_pane.append(&list_stack);
    list_pane.set_size_request(300, -1);

    // ── reader
    let rd = reader::Reader::new();
    let reader_pane = gtk::Box::new(gtk::Orientation::Vertical, 0);
    reader_pane.add_css_class("pane");
    reader_pane.append(&rd.root);

    let inner = gtk::Paned::new(gtk::Orientation::Horizontal);
    inner.set_start_child(Some(&list_pane));
    inner.set_end_child(Some(&reader_pane));
    inner.set_position(470);
    inner.set_shrink_start_child(false);
    inner.set_shrink_end_child(false);
    let outer = gtk::Paned::new(gtk::Orientation::Horizontal);
    outer.set_start_child(Some(&side_pane));
    outer.set_end_child(Some(&inner));
    outer.set_position(230);
    outer.set_shrink_start_child(false);
    outer.set_vexpand(true);

    // ── status bar
    let statusbar = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    statusbar.add_css_class("statusbar");
    let mode_label = label("NORMAL", "mode");
    statusbar.append(&mode_label);
    let hints = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    statusbar.append(&hints);
    let toast = label("", "toast");
    toast.set_hexpand(true);
    toast.set_xalign(1.0);
    toast.set_ellipsize(gtk::pango::EllipsizeMode::Start);
    statusbar.append(&toast);

    let main = gtk::Box::new(gtk::Orientation::Vertical, 0);
    main.append(&topbar);
    main.append(&outer);

    let compose = compose::Compose::new();
    let stack = gtk::Stack::new();
    stack.set_transition_type(gtk::StackTransitionType::Crossfade);
    stack.set_transition_duration(90);
    stack.add_named(&main, Some("main"));
    stack.add_named(&compose.root, Some("compose"));

    stack.set_vexpand(true);
    let shell = gtk::Box::new(gtk::Orientation::Vertical, 0);
    shell.append(&stack);
    shell.append(&statusbar);
    let overlay = gtk::Overlay::new();
    overlay.set_child(Some(&shell));
    let scrim = gtk::Box::new(gtk::Orientation::Vertical, 0);
    scrim.add_css_class("scrim");
    scrim.set_visible(false);
    overlay.add_overlay(&scrim);
    let pal = palette::Palette::new();
    let help = palette::help_dialog();
    let form = form::Form::new();
    for w in [&pal.root, &help, &form.root] {
        w.set_visible(false);
        scrim.append(w);
    }
    win.set_child(Some(&overlay));

    let fetch = (cfg.favicons != "off").then(|| {
        let tx = tx.clone();
        crate::avatar::spawn(cfg.favicons.clone(), move |d| {
            let _ = tx.send_blocking(Event::Icon(d));
        })
    });
    let icons = widgets::Icons::new(fetch);
    let acct_css = gtk::CssProvider::new();
    gtk::style_context_add_provider_for_display(&display, &acct_css, gtk::STYLE_PROVIDER_PRIORITY_USER + 2);

    let n = cfg.accounts.len();
    let theme_name = cfg.theme.clone();
    let app = Rc::new(App {
        win: win.clone(),
        accounts: RefCell::new(cfg.accounts.clone()),
        cfg: RefCell::new(cfg),
        workers: RefCell::new(Vec::new()),
        store: RefCell::new(store),
        events: tx,
        theme_css,
        font_css,
        theme_name: RefCell::new(theme_name),
        stack,
        path,
        search,
        sync_label,
        spinner,
        refresh: refresh.clone(),
        sync_state: RefCell::new(vec![(false, String::new(), false); n]),
        side_pane,
        list_pane,
        reader_pane,
        side,
        side_items: RefCell::new(Vec::new()),
        list_model,
        list_sel,
        list_view,
        list_stack,
        empty_title,
        empty_text,
        rows: RefCell::new(Vec::new()),
        view: RefCell::new(View::Unified),
        marked: RefCell::new(HashSet::new()),
        reload_pending: Cell::new(false),
        quiet: Cell::new(false),
        rebind_pending: Cell::new(false),
        rd,
        icons,
        acct_css,
        current: RefCell::new(None),
        html_as_text: Cell::new(false),
        links: RefCell::new(Vec::new()),
        mode_label,
        hints,
        toast,
        toast_gen: Rc::new(Cell::new(0)),
        focus: Cell::new(Pane::List),
        mode: Cell::new(Mode::Normal),
        pending: RefCell::new(String::new()),
        overlay_scrim: scrim,
        pal,
        help,
        form,
        compose,
    });

    let start_theme = app.theme_name.borrow().clone();
    app.apply_theme(&start_theme);
    app.apply_font();
    app.apply_acct_css();
    app.wire(&factory, &compose_btn, &menu_btn);
    app.rd.wire(&app);
    app.compose.wire(&app);
    app.form.wire(&app);
    app.pal.wire(&app);
    keys::install(&app);

    {
        let app = app.clone();
        glib::spawn_future_local(async move {
            while let Ok(ev) = rx.recv().await {
                app.on_event(ev);
            }
        });
    }

    app.start_workers();
    app.rebuild_side();
    if let Some(start) = app.side_items.borrow().iter().position(|i| *i == SideItem::Unified)
        && let Some(row) = app.side.row_at_index(start as i32) {
            app.side.select_row(Some(&row));
        }
    let first_view = if app.accounts.borrow().len() == 1 {
        app.inbox_of(0).map(|f| View::Folder { acct: app.accounts.borrow()[0].name.clone(), folder: f }).unwrap_or(View::Unified)
    } else {
        View::Unified
    };
    app.open_view(first_view);
    app.set_focus(Pane::List);
    app.set_mode(Mode::Normal);
    win.present();
    if app.accounts.borrow().is_empty() {
        app.open_form();
    }
}

impl App {
    fn wire(self: &Rc<Self>, factory: &gtk::SignalListItemFactory, compose_btn: &gtk::Button, menu_btn: &gtk::Button) {
        let me = Rc::downgrade(self);
        factory.connect_bind(move |_, item| {
            let Some(app) = me.upgrade() else { return };
            let item = item.downcast_ref::<gtk::ListItem>().unwrap();
            let Some(obj) = item.item().and_downcast::<glib::BoxedAnyObject>() else { return };
            let row = obj.borrow::<Row>();
            let root = item.child().and_downcast::<gtk::Box>().unwrap();
            app.bind_row(&root, &row);
        });

        let me = Rc::downgrade(self);
        self.list_sel.connect_selected_notify(move |sel| {
            let Some(app) = me.upgrade() else { return };
            if app.quiet.get() {
                return;
            }
            let i = sel.selected();
            let row = app.rows.borrow().get(i as usize).cloned();
            match row {
                Some(r) => app.show_message(r),
                None => app.clear_reader(),
            }
        });
        let me = Rc::downgrade(self);
        self.list_view.connect_activate(move |_, _| {
            if let Some(app) = me.upgrade() {
                app.set_focus(Pane::Reader);
            }
        });

        let me = Rc::downgrade(self);
        self.side.connect_row_activated(move |_, row| {
            let Some(app) = me.upgrade() else { return };
            app.activate_side(row.index() as usize);
        });

        let me = Rc::downgrade(self);
        self.refresh.connect_clicked(move |_| {
            if let Some(app) = me.upgrade() {
                app.refresh_all();
            }
        });
        let me = Rc::downgrade(self);
        compose_btn.connect_clicked(move |_| {
            if let Some(app) = me.upgrade() {
                app.compose_new();
            }
        });
        let me = Rc::downgrade(self);
        menu_btn.connect_clicked(move |_| {
            if let Some(app) = me.upgrade() {
                app.open_palette_main();
            }
        });

        // search: filter as you type, Enter also asks the server
        let me = Rc::downgrade(self);
        self.search.connect_changed(move |_| {
            if let Some(app) = me.upgrade() {
                app.schedule_reload();
            }
        });
        let me = Rc::downgrade(self);
        self.search.connect_activate(move |e| {
            let Some(app) = me.upgrade() else { return };
            let q = e.text().to_string();
            if let View::Folder { acct, folder } = app.view.borrow().clone()
                && let Some(i) = app.acct_idx(&acct) {
                    app.send(i, Cmd::Search { folder, query: q });
                }
            app.set_mode(Mode::Normal);
            app.set_focus(Pane::List);
            app.select_index(0);
        });
        let focus = gtk::EventControllerFocus::new();
        let me = Rc::downgrade(self);
        focus.connect_enter(move |_| {
            if let Some(app) = me.upgrade()
                && app.mode.get() == Mode::Normal {
                    app.set_mode(Mode::Search);
                }
        });
        self.search.add_controller(focus);

        let me = Rc::downgrade(self);
        self.win.connect_is_active_notify(move |w| {
            if w.is_active()
                && let Some(app) = me.upgrade() {
                    app.update_title();
                }
        });
    }

    // ── workers ──────────────────────────────────────────────

    fn start_workers(&self) {
        let cfg = self.cfg.borrow();
        let mut workers = self.workers.borrow_mut();
        for (i, acc) in self.accounts.borrow().iter().enumerate().skip(workers.len()) {
            workers.push(imap::spawn(i, acc.clone(), self.events.clone(), cfg.cache_limit, cfg.sync_minutes));
        }
        let n = self.accounts.borrow().len();
        self.sync_state.borrow_mut().resize(n, (false, String::new(), false));
    }

    /// Per-account colors (dot in the sidebar, stripe in the unified list) as CSS classes.
    fn apply_acct_css(&self) {
        let mut css = String::new();
        for (i, a) in self.accounts.borrow().iter().enumerate().take(16) {
            let c = a.color.trim();
            let ok = c.starts_with('#') && c.len() <= 9 && c[1..].chars().all(|x| x.is_ascii_hexdigit());
            let color = if ok { c.to_owned() } else { ["@accent", "@green", "@yellow", "@red"][i % 4].to_owned() };
            css += &format!(".acct-dot-{i} {{ background: {color}; }}\n");
        }
        self.acct_css.load_from_string(&css);
    }

    /// Re-binds every row (new sender icons), keeping the selection.
    fn schedule_rebind(self: &Rc<Self>) {
        if self.rebind_pending.replace(true) {
            return;
        }
        let me = Rc::downgrade(self);
        glib::timeout_add_local_once(Duration::from_millis(250), move || {
            let Some(app) = me.upgrade() else { return };
            app.rebind_pending.set(false);
            let objs: Vec<glib::BoxedAnyObject> = app.rows.borrow().iter().cloned().map(glib::BoxedAnyObject::new).collect();
            let sel = app.list_sel.selected();
            app.quiet.set(true);
            app.list_model.splice(0, app.list_model.n_items(), &objs);
            app.list_sel.set_selected(sel);
            app.quiet.set(false);
            app.refresh_reader_avatar();
        });
    }

    pub fn add_account(self: &Rc<Self>, acc: Account) {
        self.accounts.borrow_mut().push(acc);
        self.apply_acct_css();
        self.start_workers();
        self.rebuild_side();
        let idx = self.accounts.borrow().len() - 1;
        self.toast(&format!("added {}, syncing…", self.accounts.borrow()[idx].name), false);
    }

    pub fn send(&self, acct: usize, cmd: Cmd) {
        if let Some(w) = self.workers.borrow().get(acct) {
            let _ = w.send(cmd);
        }
    }

    pub fn acct_idx(&self, name: &str) -> Option<usize> {
        self.accounts.borrow().iter().position(|a| a.name == name)
    }

    pub fn folder_with_role(&self, acct: usize, role: &str) -> Option<String> {
        let name = self.accounts.borrow().get(acct)?.name.clone();
        self.store.borrow().folders(&name).into_iter().find(|f| f.role == role).map(|f| f.name)
    }

    fn inbox_of(&self, acct: usize) -> Option<String> {
        self.folder_with_role(acct, "inbox").or_else(|| self.accounts.borrow().get(acct).map(|_| "INBOX".to_owned()))
    }

    /// The button and Ctrl+R: sync every account now, the open folder first.
    pub fn refresh_all(self: &Rc<Self>) {
        if self.accounts.borrow().is_empty() {
            self.toast("no accounts yet, press A to add one", true);
            return;
        }
        if let View::Folder { acct, folder } = self.view.borrow().clone()
            && let Some(i) = self.acct_idx(&acct) {
                self.send(i, Cmd::Sync(folder));
            }
        for i in 0..self.accounts.borrow().len() {
            self.send(i, Cmd::SyncAll { manual: true });
            if let Some(s) = self.sync_state.borrow_mut().get_mut(i) {
                s.0 = true;
            }
        }
        self.update_sync();
    }

    fn on_event(self: &Rc<Self>, ev: Event) {
        match ev {
            Event::Status { acct, busy, text } => {
                if let Some(s) = self.sync_state.borrow_mut().get_mut(acct) {
                    let error = text.starts_with("offline");
                    *s = (busy, text, error);
                }
                self.update_sync();
            }
            Event::Error { acct, text } => {
                let name = self.accounts.borrow().get(acct).map(|a| a.name.clone()).unwrap_or_default();
                self.toast(&format!("{name}: {text}"), true);
            }
            Event::Folders => {
                self.rebuild_side();
                self.update_title();
            }
            Event::Folder { acct, folder } => {
                let name = self.accounts.borrow().get(acct).map(|a| a.name.clone()).unwrap_or_default();
                let relevant = match &*self.view.borrow() {
                    View::Unified => self.folder_with_role(acct, "inbox").as_deref() == Some(folder.as_str()),
                    View::Folder { acct: a, folder: f } => *a == name && *f == folder,
                };
                if relevant {
                    self.schedule_reload();
                }
            }
            Event::Body { acct, folder, uid } => {
                let name = self.accounts.borrow().get(acct).map(|a| a.name.clone()).unwrap_or_default();
                let cur = self.current.borrow().as_ref().map(|(r, _)| key(r));
                if cur == Some((name, folder, uid)) {
                    let row = self.current.borrow().as_ref().map(|(r, _)| r.clone()).unwrap();
                    self.show_message(row);
                }
            }
            Event::NewMail { acct, from, subject, count } => {
                let name = self.accounts.borrow().get(acct).map(|a| a.name.clone()).unwrap_or_default();
                if self.cfg.borrow().notify && !self.win.is_active() {
                    let title = if count > 1 { format!("{count} new · {name}") } else { format!("{from} · {name}") };
                    notify(&title, &subject);
                }
                self.toast(&format!("new mail from {from}"), false);
                self.update_title();
            }
            Event::Icon(domain) => {
                self.icons.arrived(&domain);
                self.schedule_rebind();
            }
            Event::SearchDone { found, .. } => {
                self.toast(&format!("{found} matches on the server"), false);
                self.schedule_reload();
            }
        }
    }

    fn update_sync(&self) {
        let st = self.sync_state.borrow();
        let accounts = self.accounts.borrow();
        let busy: Vec<String> = st
            .iter()
            .enumerate()
            .filter(|(_, s)| s.0)
            .map(|(i, s)| format!("{}: {}", accounts.get(i).map(|a| a.name.as_str()).unwrap_or(""), s.1))
            .collect();
        let error = st.iter().enumerate().find(|(_, s)| s.2).map(|(i, _)| accounts.get(i).map(|a| a.name.clone()).unwrap_or_default());
        self.sync_label.remove_css_class("busy");
        self.sync_label.remove_css_class("error");
        if !busy.is_empty() {
            self.spinner.start();
            self.sync_label.add_css_class("busy");
            self.sync_label.set_text(&busy.join("  "));
        } else {
            self.spinner.stop();
            if let Some(name) = error {
                self.sync_label.add_css_class("error");
                self.sync_label.set_text(&format!("{name} offline · ^R retry"));
            } else {
                let last = st.iter().filter_map(|s| s.1.strip_prefix("synced ")).max().unwrap_or("");
                self.sync_label.set_text(&if last.is_empty() { String::new() } else { format!("✓ {last}") });
            }
        }
        self.refresh.set_sensitive(busy.is_empty());
    }

    pub fn toast(&self, text: &str, error: bool) {
        self.toast.set_text(text);
        if error {
            self.toast.add_css_class("error");
        } else {
            self.toast.remove_css_class("error");
        }
        let g = self.toast_gen.get() + 1;
        self.toast_gen.set(g);
        let toast = self.toast.clone();
        let gen_cell = self.toast_gen.clone();
        glib::timeout_add_local_once(Duration::from_secs(if error { 9 } else { 5 }), move || {
            // a newer toast keeps its own timer
            if gen_cell.get() == g {
                toast.set_text("");
            }
        });
    }

    fn update_title(&self) {
        let n = self.store.borrow().unified_unseen();
        self.win.set_title(Some(&if n > 0 { format!("brief ({n})") } else { "brief".into() }));
    }

    // ── theme ────────────────────────────────────────────────

    pub fn apply_theme(&self, name: &str) {
        self.theme_css.load_from_string(&theme::css(name));
        *self.theme_name.borrow_mut() = name.to_owned();
        // tags recolor text already in the buffer
        self.setup_tags();
    }

    pub fn apply_font(&self) {
        let cfg = self.cfg.borrow();
        // emoji fonts are left out on purpose: named here, Pango takes digits from them too
        // (Apple's has blank ones); fontconfig fallback finds them anyway, see platform::apple_emoji
        let mut families: Vec<String> = Vec::new();
        let mut generic = "sans-serif";
        for f in cfg.font.split(',').map(str::trim).filter(|f| !f.is_empty() && !f.contains("Emoji")) {
            match f {
                "monospace" | "sans-serif" | "serif" => generic = f,
                f => families.push(format!("\"{f}\"")),
            }
        }
        families.push(generic.to_owned());
        let css = format!(
            "window.brief {{ font-family: {}; font-size: {}pt; }}",
            families.join(", "),
            cfg.font_size
        );
        self.font_css.load_from_string(&css);
    }

    // ── mode and focus ───────────────────────────────────────

    pub fn set_mode(&self, mode: Mode) {
        self.mode.set(mode);
        let (text, insert) = match mode {
            Mode::Normal => ("NORMAL", false),
            Mode::Search => ("SEARCH", true),
            Mode::Palette => ("COMMAND", true),
            Mode::Help => ("HELP", false),
            Mode::Form => ("ACCOUNT", true),
            Mode::Compose => ("COMPOSE", true),
        };
        self.mode_label.set_text(text);
        if insert {
            self.mode_label.add_css_class("insert");
        } else {
            self.mode_label.remove_css_class("insert");
        }
        self.update_hints();
    }

    fn update_hints(&self) {
        while let Some(c) = self.hints.first_child() {
            self.hints.remove(&c);
        }
        let list: &[(&str, &str)] = match (self.mode.get(), self.focus.get()) {
            (Mode::Search, _) => &[("Enter", "search server"), ("Esc", "back")],
            (Mode::Palette, _) => &[("↑↓", "move"), ("Enter", "run"), ("Esc", "close")],
            (Mode::Help, _) => &[("Esc", "close")],
            (Mode::Form, _) => &[("Tab", "next"), ("Ctrl+T", "test"), ("Ctrl+Enter", "test & add"), ("Esc", "cancel")],
            (Mode::Compose, _) => &[("Ctrl+Enter", "send"), ("Ctrl+E", "editor"), ("Ctrl+O", "attach"), ("Esc", "close")],
            (Mode::Normal, Pane::Side) => &[("j k", "folders"), ("l", "messages"), ("?", "keys")],
            (Mode::Normal, Pane::List) => &[("j k", "move"), ("Enter", "read"), ("c", "write"), ("Ctrl+K", "commands"), ("?", "keys")],
            (Mode::Normal, Pane::Reader) => &[("j k", "scroll"), ("r", "reply"), ("I", "load images"), ("h", "back"), ("?", "keys")],
        };
        for (k, d) in list {
            self.hints.append(&label(k, "key"));
            self.hints.append(&label(d, "desc"));
        }
    }

    pub fn set_focus(&self, pane: Pane) {
        self.focus.set(pane);
        for (p, w) in [(Pane::Side, &self.side_pane), (Pane::List, &self.list_pane), (Pane::Reader, &self.reader_pane)] {
            if p == pane {
                w.add_css_class("focused");
            } else {
                w.remove_css_class("focused");
            }
        }
        match pane {
            Pane::Side => {
                self.side.grab_focus();
            }
            Pane::List => {
                self.list_view.grab_focus();
            }
            Pane::Reader => {
                // keys stay with the window either way; this just takes focus off the list
                self.rd.body.grab_focus();
            }
        }
        self.update_hints();
    }

    // ── sidebar ──────────────────────────────────────────────

    pub fn rebuild_side(&self) {
        let selected = self.side.selected_row().and_then(|r| self.side_items.borrow().get(r.index() as usize).cloned());
        while let Some(c) = self.side.first_child() {
            self.side.remove(&c);
        }
        let mut items = Vec::new();
        let store = self.store.borrow();
        let accounts = self.accounts.borrow();
        let item_row = |icon: &str, name: &str, unseen: u32, depth: usize| {
            let b = gtk::Box::new(gtk::Orientation::Horizontal, 10);
            b.add_css_class("side-item");
            b.set_margin_start((depth * 16) as i32);
            b.append(&widgets::emoji(icon, 18));
            b.append(ellipsize(&label(name, "side-name")));
            if unseen > 0 {
                let pill = label(&unseen.to_string(), "pill");
                pill.set_valign(gtk::Align::Center);
                b.append(&pill);
            }
            b
        };
        if accounts.len() > 1 {
            let row = gtk::ListBoxRow::new();
            row.set_child(Some(&item_row("unified", "All inboxes", store.unified_unseen(), 0)));
            row.add_css_class("side-unified");
            self.side.append(&row);
            items.push(SideItem::Unified);
        }
        for (i, acc) in accounts.iter().enumerate() {
            let head = gtk::ListBoxRow::new();
            head.set_selectable(false);
            head.set_activatable(false);
            let hb = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            hb.add_css_class("side-acct");
            let dot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            dot.add_css_class("acct-dot");
            dot.add_css_class(&format!("acct-dot-{}", i.min(15)));
            dot.set_valign(gtk::Align::Center);
            hb.append(&dot);
            let names = gtk::Box::new(gtk::Orientation::Vertical, 0);
            names.append(ellipsize(&label(&acc.name, "side-acct-name")));
            names.append(ellipsize(&label(&acc.email, "side-acct-mail")));
            hb.append(&names);
            if i < 9 {
                let k = label(&(i + 1).to_string(), "side-key");
                k.set_valign(gtk::Align::Center);
                hb.append(&k);
            }
            head.set_child(Some(&hb));
            self.side.append(&head);
            items.push(SideItem::Header(i));
            let folders = store.folders(&acc.name);
            if folders.is_empty() {
                let row = gtk::ListBoxRow::new();
                row.set_selectable(false);
                row.set_activatable(false);
                let st = self.sync_state.borrow();
                let text = st.get(i).map(|s| s.1.clone()).filter(|t| !t.is_empty()).unwrap_or_else(|| "connecting…".into());
                let l = label(&text, "side-name");
                l.add_css_class("side-item");
                ellipsize(&l);
                row.set_child(Some(&l));
                self.side.append(&row);
                items.push(SideItem::Header(i));
            }
            for f in folders {
                let depth = if f.role.is_empty() && !f.delim.is_empty() { f.name.matches(f.delim.as_str()).count().min(4) } else { 0 };
                let depth = if f.name.starts_with("INBOX") && f.role.is_empty() { depth.max(1) } else { depth };
                let row = gtk::ListBoxRow::new();
                let unseen = if matches!(f.role.as_str(), "trash" | "junk" | "sent" | "drafts") { 0 } else { f.unseen };
                let icon = if f.role.is_empty() { "folder" } else { f.role.as_str() };
                row.set_child(Some(&item_row(icon, &display_folder(&f), unseen, depth)));
                self.side.append(&row);
                items.push(SideItem::Folder(i, f.name.clone()));
            }
        }
        drop(store);
        drop(accounts);
        *self.side_items.borrow_mut() = items;
        // keep the selection on the current view
        let want = match &*self.view.borrow() {
            View::Unified => Some(SideItem::Unified),
            View::Folder { acct, folder } => self.acct_idx(acct).map(|i| SideItem::Folder(i, folder.clone())),
        };
        let want = want.or(selected);
        if let Some(pos) = want.and_then(|w| self.side_items.borrow().iter().position(|i| *i == w))
            && let Some(row) = self.side.row_at_index(pos as i32) {
                self.side.select_row(Some(&row));
            }
    }

    fn activate_side(self: &Rc<Self>, idx: usize) {
        let item = self.side_items.borrow().get(idx).cloned();
        match item {
            Some(SideItem::Unified) => self.open_view(View::Unified),
            Some(SideItem::Folder(i, name)) => {
                let acct = self.accounts.borrow()[i].name.clone();
                self.open_view(View::Folder { acct, folder: name });
            }
            _ => {}
        }
    }

    /// Moves the sidebar selection by `delta` selectable rows and opens it.
    pub fn side_step(self: &Rc<Self>, delta: i32) {
        let items = self.side_items.borrow().clone();
        let cur = self.side.selected_row().map(|r| r.index()).unwrap_or(-1);
        let mut i = cur;
        loop {
            i += delta.signum();
            if i < 0 || i as usize >= items.len() {
                return;
            }
            if !matches!(items[i as usize], SideItem::Header(_)) {
                break;
            }
        }
        if let Some(row) = self.side.row_at_index(i) {
            self.side.select_row(Some(&row));
            self.activate_side(i as usize);
        }
    }

    pub fn open_view(self: &Rc<Self>, view: View) {
        if let View::Folder { acct, folder } = &view
            && let Some(i) = self.acct_idx(acct) {
                self.send(i, Cmd::Sync(folder.clone()));
            }
        let changed = *self.view.borrow() != view;
        *self.view.borrow_mut() = view;
        if changed {
            self.marked.borrow_mut().clear();
            self.search.set_text("");
        }
        self.reload_list();
        self.update_path();
        if changed {
            self.select_index(0);
        }
        self.rebuild_side_selection();
    }

    fn rebuild_side_selection(&self) {
        let want = match &*self.view.borrow() {
            View::Unified => Some(SideItem::Unified),
            View::Folder { acct, folder } => self.acct_idx(acct).map(|i| SideItem::Folder(i, folder.clone())),
        };
        if let Some(pos) = want.and_then(|w| self.side_items.borrow().iter().position(|i| *i == w))
            && let Some(row) = self.side.row_at_index(pos as i32) {
                self.side.select_row(Some(&row));
            }
    }

    fn update_path(&self) {
        let text = match &*self.view.borrow() {
            View::Unified => "<b>All inboxes</b>".to_owned(),
            View::Folder { acct, folder } => {
                let f = self.store.borrow().folders(acct).into_iter().find(|f| f.name == *folder);
                let name = f.map(|f| display_folder(&f)).unwrap_or_else(|| imap::decode_utf7(folder));
                format!("{} › <b>{}</b>", glib::markup_escape_text(acct), glib::markup_escape_text(&name))
            }
        };
        self.path.set_markup(&text);
    }

    // ── message list ─────────────────────────────────────────

    pub fn schedule_reload(self: &Rc<Self>) {
        if self.reload_pending.replace(true) {
            return;
        }
        let me = Rc::downgrade(self);
        glib::timeout_add_local_once(Duration::from_millis(60), move || {
            if let Some(app) = me.upgrade() {
                app.reload_pending.set(false);
                app.reload_list();
            }
        });
    }

    pub fn reload_list(&self) {
        let query = self.search.text().to_string();
        let limit = self.cfg.borrow().cache_limit.max(200) * if *self.view.borrow() == View::Unified { 2 } else { 1 };
        let rows = self.store.borrow().list(&self.view.borrow(), &query, limit);
        let selected = self.rows.borrow().get(self.list_sel.selected() as usize).map(key);
        let same = {
            let old = self.rows.borrow();
            old.len() == rows.len()
                && old.iter().zip(&rows).all(|(a, b)| {
                    key(a) == key(b) && a.seen == b.seen && a.flagged == b.flagged && a.preview == b.preview && a.attach == b.attach
                })
        };
        if same {
            self.update_empty();
            return;
        }
        let objs: Vec<glib::BoxedAnyObject> = rows.iter().cloned().map(glib::BoxedAnyObject::new).collect();
        *self.rows.borrow_mut() = rows;
        let new_pos = selected.and_then(|k| self.rows.borrow().iter().position(|r| key(r) == k));
        self.quiet.set(true);
        self.list_model.splice(0, self.list_model.n_items(), &objs);
        self.list_sel.set_selected(new_pos.map_or(gtk::INVALID_LIST_POSITION, |p| p as u32));
        self.quiet.set(false);
        if new_pos.is_none() && self.current.borrow().is_some() {
            // the open message left this view (moved or deleted elsewhere)
            let still = self.current.borrow().as_ref().is_some_and(|(r, _)| self.rows.borrow().iter().any(|x| key(x) == key(r)));
            if !still {
                self.clear_reader();
            }
        }
        self.update_empty();
    }

    fn update_empty(&self) {
        let rows = self.rows.borrow();
        if !rows.is_empty() {
            self.list_stack.set_visible_child_name("list");
            return;
        }
        self.list_stack.set_visible_child_name("empty");
        if self.accounts.borrow().is_empty() {
            self.empty_title.set_text("No accounts yet");
            self.empty_text.set_text("Press A to add one.\nAny IMAP/SMTP server works: Gmail, Outlook, iCloud, your own.");
        } else if !self.search.text().is_empty() {
            self.empty_title.set_text("No matches");
            self.empty_text.set_text("Enter searches the whole folder on the server.");
        } else if self.sync_state.borrow().iter().any(|s| s.0) {
            self.empty_title.set_text("Syncing…");
            self.empty_text.set_text("");
        } else {
            self.empty_title.set_text("All clear");
            self.empty_text.set_text("Nothing in here. Ctrl+R to check again.");
        }
    }

    fn bind_row(&self, root: &gtk::Box, r: &Row) {
        let unified = *self.view.borrow() == View::Unified;
        let children = |w: &gtk::Widget| {
            let mut v = Vec::new();
            let mut c = w.first_child();
            while let Some(x) = c {
                c = x.next_sibling();
                v.push(x);
            }
            v
        };
        let kids = children(root.upcast_ref());
        let face = kids[1].downcast_ref::<gtk::Overlay>().unwrap();
        let av = widgets::Avatar::from_widget(face.child().and_downcast_ref::<gtk::Stack>().unwrap());
        let badge = face.last_child().and_downcast::<gtk::Box>().unwrap();
        let text = children(&kids[2]);
        let top: Vec<gtk::Label> = children(&text[0]).into_iter().map(|w| w.downcast::<gtk::Label>().unwrap()).collect();
        let outgoing = r.folder_is_outgoing();
        let (name, addr) = if outgoing {
            let to = r.to.split(',').next().unwrap_or("").trim().to_owned();
            let addr = to.rsplit_once('<').map(|(_, a)| a.trim_end_matches('>').to_owned()).unwrap_or_else(|| to.clone());
            let name = to.split_once('<').map(|(n, _)| n.trim().trim_matches('"').to_owned()).unwrap_or_default();
            (name, addr)
        } else {
            (r.from_name.clone(), r.from_addr.clone())
        };
        av.set(&self.icons, &name, &addr);
        let shown = if name.is_empty() { addr.clone() } else { name.clone() };
        top[0].set_text(&if outgoing { format!("To {shown}") } else { shown });
        let mut flags = String::new();
        if r.answered {
            flags += "↩ ";
        }
        if r.attach {
            flags += "📎 ";
        }
        if r.flagged {
            flags += "⭐ ";
        }
        top[1].set_text(flags.trim_end());
        top[1].set_visible(!flags.is_empty());
        top[2].set_text(&short_date(r.date));
        text[1].downcast_ref::<gtk::Label>().unwrap().set_text(if r.subject.is_empty() { "(no subject)" } else { &r.subject });
        let prev = text[2].downcast_ref::<gtk::Label>().unwrap();
        prev.set_text(&r.preview);
        prev.set_visible(!r.preview.is_empty());
        root.set_css_classes(&["msg"]);
        if !r.seen {
            root.add_css_class("unread");
        }
        if self.marked.borrow().contains(&key(r)) {
            root.add_css_class("marked");
        }
        badge.set_css_classes(&["acct-badge"]);
        match self.acct_idx(&r.acct).filter(|_| unified) {
            Some(i) => {
                badge.add_css_class(&format!("acct-dot-{}", i.min(15)));
                badge.set_tooltip_text(Some(&r.acct));
                badge.set_visible(true);
            }
            None => badge.set_visible(false),
        }
    }

    /// Re-binds rows after a local change (flags, marks). ListView only rebinds a new object, so
    /// each row gets one and the selection is put back quietly.
    pub fn refresh_rows(&self, keys: &[Key]) {
        let sel = self.list_sel.selected();
        self.quiet.set(true);
        let rows = self.rows.borrow();
        for k in keys {
            let Some(pos) = rows.iter().position(|r| key(r) == *k) else { continue };
            self.list_model.splice(pos as u32, 1, &[glib::BoxedAnyObject::new(rows[pos].clone())]);
        }
        if sel != gtk::INVALID_LIST_POSITION && self.list_sel.selected() != sel {
            self.list_sel.set_selected(sel);
        }
        self.quiet.set(false);
    }

    pub fn selected_index(&self) -> Option<usize> {
        let s = self.list_sel.selected();
        (s != gtk::INVALID_LIST_POSITION).then_some(s as usize)
    }

    pub fn select_index(&self, i: usize) {
        let n = self.rows.borrow().len();
        if n == 0 {
            self.clear_reader();
            return;
        }
        let i = i.min(n - 1) as u32;
        self.list_sel.set_selected(i);
        self.list_view.scroll_to(i, gtk::ListScrollFlags::NONE, None);
    }

    pub fn list_step(&self, delta: i64) {
        let n = self.rows.borrow().len() as i64;
        if n == 0 {
            return;
        }
        let cur = self.selected_index().map(|i| i as i64).unwrap_or(-1);
        let next = if cur < 0 { 0 } else { (cur + delta).clamp(0, n - 1) };
        self.select_index(next as usize);
    }

    /// Messages an action applies to: the marked ones, else the selected one.
    pub fn targets(&self) -> Vec<Row> {
        let marked = self.marked.borrow();
        if !marked.is_empty() {
            return self.rows.borrow().iter().filter(|r| marked.contains(&key(r))).cloned().collect();
        }
        self.selected_index().and_then(|i| self.rows.borrow().get(i).cloned()).into_iter().collect()
    }

    // ── actions ──────────────────────────────────────────────

    fn groups(rows: &[Row]) -> Vec<(String, String, Vec<u32>)> {
        let mut out: Vec<(String, String, Vec<u32>)> = Vec::new();
        for r in rows {
            match out.iter_mut().find(|(a, f, _)| *a == r.acct && *f == r.folder) {
                Some(g) => g.2.push(r.uid),
                None => out.push((r.acct.clone(), r.folder.clone(), vec![r.uid])),
            }
        }
        out
    }

    pub fn set_seen(self: &Rc<Self>, rows: &[Row], on: bool) {
        let rows: Vec<Row> = rows.iter().filter(|r| r.seen != on).cloned().collect();
        if rows.is_empty() {
            return;
        }
        for (acct, folder, uids) in Self::groups(&rows) {
            {
                let store = self.store.borrow();
                for uid in &uids {
                    store.set_flag(&acct, &folder, *uid, "seen", on);
                }
                store.add_unseen(&acct, &folder, if on { -(uids.len() as i64) } else { uids.len() as i64 });
            }
            if let Some(i) = self.acct_idx(&acct) {
                self.send(i, Cmd::Flag { folder, uids, flag: "\\Seen", on });
            }
        }
        self.apply_local(&rows, |r| r.seen = on);
        self.rebuild_side();
        self.update_title();
    }

    /// Mirrors a local flag change into the list and the reader.
    fn apply_local(&self, rows: &[Row], f: impl Fn(&mut Row)) {
        let keys: Vec<Key> = rows.iter().map(key).collect();
        for r in self.rows.borrow_mut().iter_mut().filter(|r| keys.contains(&key(r))) {
            f(r);
        }
        if let Some((cur, _)) = self.current.borrow_mut().as_mut()
            && keys.contains(&key(cur)) {
                f(cur);
            }
        self.refresh_rows(&keys);
    }

    pub fn toggle_read(self: &Rc<Self>) {
        let t = self.targets();
        if let Some(first) = t.first() {
            let on = !first.seen;
            self.set_seen(&t, on);
            self.clear_marks();
        }
    }

    pub fn toggle_star(self: &Rc<Self>) {
        let t = self.targets();
        let Some(first) = t.first() else { return };
        let on = !first.flagged;
        for (acct, folder, uids) in Self::groups(&t) {
            {
                let store = self.store.borrow();
                for uid in &uids {
                    store.set_flag(&acct, &folder, *uid, "flagged", on);
                }
            }
            if let Some(i) = self.acct_idx(&acct) {
                self.send(i, Cmd::Flag { folder, uids, flag: "\\Flagged", on });
            }
        }
        self.apply_local(&t, |r| r.flagged = on);
        self.clear_marks();
    }

    /// Removes rows from the list right away and picks the next message.
    fn remove_rows(self: &Rc<Self>, rows: &[Row]) {
        let keys: Vec<Key> = rows.iter().map(key).collect();
        let sel = self.selected_index().unwrap_or(0);
        for (acct, folder, uids) in Self::groups(rows) {
            let unseen = rows.iter().filter(|r| r.acct == acct && r.folder == folder && !r.seen).count() as i64;
            let mut store = self.store.borrow_mut();
            store.delete(&acct, &folder, &uids);
            store.add_unseen(&acct, &folder, -unseen);
        }
        self.marked.borrow_mut().clear();
        let mut i = 0;
        let mut removed_before = 0;
        while i < self.rows.borrow().len() {
            if keys.contains(&key(&self.rows.borrow()[i])) {
                self.rows.borrow_mut().remove(i);
                self.list_model.remove(i as u32);
                if i < sel {
                    removed_before += 1;
                }
            } else {
                i += 1;
            }
        }
        *self.current.borrow_mut() = None;
        self.select_index(sel.saturating_sub(removed_before));
        self.update_empty();
        self.rebuild_side();
        self.update_title();
    }

    pub fn move_to_role(self: &Rc<Self>, role: &str, verb: &str) {
        let t = self.targets();
        if t.is_empty() {
            return;
        }
        let mut moved = 0;
        for (acct, folder, uids) in Self::groups(&t) {
            let Some(i) = self.acct_idx(&acct) else { continue };
            let Some(dest) = self.folder_with_role(i, role) else {
                self.toast(&format!("{acct} has no {role} folder"), true);
                continue;
            };
            if dest == folder {
                continue;
            }
            moved += uids.len();
            self.send(i, Cmd::Move { folder, uids, to: dest });
        }
        if moved > 0 {
            let rows: Vec<Row> = t.into_iter().filter(|r| self.acct_idx(&r.acct).and_then(|i| self.folder_with_role(i, role)).is_some_and(|d| d != r.folder)).collect();
            self.remove_rows(&rows);
            self.toast(&format!("{verb} {moved}"), false);
        }
    }

    pub fn move_to(self: &Rc<Self>, rows: Vec<Row>, dest: String) {
        let rows: Vec<Row> = rows.into_iter().filter(|r| r.folder != dest).collect();
        for (acct, folder, uids) in Self::groups(&rows) {
            if let Some(i) = self.acct_idx(&acct) {
                self.send(i, Cmd::Move { folder, uids, to: dest.clone() });
            }
        }
        let n = rows.len();
        self.remove_rows(&rows);
        self.toast(&format!("moved {n} to {}", imap::decode_utf7(&dest)), false);
    }

    pub fn delete(self: &Rc<Self>) {
        let t = self.targets();
        if t.is_empty() {
            return;
        }
        for (acct, folder, uids) in Self::groups(&t) {
            if let Some(i) = self.acct_idx(&acct) {
                self.send(i, Cmd::Delete { folder, uids });
            }
        }
        let n = t.len();
        self.remove_rows(&t);
        self.toast(&format!("deleted {n}"), false);
    }

    pub fn toggle_mark(&self) {
        let Some(i) = self.selected_index() else { return };
        let Some(r) = self.rows.borrow().get(i).cloned() else { return };
        let k = key(&r);
        {
            let mut m = self.marked.borrow_mut();
            if !m.remove(&k) {
                m.insert(k.clone());
            }
        }
        self.refresh_rows(&[k]);
        let n = self.marked.borrow().len();
        self.toast(&if n > 0 { format!("{n} selected · e d s u m apply to all · Esc clears") } else { String::new() }, false);
    }

    pub fn mark_all(&self) {
        let keys: Vec<Key> = self.rows.borrow().iter().map(key).collect();
        self.marked.borrow_mut().extend(keys.iter().cloned());
        self.refresh_rows(&keys);
        self.toast(&format!("{} selected", keys.len()), false);
    }

    pub fn clear_marks(&self) {
        let keys: Vec<Key> = self.marked.borrow_mut().drain().collect();
        self.refresh_rows(&keys);
    }

    /// Marks the open folder read on the server, or every inbox in the unified view. While a
    /// search is typed only the matching messages are marked.
    pub fn mark_all_read(self: &Rc<Self>) {
        if !self.search.text().is_empty() {
            let rows: Vec<Row> = self.rows.borrow().iter().filter(|r| !r.seen).cloned().collect();
            let n = rows.len();
            self.set_seen(&rows, true);
            return self.toast(&format!("marked {n} read"), false);
        }
        let folders: Vec<(usize, String)> = match self.view.borrow().clone() {
            View::Folder { acct, folder } => self.acct_idx(&acct).map(|i| (i, folder)).into_iter().collect(),
            View::Unified => (0..self.accounts.borrow().len()).filter_map(|i| self.folder_with_role(i, "inbox").map(|f| (i, f))).collect(),
        };
        let mut n = 0;
        for (i, folder) in folders {
            let acct = self.accounts.borrow()[i].name.clone();
            {
                let store = self.store.borrow();
                n += store.folders(&acct).iter().find(|f| f.name == folder).map_or(0, |f| f.unseen);
                store.set_folder_seen(&acct, &folder);
            }
            self.send(i, Cmd::SeenAll(folder));
        }
        let rows: Vec<Row> = self.rows.borrow().iter().filter(|r| !r.seen).cloned().collect();
        self.apply_local(&rows, |r| r.seen = true);
        self.rebuild_side();
        self.update_title();
        self.toast(&if n == 0 { "nothing unread".to_owned() } else { format!("marked {n} read") }, false);
    }

    /// Writes the HTML part to a file with remote content blocked and opens it in the browser.
    pub fn open_in_browser(&self, allow_remote: bool) {
        let Some((row, Some(p))) = self.current.borrow().clone() else { return };
        let dir = std::env::var_os("XDG_RUNTIME_DIR").map(std::path::PathBuf::from).unwrap_or_else(std::env::temp_dir).join("brief");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(format!("{}-{}.html", row.acct.replace(['/', ' '], "_"), row.uid));
        let csp = if allow_remote {
            ""
        } else {
            "<meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; img-src data: cid:; style-src 'unsafe-inline' data:; font-src data:\">"
        };
        let body = match &p.html {
            Some(h) => h.clone(),
            None => message::text_page(&p.text),
        };
        let page = format!("<!doctype html><meta charset=utf-8>{csp}<title>{}</title>{body}", glib::markup_escape_text(&p.subject));
        if std::fs::write(&path, page).is_ok() {
            open_uri(&path.to_string_lossy());
        }
    }

    pub fn attachment_action(&self, idx: usize, save: bool) {
        let Some((_, Some(p))) = self.current.borrow().clone() else { return };
        let Some(a) = p.attachments.get(idx) else { return };
        let name = a.name.replace(['/', '\\'], "_");
        let dir = if save {
            dirs::download_dir().unwrap_or_else(|| dirs::home_dir().unwrap_or_default())
        } else {
            std::env::var_os("XDG_RUNTIME_DIR").map(std::path::PathBuf::from).unwrap_or_else(std::env::temp_dir).join("brief")
        };
        let _ = std::fs::create_dir_all(&dir);
        let mut path = dir.join(&name);
        if save {
            let mut n = 1;
            while path.exists() {
                path = dir.join(format!("{n}-{name}"));
                n += 1;
            }
        }
        match std::fs::write(&path, &a.data) {
            Ok(()) if save => self.toast(&format!("saved {}", path.display()), false),
            Ok(()) => open_uri(&path.to_string_lossy()),
            Err(e) => self.toast(&format!("{}: {e}", path.display()), true),
        }
    }

    pub fn show_overlay(&self, which: &gtk::Widget) {
        for w in [self.pal.root.upcast_ref::<gtk::Widget>(), self.help.upcast_ref(), self.form.root.upcast_ref()] {
            w.set_visible(w == which);
        }
        self.overlay_scrim.set_visible(true);
    }

    pub fn hide_overlay(&self) {
        self.overlay_scrim.set_visible(false);
        self.set_mode(if self.stack.visible_child_name().as_deref() == Some("compose") { Mode::Compose } else { Mode::Normal });
        if self.mode.get() == Mode::Normal {
            self.set_focus(self.focus.get());
        }
    }

    pub fn show_help(&self) {
        self.show_overlay(self.help.upcast_ref());
        self.set_mode(Mode::Help);
        self.help.grab_focus();
    }

    pub fn open_form(self: &Rc<Self>) {
        self.form.open(self);
    }
}

impl Row {
    fn folder_is_outgoing(&self) -> bool {
        let f = self.folder.to_lowercase();
        f.contains("sent") || f.contains("gesendet") || f.contains("draft") || f.contains("entw")
    }
}

pub fn human_size(n: usize) -> String {
    if n < 1024 {
        format!("{n} B")
    } else if n < 1024 * 1024 {
        format!("{} KB", n / 1024)
    } else {
        format!("{:.1} MB", n as f64 / 1048576.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_found_and_trimmed() {
        let t = "see https://x.ch/a?b=1. and (http://y.com/z) or mailto:me@x.ch";
        let found: Vec<&str> = find_links(t).into_iter().map(|(s, e)| &t[s..e]).collect();
        assert_eq!(found, ["https://x.ch/a?b=1", "http://y.com/z", "mailto:me@x.ch"]);
    }
}
