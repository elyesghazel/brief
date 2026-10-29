//! `,` or Ctrl+,: the settings, a full-window page like the composer.
//!
//! Sections on the left, their rows on the right. j k move between rows, Tab (Ctrl+PageDown,
//! `]`, 1-7) between sections, Enter toggles, edits or runs a row, h l change a choice, `/` finds
//! a setting, Esc leaves an edit, then an account's page, then the settings. Every change applies
//! at once and goes into config.toml key by key (`config::set`), so the file keeps its comments.

use super::form::{self, Item, Progress};
use super::{App, Mode, human_size, label, open_uri, web, widgets};
use crate::config::{self, Account, Security};
use crate::oauth::Provider;
use crate::store::{self, Store, View};
use crate::theme;
use gtk::prelude::*;
use gtk::{glib, graphene};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};
use std::time::Duration;

const SECTIONS: [(&str, &str); 7] = [
    ("Appearance", "How brief looks. Changes show right away."),
    ("Reading", "Conversations, remote images and when a message counts as read."),
    ("Writing", "How the composer starts. Signatures live with each account."),
    ("Accounts", "Your mail accounts, their servers, names and signatures."),
    ("Sync & notifications", "How often brief looks for mail and how much it keeps offline."),
    ("Keyboard", "Every key, as ? shows them. Ctrl+K lists every command."),
    ("About", ""),
];
const ACCOUNTS: usize = 3;
const ABOUT: usize = 6;
const REPO: &str = "https://github.com/elyesghazel/brief";
const RELEASES: &str = "https://github.com/elyesghazel/brief/releases";
const LATEST_API: &str = "https://api.github.com/repos/elyesghazel/brief/releases/latest";

type Act = Rc<dyn Fn(&Rc<App>)>;
type Step = Rc<dyn Fn(&Rc<App>, i32)>;
type Commit = Rc<dyn Fn(&Rc<App>, String) -> Result<(), String>>;
/// Picks option `i` of a choice row.
type Pick = Rc<dyn Fn(&Rc<App>, usize)>;
type Apply = Rc<dyn Fn(&Rc<App>, f64) -> Result<(), String>>;

/// What the keys do on a row, for the status bar.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Hint {
    #[default]
    Info,
    Toggle,
    Choice,
    Text,
    Number,
    Multi,
    Button,
    Open,
}

/// A field that Enter edits: Enter (Ctrl+Enter for several lines) keeps it, Esc puts it back.
struct Edit {
    field: gtk::Widget,
    get: Rc<dyn Fn() -> String>,
    set: Rc<dyn Fn(&str)>,
    commit: Commit,
    multiline: bool,
}

struct Row {
    widget: gtk::Box,
    hint: Hint,
    activate: Option<Act>,
    step: Option<Step>,
    edit: Option<Edit>,
}

#[derive(Clone, PartialEq)]
enum Update {
    Idle,
    Checking,
    Latest(String),
    Newer(String, String),
    Failed(String),
}

pub struct Settings {
    pub root: gtk::Box,
    nav: gtk::ListBox,
    filter: gtk::Entry,
    close: gtk::Button,
    path: gtk::Label,
    scroll: gtk::ScrolledWindow,
    body: gtk::Box,
    /// Holds the focus between edits, so keys never land in a field by accident.
    sink: gtk::Button,
    css: gtk::CssProvider,
    section: Cell<usize>,
    /// The account whose page is open under Accounts.
    account: RefCell<Option<String>>,
    rows: RefCell<Vec<Row>>,
    cursor: Cell<usize>,
    /// The row being edited and its value before.
    editing: RefCell<Option<(usize, String)>>,
    in_filter: Cell<bool>,
    /// Bumped on every rebuild; callbacks of older rows do nothing.
    page_gen: Cell<u64>,
    /// Per address: the last connection test, None while it runs.
    tests: RefCell<HashMap<String, Option<(bool, String)>>>,
    update: RefCell<Update>,
    restart_gen: Cell<u64>,
}

impl Settings {
    pub fn new() -> Settings {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.add_css_class("settings");

        let head = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        head.add_css_class("topbar");
        let brand = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        brand.add_css_class("brand-box");
        brand.append(&widgets::logo(20));
        brand.append(&label("brief", "brand"));
        head.append(&brand);
        let path = label("", "path");
        path.set_use_markup(true);
        head.append(&path);
        let filter = gtk::Entry::new();
        filter.add_css_class("search");
        filter.set_primary_icon_name(Some("system-search-symbolic"));
        filter.set_placeholder_text(Some("Find a setting   /"));
        filter.set_max_width_chars(40);
        let center = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        center.set_hexpand(true);
        center.set_halign(gtk::Align::Center);
        center.append(&filter);
        head.append(&center);
        let close = widgets::icon_button("window-close-symbolic", "Close", "Back to the mail (Esc)");
        close.add_css_class("bar");
        head.append(&close);
        root.append(&head);

        let main = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        main.set_vexpand(true);
        let nav = gtk::ListBox::new();
        nav.add_css_class("set-nav");
        nav.set_selection_mode(gtk::SelectionMode::Single);
        nav.set_size_request(230, -1);
        for (i, (name, _)) in SECTIONS.iter().enumerate() {
            let b = gtk::Box::new(gtk::Orientation::Horizontal, 10);
            b.add_css_class("set-nav-item");
            let n = label(name, "set-nav-name");
            n.set_hexpand(true);
            b.append(&n);
            let k = label(&(i + 1).to_string(), "set-nav-key");
            k.set_valign(gtk::Align::Center);
            b.append(&k);
            let row = gtk::ListBoxRow::new();
            row.set_child(Some(&b));
            row.set_focusable(false);
            nav.append(&row);
        }
        main.append(&nav);
        main.append(&gtk::Separator::new(gtk::Orientation::Vertical));
        let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
        body.add_css_class("set-body");
        body.set_hexpand(true);
        let scroll = gtk::ScrolledWindow::builder().child(&body).hscrollbar_policy(gtk::PolicyType::Never).hexpand(true).vexpand(true).build();
        main.append(&scroll);
        root.append(&main);
        let sink = gtk::Button::new();
        sink.add_css_class("focus-sink");
        sink.set_can_target(false);
        root.append(&sink);

        Settings {
            root,
            nav,
            filter,
            close,
            path,
            scroll,
            body,
            sink,
            css: gtk::CssProvider::new(),
            section: Cell::new(0),
            account: RefCell::new(None),
            rows: RefCell::new(Vec::new()),
            cursor: Cell::new(0),
            editing: RefCell::new(None),
            in_filter: Cell::new(false),
            page_gen: Cell::new(0),
            tests: RefCell::new(HashMap::new()),
            update: RefCell::new(Update::Idle),
            restart_gen: Cell::new(0),
        }
    }

    pub fn wire(&self, app: &Rc<App>) {
        gtk::style_context_add_provider_for_display(&self.root.display(), &self.css, gtk::STYLE_PROVIDER_PRIORITY_USER + 3);
        for i in 0..SECTIONS.len() {
            let Some(row) = self.nav.row_at_index(i as i32) else { continue };
            let me = Rc::downgrade(app);
            on_click(&row, move || {
                if let Some(app) = me.upgrade() {
                    app.settings_go(i);
                }
            });
        }
        let me = Rc::downgrade(app);
        self.filter.connect_changed(move |_| {
            if let Some(app) = me.upgrade() {
                app.settings.cursor.set(0);
                app.settings_render();
                app.settings.scroll.vadjustment().set_value(0.0);
            }
        });
        let focus = gtk::EventControllerFocus::new();
        let me = Rc::downgrade(app);
        focus.connect_enter(move |_| {
            if let Some(app) = me.upgrade() {
                app.settings_finish_edit(true, false);
                app.settings.in_filter.set(true);
                app.update_hints();
            }
        });
        let me = Rc::downgrade(app);
        focus.connect_leave(move |_| {
            if let Some(app) = me.upgrade() {
                app.settings.in_filter.set(false);
                app.update_hints();
            }
        });
        self.filter.add_controller(focus);
        let me = Rc::downgrade(app);
        self.close.connect_clicked(move |_| {
            if let Some(app) = me.upgrade() {
                app.close_settings();
            }
        });
    }

    pub fn is_editing(&self) -> bool {
        self.editing.borrow().is_some()
    }

    /// Keys go nowhere until a field is edited.
    pub fn park_focus(&self) {
        self.sink.grab_focus();
    }
}

/// Builds one page: groups of rows, filtered by the search.
struct Page<'a> {
    app: &'a Rc<App>,
    body: gtk::Box,
    page_gen: u64,
    query: String,
    /// In search results: the section the rows come from.
    section: Option<&'static str>,
    group_title: String,
    group: Option<gtk::Box>,
    rows: Vec<Row>,
}

/// One row: a title and a line of description on the left, the control on the right (below
/// them when `wide`).
#[derive(Default)]
struct Spec<'s> {
    title: &'s str,
    desc: &'s str,
    desc_class: &'s str,
    lead: Option<gtk::Widget>,
    control: Option<gtk::Widget>,
    wide: bool,
    hint: Hint,
    activate: Option<Act>,
    step: Option<Step>,
    edit: Option<Edit>,
}

fn weak(app: &Rc<App>) -> Weak<App> {
    Rc::downgrade(app)
}

/// Runs `f` on a click (release), with a pointer cursor.
fn on_click(w: &impl IsA<gtk::Widget>, f: impl Fn() + 'static) {
    let click = gtk::GestureClick::new();
    click.connect_released(move |g, _, _, _| {
        g.set_state(gtk::EventSequenceState::Claimed);
        f();
    });
    w.add_controller(click);
    w.set_cursor_from_name(Some("pointer"));
}

impl<'a> Page<'a> {
    fn group(&mut self, title: &str) {
        self.group_title = title.to_owned();
        self.group = None;
    }

    fn hit(&self, title: &str, desc: &str) -> bool {
        if self.query.is_empty() {
            return true;
        }
        let hay = format!("{} {} {title} {desc}", self.section.unwrap_or(""), self.group_title).to_lowercase();
        self.query.split_whitespace().all(|w| hay.contains(w))
    }

    fn container(&mut self) -> gtk::Box {
        if let Some(g) = &self.group {
            return g.clone();
        }
        let title = match self.section {
            Some(s) if !self.group_title.is_empty() => format!("{s} · {}", self.group_title),
            Some(s) => s.to_owned(),
            None => self.group_title.clone(),
        };
        if !title.is_empty() {
            self.body.append(&label(&title, "set-kicker"));
        } else if !self.rows.is_empty() {
            self.body.append(&label("", "set-gap"));
        }
        let g = gtk::Box::new(gtk::Orientation::Vertical, 0);
        g.add_css_class("set-group");
        self.body.append(&g);
        self.group = Some(g.clone());
        g
    }

    fn push(&mut self, s: Spec) {
        if !self.hit(s.title, s.desc) {
            return;
        }
        let group = self.container();
        let row = gtk::Box::new(if s.wide { gtk::Orientation::Vertical } else { gtk::Orientation::Horizontal }, if s.wide { 12 } else { 16 });
        row.add_css_class("set-row");
        let line = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        line.set_hexpand(true);
        if let Some(l) = &s.lead {
            l.set_valign(gtk::Align::Center);
            line.append(l);
        }
        let text = gtk::Box::new(gtk::Orientation::Vertical, 0);
        text.set_hexpand(true);
        text.set_valign(gtk::Align::Center);
        let title = label(s.title, "set-title");
        title.set_wrap(true);
        title.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        text.append(&title);
        if !s.desc.is_empty() {
            let d = label(s.desc, "set-desc");
            if !s.desc_class.is_empty() {
                d.add_css_class(s.desc_class);
            }
            d.set_wrap(true);
            d.set_wrap_mode(gtk::pango::WrapMode::WordChar);
            d.set_max_width_chars(64);
            text.append(&d);
        }
        line.append(&text);
        row.append(&line);
        if let Some(c) = &s.control {
            if !s.wide {
                c.set_valign(gtk::Align::Center);
            }
            row.append(c);
        }
        let idx = self.rows.len();
        let pg = self.page_gen;
        let me = weak(self.app);
        // a press anywhere on the row selects it; the control still gets the click
        let click = gtk::GestureClick::new();
        click.set_propagation_phase(gtk::PropagationPhase::Capture);
        click.connect_pressed(move |_, _, _, _| {
            if let Some(app) = me.upgrade() {
                app.settings_pick(pg, idx);
            }
        });
        row.add_controller(click);
        if matches!(s.hint, Hint::Open | Hint::Toggle) {
            let me = weak(self.app);
            on_click(&row, move || {
                if let Some(app) = me.upgrade() {
                    app.settings_activate(pg, idx);
                }
            });
        }
        if let Some(e) = &s.edit {
            let focus = gtk::EventControllerFocus::new();
            let me = weak(self.app);
            focus.connect_enter(move |_| {
                if let Some(app) = me.upgrade() {
                    app.settings_focus_in(pg, idx);
                }
            });
            let me = weak(self.app);
            focus.connect_leave(move |_| {
                if let Some(app) = me.upgrade() {
                    app.settings_focus_out(pg, idx);
                }
            });
            e.field.add_controller(focus);
        }
        group.append(&row);
        self.rows.push(Row { widget: row, hint: s.hint, activate: s.activate, step: s.step, edit: s.edit });
    }

    fn toggle(&mut self, title: &str, desc: &str, on: bool, set: impl Fn(&Rc<App>, bool) + 'static) {
        if !self.hit(title, desc) {
            return;
        }
        let sw = gtk::Switch::new();
        sw.set_active(on);
        sw.set_focusable(false);
        // the whole row toggles it
        sw.set_can_target(false);
        let me = weak(self.app);
        sw.connect_active_notify(move |w| {
            if let Some(app) = me.upgrade() {
                set(&app, w.is_active());
            }
        });
        let flip = sw.clone();
        let activate: Act = Rc::new(move |_| flip.set_active(!flip.is_active()));
        self.push(Spec { title, desc, control: Some(sw.upcast()), hint: Hint::Toggle, activate: Some(activate), ..Default::default() });
    }

    /// A segmented choice; `options` are (label, value).
    fn choice(&mut self, title: &str, desc: &str, options: Vec<(String, String)>, current: &str, set: impl Fn(&Rc<App>, &str) + 'static) {
        if !self.hit(title, desc) {
            return;
        }
        let bx = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        bx.add_css_class("seg");
        let buttons: Rc<Vec<gtk::Button>> = Rc::new(
            options
                .iter()
                .map(|(l, _)| {
                    let b = gtk::Button::with_label(l);
                    b.set_focusable(false);
                    bx.append(&b);
                    b
                })
                .collect(),
        );
        let values: Vec<String> = options.into_iter().map(|(_, v)| v).collect();
        let cur = Rc::new(Cell::new(values.iter().position(|v| v == current).unwrap_or(0)));
        let mark = {
            let buttons = buttons.clone();
            move |i: usize| {
                for (j, b) in buttons.iter().enumerate() {
                    if i == j {
                        b.add_css_class("on");
                    } else {
                        b.remove_css_class("on");
                    }
                }
            }
        };
        mark(cur.get());
        let n = values.len();
        let pick: Pick = {
            let cur = cur.clone();
            Rc::new(move |app, i| {
                if i == cur.get() || i >= n {
                    return;
                }
                cur.set(i);
                mark(i);
                set(app, &values[i]);
            })
        };
        for (i, b) in buttons.iter().enumerate() {
            let (pick, me) = (pick.clone(), weak(self.app));
            b.connect_clicked(move |_| {
                if let Some(app) = me.upgrade() {
                    pick(&app, i);
                }
            });
        }
        let (p2, c2) = (pick.clone(), cur.clone());
        let step: Step = Rc::new(move |app, d| p2(app, (c2.get() as i32 + d).clamp(0, n as i32 - 1) as usize));
        let activate: Act = Rc::new(move |app| pick(app, (cur.get() + 1) % n));
        self.push(Spec { title, desc, control: Some(bx.upcast()), hint: Hint::Choice, activate: Some(activate), step: Some(step), ..Default::default() });
    }

    fn entry_edit<E: IsA<gtk::Editable> + IsA<gtk::Widget>>(e: &E, commit: impl Fn(&Rc<App>, String) -> Result<(), String> + 'static) -> Edit {
        let (g, s) = (e.clone(), e.clone());
        Edit {
            field: e.clone().upcast::<gtk::Widget>(),
            get: Rc::new(move || g.text().to_string()),
            set: Rc::new(move |t| s.set_text(t)),
            commit: Rc::new(commit),
            multiline: false,
        }
    }

    fn text(&mut self, title: &str, desc: &str, value: &str, placeholder: &str, commit: impl Fn(&Rc<App>, String) -> Result<(), String> + 'static) {
        self.text_sized(title, desc, value, placeholder, 30, commit);
    }

    fn text_sized(&mut self, title: &str, desc: &str, value: &str, placeholder: &str, chars: i32, commit: impl Fn(&Rc<App>, String) -> Result<(), String> + 'static) {
        if !self.hit(title, desc) {
            return;
        }
        let e = gtk::Entry::new();
        e.add_css_class("set-entry");
        e.set_text(value);
        e.set_placeholder_text(Some(placeholder));
        e.set_width_chars(chars);
        e.set_max_width_chars(chars);
        let edit = Self::entry_edit(&e, commit);
        self.push(Spec { title, desc, control: Some(e.upcast()), hint: Hint::Text, edit: Some(edit), ..Default::default() });
    }

    fn password(&mut self, title: &str, desc: &str, commit: impl Fn(&Rc<App>, String) -> Result<(), String> + 'static) {
        if !self.hit(title, desc) {
            return;
        }
        let e = gtk::PasswordEntry::new();
        e.add_css_class("set-entry");
        e.set_show_peek_icon(true);
        e.set_placeholder_text(Some("•••••••• · Enter to change"));
        e.set_width_chars(30);
        let edit = Self::entry_edit(&e, commit);
        self.push(Spec { title, desc, control: Some(e.upcast()), hint: Hint::Text, edit: Some(edit), ..Default::default() });
    }

    /// A number with − and +; h l step it, Enter types it.
    fn number(&mut self, title: &str, desc: &str, value: f64, range: (f64, f64, f64), unit: &str, commit: impl Fn(&Rc<App>, f64) + 'static) {
        if !self.hit(title, desc) {
            return;
        }
        let (min, max, by) = range;
        let fmt = move |v: f64| if by.fract() == 0.0 { format!("{}", v as i64) } else { format!("{v:.1}") };
        let bx = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        bx.add_css_class("stepper");
        let minus = gtk::Button::with_label("−");
        let e = gtk::Entry::new();
        e.add_css_class("set-entry");
        e.add_css_class("num");
        e.set_width_chars(5);
        e.set_max_width_chars(5);
        EditableExt::set_alignment(&e, 0.5);
        e.set_text(&fmt(value));
        let plus = gtk::Button::with_label("+");
        for b in [&minus, &plus] {
            b.set_focusable(false);
            b.add_css_class("step-btn");
        }
        bx.append(&minus);
        bx.append(&e);
        bx.append(&plus);
        if !unit.is_empty() {
            bx.append(&label(unit, "set-unit"));
        }
        let what = title.to_owned();
        let apply: Apply = {
            let e = e.clone();
            Rc::new(move |app, v| {
                if !(min..=max).contains(&v) {
                    return Err(format!("{what} goes from {} to {}", fmt(min), fmt(max)));
                }
                e.set_text(&fmt(v));
                commit(app, v);
                Ok(())
            })
        };
        let step: Step = {
            let (e, apply) = (e.clone(), apply.clone());
            Rc::new(move |app, d| {
                let v = e.text().trim().replace(',', ".").parse::<f64>().unwrap_or(value);
                let v = ((v + d as f64 * by) / by).round() * by;
                let _ = apply(app, v.clamp(min, max));
            })
        };
        for (b, d) in [(&minus, -1), (&plus, 1)] {
            let (step, me) = (step.clone(), weak(self.app));
            b.connect_clicked(move |_| {
                if let Some(app) = me.upgrade() {
                    step(&app, d);
                }
            });
        }
        let edit = Self::entry_edit(&e, move |app, text| {
            let v: f64 = text.trim().replace(',', ".").parse().map_err(|_| format!("{text} is not a number"))?;
            apply(app, v)
        });
        self.push(Spec { title, desc, control: Some(bx.upcast()), hint: Hint::Number, step: Some(step), edit: Some(edit), ..Default::default() });
    }

    fn multiline(&mut self, title: &str, desc: &str, value: &str, commit: impl Fn(&Rc<App>, String) -> Result<(), String> + 'static) {
        if !self.hit(title, desc) {
            return;
        }
        let tv = gtk::TextView::new();
        tv.add_css_class("set-textarea");
        tv.set_wrap_mode(gtk::WrapMode::WordChar);
        tv.set_accepts_tab(false);
        tv.buffer().set_text(value);
        let frame = gtk::ScrolledWindow::builder()
            .child(&tv)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .min_content_height(90)
            .max_content_height(220)
            .propagate_natural_height(true)
            .build();
        frame.add_css_class("set-textbox");
        let (g, s) = (tv.buffer(), tv.buffer());
        let edit = Edit {
            field: tv.upcast(),
            get: Rc::new(move || g.text(&g.start_iter(), &g.end_iter(), false).to_string()),
            set: Rc::new(move |t| s.set_text(t)),
            commit: Rc::new(commit),
            multiline: true,
        };
        self.push(Spec { title, desc, control: Some(frame.upcast()), wide: true, hint: Hint::Multi, edit: Some(edit), ..Default::default() });
    }

    /// `class`: "" , "primary" or "danger".
    fn button(&mut self, title: &str, desc: &str, desc_class: &str, text: &str, class: &str, run: impl Fn(&Rc<App>) + 'static) {
        if !self.hit(title, desc) {
            return;
        }
        let b = gtk::Button::with_label(text);
        b.add_css_class("set-btn");
        if !class.is_empty() {
            b.add_css_class(class);
        }
        b.set_focusable(false);
        let run: Act = Rc::new(run);
        let (r, me) = (run.clone(), weak(self.app));
        b.connect_clicked(move |_| {
            if let Some(app) = me.upgrade() {
                r(&app);
            }
        });
        self.push(Spec { title, desc, desc_class, control: Some(b.upcast()), hint: Hint::Button, activate: Some(run), ..Default::default() });
    }

    fn info(&mut self, title: &str, desc: &str, value: &str) {
        let v = label(value, "set-value");
        v.set_ellipsize(gtk::pango::EllipsizeMode::Start);
        v.set_max_width_chars(34);
        self.push(Spec { title, desc, control: (!value.is_empty()).then(|| v.upcast()), ..Default::default() });
    }

    /// A row that opens something: another page, a link.
    fn link(&mut self, title: &str, desc: &str, lead: Option<gtk::Widget>, trail: &str, run: impl Fn(&Rc<App>) + 'static) {
        let b = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        if !trail.is_empty() {
            b.append(&label(trail, "set-value"));
        }
        let back = lead.as_ref().and_then(|l| l.downcast_ref::<gtk::Label>()).is_some_and(|l| l.text() == "‹");
        if !back {
            b.append(&label("›", "set-chev"));
        }
        self.push(Spec { title, desc, lead, control: Some(b.upcast()), hint: Hint::Open, activate: Some(Rc::new(run)), ..Default::default() });
    }

    /// A key and what it does, for the Keyboard section.
    fn key(&mut self, keys: &str, what: &str) {
        if !self.hit(what, keys) {
            return;
        }
        let k = label(keys, "set-key");
        self.push(Spec { title: what, control: Some(k.upcast()), ..Default::default() });
        if let Some(r) = self.rows.last() {
            r.widget.add_css_class("compact");
        }
    }
}

/// Like the setup's dots: the account's color, the first follows the theme.
fn dot(class: &str) -> gtk::Box {
    let d = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    d.add_css_class("color-dot");
    d.add_css_class(class);
    d.set_valign(gtk::Align::Center);
    d
}

fn opts(list: &[(&str, &str)]) -> Vec<(String, String)> {
    list.iter().map(|(l, v)| (l.to_string(), v.to_string())).collect()
}

/// A preset list plus the current value when it is none of them.
fn with_current(mut list: Vec<(String, String)>, current: String, show: impl Fn(&str) -> String) -> Vec<(String, String)> {
    if !list.iter().any(|(_, v)| *v == current) {
        list.push((show(&current), current));
    }
    list
}

fn minutes(v: &str) -> String {
    match v.parse::<u64>() {
        Ok(m) if m >= 60 && m % 60 == 0 => format!("{} h", m / 60),
        _ => format!("{v} min"),
    }
}

/// "v0.2.0" is newer than "0.1.0".
fn newer(tag: &str, current: &str) -> bool {
    let parse = |s: &str| -> Vec<u64> {
        s.trim().trim_start_matches(['v', 'V']).split(['.', '-', '+']).take(3).map(|p| p.parse().unwrap_or(0)).collect()
    };
    parse(tag) > parse(current)
}

/// The newest release's tag and page.
fn latest_release() -> Result<(String, String), String> {
    let agent: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(10))).build().into();
    let mut resp = agent
        .get(LATEST_API)
        .header("User-Agent", concat!("brief/", env!("CARGO_PKG_VERSION"), " (update check)"))
        .header("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| e.to_string())?;
    let body = resp.body_mut().with_config().limit(1 << 20).read_to_string().map_err(|e| e.to_string())?;
    let v: serde_json::Value = serde_json::from_str(&body).map_err(|e| e.to_string())?;
    let tag = v.get("tag_name").and_then(|t| t.as_str()).ok_or("no release found")?.to_owned();
    let url = v.get("html_url").and_then(|t| t.as_str()).unwrap_or(RELEASES).to_owned();
    Ok((tag, url))
}

/// Applies a written account field to the account in memory.
fn apply_field(acc: &mut Account, path: &[&str], value: Option<&toml::Value>) {
    let text = value.and_then(|v| v.as_str()).unwrap_or_default().to_owned();
    match path {
        ["display_name"] => acc.display_name = text,
        ["signature"] => acc.signature = text,
        ["color"] => acc.color = text,
        ["user"] => acc.user = text,
        ["password_cmd"] => acc.password_cmd = text,
        ["password"] => acc.password = text,
        [t, k] => {
            let s = if *t == "imap" { &mut acc.imap } else { &mut acc.smtp };
            match *k {
                "host" => s.host = text,
                "port" => s.port = value.and_then(|v| v.as_integer()).unwrap_or(0) as u16,
                "security" => {
                    s.security = match text.as_str() {
                        "starttls" => Security::Starttls,
                        "none" => Security::None,
                        _ => Security::Tls,
                    }
                }
                _ => {}
            }
        }
        _ => {}
    }
}

/// Some(text) as a value, or None (remove the key) when empty.
fn text_value(v: &str) -> Option<toml::Value> {
    let v = v.trim();
    (!v.is_empty()).then(|| toml::Value::String(v.to_owned()))
}

impl App {
    fn settings_open(&self) -> bool {
        self.stack.visible_child_name().as_deref() == Some("settings")
    }

    pub fn open_settings(self: &Rc<Self>) {
        match self.stack.visible_child_name().as_deref() {
            Some("compose") => return self.toast("close the composer first", true),
            Some("settings") => return,
            _ => {}
        }
        let s = &self.settings;
        s.account.replace(None);
        s.cursor.set(0);
        if s.filter.text().is_empty() {
            self.settings_render();
        } else {
            s.filter.set_text("");
        }
        self.stack.set_visible_child_name("settings");
        // GTK hands the focus to the first field of the new page; take it back
        s.park_focus();
        self.settings_finish_edit(false, true);
        s.cursor.set(0);
        self.settings_mark();
        self.set_mode(Mode::Settings);
        s.scroll.vadjustment().set_value(0.0);
    }

    pub fn close_settings(self: &Rc<Self>) {
        self.settings_finish_edit(true, false);
        self.stack.set_visible_child_name("main");
        self.set_mode(Mode::Normal);
        self.set_focus(self.focus.get());
    }

    /// Builds the page again, keeping the row and the scroll position.
    pub fn settings_refresh(self: &Rc<Self>) {
        if !self.settings_open() {
            return;
        }
        let me = Rc::downgrade(self);
        glib::idle_add_local_once(move || {
            let Some(app) = me.upgrade() else { return };
            if app.settings.is_editing() {
                return;
            }
            let adj = app.settings.scroll.vadjustment();
            let at = adj.value();
            app.settings_render();
            glib::idle_add_local_once(move || adj.set_value(at));
        });
    }

    /// An account was removed: leave its page.
    pub fn settings_account_gone(self: &Rc<Self>, name: &str) {
        if self.settings.account.borrow().as_deref() == Some(name) {
            self.settings.account.replace(None);
        }
        self.settings_refresh();
    }

    fn settings_go(self: &Rc<Self>, section: usize) {
        let s = &self.settings;
        self.settings_finish_edit(true, false);
        s.section.set(section.min(SECTIONS.len() - 1));
        s.account.replace(None);
        s.cursor.set(0);
        if s.filter.text().is_empty() {
            self.settings_render();
        } else {
            s.filter.set_text("");
        }
        s.park_focus();
        s.scroll.vadjustment().set_value(0.0);
    }

    fn settings_open_account(self: &Rc<Self>, name: &str) {
        let s = &self.settings;
        s.section.set(ACCOUNTS);
        s.account.replace(Some(name.to_owned()));
        // the first row after "All accounts"
        s.cursor.set(1);
        if s.filter.text().is_empty() {
            self.settings_render();
        } else {
            s.filter.set_text("");
            s.cursor.set(1);
            self.settings_render();
        }
        s.park_focus();
        s.scroll.vadjustment().set_value(0.0);
    }

    fn settings_back(self: &Rc<Self>) {
        let s = &self.settings;
        let Some(name) = s.account.take() else { return };
        s.cursor.set(self.acct_idx(&name).unwrap_or(0));
        self.settings_render();
        s.scroll.vadjustment().set_value(0.0);
        self.settings_scroll_later();
    }

    fn settings_render(self: &Rc<Self>) {
        let s = &self.settings;
        s.editing.replace(None);
        let page_gen = s.page_gen.get() + 1;
        s.page_gen.set(page_gen);
        while let Some(c) = s.body.first_child() {
            s.body.remove(&c);
        }
        let query = s.filter.text().trim().to_lowercase();
        let mut page = Page {
            app: self,
            body: s.body.clone(),
            page_gen,
            query: query.clone(),
            section: None,
            group_title: String::new(),
            group: None,
            rows: Vec::new(),
        };
        let section = s.section.get();
        let account = s.account.borrow().clone().filter(|_| section == ACCOUNTS && query.is_empty());
        let heading = |title: &str, lead: &str| {
            s.body.append(&label(title, "set-h1"));
            let l = label(lead, "set-lead");
            l.set_wrap(true);
            s.body.append(&l);
        };
        if !query.is_empty() {
            s.nav.unselect_all();
            s.path.set_markup("Settings › <b>Find</b>");
            heading("Results", &format!("Settings matching “{}”.", s.filter.text().trim()));
            for (i, (name, _)) in SECTIONS.iter().enumerate() {
                page.section = Some(name);
                page.group(name);
                self.settings_section(&mut page, i);
            }
            if page.rows.is_empty() {
                s.body.append(&label("Nothing matches. Esc clears the search.", "set-empty"));
            }
        } else {
            if let Some(r) = s.nav.row_at_index(section as i32) {
                s.nav.select_row(Some(&r));
            }
            let (name, lead) = SECTIONS[section];
            match &account {
                Some(acct) => {
                    s.path.set_markup(&format!("Settings › Accounts › <b>{}</b>", glib::markup_escape_text(acct)));
                    let email = self.accounts.borrow().iter().find(|a| a.name == *acct).map(|a| a.email.clone()).unwrap_or_default();
                    heading(acct, &email);
                    self.settings_account_page(&mut page, acct);
                }
                None => {
                    s.path.set_markup(&format!("Settings › <b>{}</b>", glib::markup_escape_text(name)));
                    if section == ABOUT {
                        self.settings_hero();
                    } else {
                        heading(name, lead);
                    }
                    self.settings_section(&mut page, section);
                }
            }
        }
        let rows = std::mem::take(&mut page.rows);
        drop(page);
        let n = rows.len();
        *s.rows.borrow_mut() = rows;
        s.cursor.set(s.cursor.get().min(n.saturating_sub(1)));
        self.settings_mark();
        self.update_hints();
    }

    fn settings_section(self: &Rc<Self>, p: &mut Page, section: usize) {
        match section {
            0 => self.settings_appearance(p),
            1 => self.settings_reading(p),
            2 => self.settings_writing(p),
            3 => self.settings_accounts(p),
            4 => self.settings_sync(p),
            5 => {
                for group in super::palette::HELP {
                    for (k, d) in group.iter() {
                        match k.strip_prefix('#') {
                            Some(title) => p.group(title),
                            None => p.key(k, d),
                        }
                    }
                }
            }
            _ => self.settings_about(p),
        }
    }

    // ── sections ─────────────────────────────────────────────

    fn settings_appearance(self: &Rc<Self>, p: &mut Page) {
        let cfg = self.cfg.borrow().clone();
        p.group("Theme");
        if p.hit("Theme", "colors dark light zunder") {
            self.settings_themes(p);
        }
        p.group("Text");
        p.text("Font", "CSS font families, the first one installed wins. Mail text uses it too.", &cfg.font, crate::platform::FONT, |a, v| {
            a.set_font(&v);
            Ok(())
        });
        p.number("Size", "Of everything, in points. The HTML view zooms on its own with Ctrl+= and Ctrl+-.", cfg.font_size, (7.0, 20.0, 0.5), "pt", |a, v| {
            a.cfg.borrow_mut().font_size = v;
            a.apply_font();
            a.settings_save("font_size", v);
        });
        p.group("Mail");
        if web::AVAILABLE {
            p.toggle("HTML mail", "Render HTML mail in the window, remote images blocked. Off shows the text version. H switches one mail.", cfg.html, |a, on| {
                a.cfg.borrow_mut().html = on;
                a.rerender();
                a.settings_save("html", on);
            });
        } else {
            p.info("HTML mail", "This build shows HTML mail as text; o opens it in the browser.", "text only");
        }
        let icons = opts(&[("DuckDuckGo", "duckduckgo"), ("Direct", "direct"), ("Off", "off")]);
        p.choice(
            "Sender icons",
            "DuckDuckGo fetches logos without the sender noticing. Direct asks the sender's site and looks sharper. Off shows initials.",
            icons,
            &cfg.favicons,
            |a, v| {
                a.cfg.borrow_mut().favicons = v.to_owned();
                a.icons.set_fetch(super::icon_fetcher(v, &a.events));
                a.schedule_rebind();
                a.settings_save("favicons", v);
            },
        );
        p.group("Window");
        p.toggle("Title bar", "The window's own title bar. Tiling desktops do without it.", cfg.decorations, |a, on| {
            a.win.set_decorated(on);
            a.cfg.borrow_mut().decorations = on;
            a.settings_save("decorations", on);
        });
    }

    /// Every theme as a swatch in its own colors; h l and clicks switch live.
    fn settings_themes(self: &Rc<Self>, p: &mut Page) {
        let names = theme::names();
        let mut css = String::new();
        for (i, n) in names.iter().enumerate() {
            let [bg, fg, accent] = theme::preview(n);
            css += &format!(".set-sw-{i} {{ background: {bg}; }} .set-sw-{i} .swatch-fg {{ color: {fg}; }} .set-sw-{i} .swatch-accent {{ background: {accent}; }}\n");
        }
        self.settings.css.load_from_string(&css);
        let flow = gtk::FlowBox::new();
        flow.add_css_class("set-themes");
        flow.set_selection_mode(gtk::SelectionMode::None);
        flow.set_homogeneous(true);
        flow.set_max_children_per_line(7);
        flow.set_min_children_per_line(4);
        flow.set_column_spacing(6);
        flow.set_row_spacing(8);
        flow.set_can_focus(false);
        let mut cells = Vec::new();
        for (i, n) in names.iter().enumerate() {
            let cell = gtk::Box::new(gtk::Orientation::Vertical, 6);
            cell.add_css_class("swatch-cell");
            let sw = gtk::Box::new(gtk::Orientation::Vertical, 4);
            sw.add_css_class("swatch");
            sw.add_css_class(&format!("set-sw-{i}"));
            sw.append(&label("Aa", "swatch-fg"));
            let bar = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            bar.add_css_class("swatch-accent");
            bar.set_halign(gtk::Align::Start);
            sw.append(&bar);
            cell.append(&sw);
            cell.append(super::ellipsize(&label(n, "swatch-name")));
            cell.set_tooltip_text(Some(n));
            flow.insert(&cell, -1);
            if let Some(c) = flow.child_at_index(i as i32) {
                c.set_focusable(false);
            }
            cells.push(cell);
        }
        let current = self.theme_name.borrow().clone();
        let idx = Rc::new(Cell::new(names.iter().position(|n| *n == current).unwrap_or(0)));
        let cells = Rc::new(cells);
        let mark = {
            let cells = cells.clone();
            move |i: usize| {
                for (j, c) in cells.iter().enumerate() {
                    if i == j {
                        c.add_css_class("current");
                    } else {
                        c.remove_css_class("current");
                    }
                }
            }
        };
        mark(idx.get());
        let names = Rc::new(names);
        let pick: Pick = {
            let (idx, names) = (idx.clone(), names.clone());
            Rc::new(move |app, i| {
                idx.set(i);
                mark(i);
                app.apply_theme(&names[i]);
                app.settings_save("theme", names[i].as_str());
            })
        };
        for (i, c) in cells.iter().enumerate() {
            let (pick, me) = (pick.clone(), weak(self));
            on_click(c, move || {
                if let Some(app) = me.upgrade() {
                    pick(&app, i);
                }
            });
        }
        let n = names.len() as i32;
        let (p2, i2) = (pick.clone(), idx.clone());
        let step: Step = Rc::new(move |app, d| p2(app, (i2.get() as i32 + d).rem_euclid(n) as usize));
        let activate: Act = Rc::new(move |app| pick(app, (idx.get() + 1) % n as usize));
        p.push(Spec {
            title: "Theme",
            desc: "h l or a click switch it. Your own go into themes/ next to config.toml as <name>.css.",
            control: Some(flow.upcast()),
            wide: true,
            hint: Hint::Choice,
            activate: Some(activate),
            step: Some(step),
            ..Default::default()
        });
    }

    fn settings_reading(self: &Rc<Self>, p: &mut Page) {
        let cfg = self.cfg.borrow().clone();
        p.group("Conversations");
        p.toggle(
            "Group into conversations",
            "One row per conversation, your replies from Sent included. The reader folds the older messages.",
            cfg.threads,
            |a, on| {
                if a.threads() != on {
                    a.toggle_threads();
                }
            },
        );
        p.group("Read status");
        let delays = opts(&[("Instantly", "0"), ("After 0.7 s", "700"), ("After 2 s", "2000"), ("After 5 s", "5000"), ("Never", "-1")]);
        let delays = with_current(delays, cfg.mark_read_ms.to_string(), |v| format!("{v} ms"));
        p.choice("Mark as read", "When an open message counts as read, so skimming with j k leaves mail unread. u switches it by hand.", delays, &cfg.mark_read_ms.to_string(), |a, v| {
            let ms: i64 = v.parse().unwrap_or(700);
            a.cfg.borrow_mut().mark_read_ms = ms;
            a.settings_save("mark_read_ms", ms);
        });
        p.group("Remote images");
        let trusted = self.store.borrow().trusted();
        if trusted.is_empty() {
            p.info("No senders yet", "Remote images stay blocked until you press I. Always for sender, on a mail's image banner, adds its sender here.", "");
        }
        for who in trusted {
            let w = who.clone();
            p.button(&who, "Remote images from this sender always load.", "", "Remove", "", move |a| {
                a.store.borrow().set_trusted(&w, false);
                a.toast(&format!("remote images from {w} are blocked again"), false);
                a.settings_refresh();
            });
        }
    }

    fn settings_writing(self: &Rc<Self>, p: &mut Page) {
        let cfg = self.cfg.borrow().clone();
        p.group("Composer");
        p.choice(
            "New messages start as",
            "Rich text is sent as HTML with a plain text copy, and only once you format something. Ctrl+Shift+M switches while writing.",
            opts(&[("Rich text", "rich"), ("Plain text", "plain")]),
            &cfg.compose,
            |a, v| {
                a.cfg.borrow_mut().compose = v.to_owned();
                a.settings_save("compose", v);
            },
        );
        p.toggle("Formatting bar", "The bar above the message in rich text. Ctrl+Shift+F shows or hides it while writing.", cfg.toolbar, |a, on| {
            a.cfg.borrow_mut().toolbar = on;
            a.settings_save("toolbar", on);
        });
        let default = if cfg!(windows) { "%EDITOR%, else Notepad" } else { "$TERMINAL -e $EDITOR" };
        p.text("External editor", "Ctrl+E in the composer opens the message in it; brief adds the file path.", &cfg.editor, default, |a, v| {
            let v = v.trim().to_owned();
            a.cfg.borrow_mut().editor = v.clone();
            let r = if v.is_empty() { config::unset("editor") } else { config::set("editor", v) };
            if let Err(e) = r {
                a.toast(&e, true);
            }
            Ok(())
        });
        p.group("Signatures");
        let accounts = self.accounts.borrow().clone();
        if accounts.is_empty() {
            p.info("No accounts yet", "Each account has its own signature.", "");
        }
        for (i, acc) in accounts.iter().enumerate() {
            let first = acc.signature.lines().map(str::trim).find(|l| !l.is_empty() && *l != "--").unwrap_or("");
            let desc = if acc.signature.trim().is_empty() { "No signature".to_owned() } else { first.to_owned() };
            let name = acc.name.clone();
            p.link(&acc.name, &desc, Some(dot(&format!("acct-dot-{}", i.min(15))).upcast()), "Edit", move |a| a.settings_open_account(&name));
        }
    }

    fn settings_accounts(self: &Rc<Self>, p: &mut Page) {
        p.group("Accounts");
        let accounts = self.accounts.borrow().clone();
        if accounts.is_empty() {
            p.info("No accounts yet", "Add one to start reading mail.", "");
        }
        for (i, acc) in accounts.iter().enumerate() {
            let state = if crate::oauth::needs_sign_in(acc) {
                "signed out"
            } else if acc.is_oauth() {
                "browser sign-in"
            } else {
                "password"
            };
            let name = acc.name.clone();
            let lead = dot(&format!("acct-dot-{}", i.min(15)));
            lead.add_css_class("big");
            p.link(&acc.name, &acc.email, Some(lead.upcast()), state, move |a| a.settings_open_account(&name));
        }
        p.group("");
        p.button("Add an account", "Gmail, Outlook, iCloud or any IMAP and SMTP server. A does the same.", "", "Add…", "primary", |a| a.open_form());
    }

    fn settings_account_page(self: &Rc<Self>, p: &mut Page, name: &str) {
        let Some(acc) = self.accounts.borrow().iter().find(|a| a.name == name).cloned() else {
            p.link("All accounts", "This account is gone.", None, "", |a| a.settings_back());
            return;
        };
        let i = self.acct_idx(name).unwrap_or(0);
        p.link("All accounts", "", Some(label("‹", "set-chev").upcast()), "", |a| a.settings_back());

        p.group("Identity");
        let n = acc.name.clone();
        p.text("Label", "In the sidebar and the unified inbox. The offline mail moves along.", &acc.name, "", move |a, v| a.settings_rename(&n, v));
        p.info("Address", "To send from another address, add it as an account.", &acc.email);
        let n = acc.name.clone();
        p.text("Your name", "In the From line of what you send. Empty uses the label.", &acc.display_name, &acc.name, move |a, v| {
            a.settings_account_set(&n, &["display_name"], text_value(&v), false)
        });
        self.settings_colors(p, &acc, i);
        let n = acc.name.clone();
        p.multiline("Signature", "Goes below new messages, replies and forwards. Enter starts editing, Ctrl+Enter keeps it.", &acc.signature, move |a, v| {
            let v = v.trim_end().to_owned();
            a.settings_account_set(&n, &["signature"], (!v.trim().is_empty()).then_some(toml::Value::String(v)), false)
        });

        p.group("Sign-in");
        if acc.is_oauth() {
            let who = Provider::from_name(&acc.oauth.provider).map_or_else(|| acc.oauth.provider.clone(), |p| p.label().to_owned());
            let (desc, class) = if crate::oauth::needs_sign_in(&acc) {
                ("Signed out. Sign in again to sync.", "error")
            } else {
                ("brief keeps a token in your keyring, never the password.", "")
            };
            let n = acc.name.clone();
            p.button(&format!("Signed in with {who}"), desc, class, "Sign in again", "", move |a| a.sign_in_again(&n));
        } else if !acc.password_cmd.is_empty() && acc.password_cmd != crate::platform::keyring_cmd(&acc.email) {
            let n = acc.name.clone();
            p.text("Password command", "Prints the password, e.g. pass mail/work.", &acc.password_cmd, "", move |a, v| {
                if v.trim().is_empty() {
                    return Err("the command cannot be empty".into());
                }
                a.settings_account_set(&n, &["password_cmd"], text_value(&v), true)
            });
        } else {
            let desc = if acc.password.is_empty() { "Kept in your keyring, or secrets.toml without one." } else { "Kept in config.toml as written there." };
            let n = acc.name.clone();
            p.password("Password", desc, move |a, v| a.settings_set_password(&n, v));
        }
        let (n, email) = (acc.name.clone(), acc.email.clone());
        p.text("Username", "The login, when it is not the address.", &acc.user, &acc.email, move |a, v| {
            let v = v.trim();
            a.settings_account_set(&n, &["user"], (!v.is_empty() && v != email).then(|| toml::Value::String(v.to_owned())), true)
        });

        for (table, server, title) in [("imap", &acc.imap, "Incoming mail · IMAP"), ("smtp", &acc.smtp, "Outgoing mail · SMTP")] {
            p.group(title);
            let n = acc.name.clone();
            p.text("Server", "", &server.host, "", move |a, v| match text_value(&v) {
                Some(v) => a.settings_account_set(&n, &[table, "host"], Some(v), true),
                None => Err("the server cannot be empty".into()),
            });
            let n = acc.name.clone();
            p.text_sized("Port", "", &server.port.to_string(), "", 7, move |a, v| match v.trim().parse::<u16>() {
                Ok(port) if port > 0 => a.settings_account_set(&n, &[table, "port"], Some(toml::Value::Integer(port.into())), true),
                _ => Err(format!("{v} is not a port")),
            });
            let n = acc.name.clone();
            let desc = if table == "imap" { "TLS: 993. STARTTLS: 143. None sends everything unencrypted." } else { "TLS: 465. STARTTLS: 587. None sends everything unencrypted." };
            p.choice("Security", desc, opts(&[("TLS", "tls"), ("STARTTLS", "starttls"), ("None", "none")]), server.security.as_str(), move |a, v| {
                if let Err(e) = a.settings_account_set(&n, &[table, "security"], text_value(v), true) {
                    a.toast(&e, true);
                }
            });
        }

        p.group("Connection");
        let (desc, class) = match self.settings.tests.borrow().get(&acc.email) {
            None => ("Logs in to both servers with the settings above.".to_owned(), ""),
            Some(None) => ("Testing…".to_owned(), ""),
            Some(Some((true, t))) => (t.clone(), "ok"),
            Some(Some((false, t))) => (t.clone(), "error"),
        };
        let n = acc.name.clone();
        p.button("Test the connection", &desc, class, "Test", "", move |a| a.settings_test(&n));
        p.group("");
        let (n, email) = (acc.name.clone(), acc.email.clone());
        p.button("Remove this account", "Takes it out of brief: its settings, saved password and offline mail. The mail on the server stays.", "", "Remove…", "danger", move |a| {
            let Some(i) = a.acct_idx(&n) else { return };
            let text = format!(
                "Takes {email} out of brief: its block in config.toml, its saved password and its cached mail. \
                 The mail on the server is not touched; add the account again with A."
            );
            a.confirm(&format!("remove {n}?"), &text, "remove", move |a| a.remove_account(i));
        });
    }

    fn settings_colors(self: &Rc<Self>, p: &mut Page, acc: &Account, i: usize) {
        let colors: Vec<&str> = form::COLORS.to_vec();
        let bx = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        bx.add_css_class("set-dots");
        let dots: Rc<Vec<gtk::Box>> = Rc::new(
            (0..colors.len())
                .map(|j| {
                    let d = dot(&if j == 0 { format!("dot-auto-{}", i % 4) } else { format!("dot-c{j}") });
                    if j == 0 {
                        d.set_tooltip_text(Some("follows the theme"));
                    }
                    bx.append(&d);
                    d
                })
                .collect(),
        );
        let custom = !colors.contains(&acc.color.as_str());
        if custom {
            bx.append(&label(&acc.color, "set-value"));
        }
        let idx = Rc::new(Cell::new(colors.iter().position(|c| *c == acc.color).unwrap_or(usize::MAX)));
        let mark = {
            let dots = dots.clone();
            move |k: usize| {
                for (j, d) in dots.iter().enumerate() {
                    if j == k {
                        d.add_css_class("current");
                    } else {
                        d.remove_css_class("current");
                    }
                }
            }
        };
        mark(idx.get());
        let n = colors.len();
        let name = acc.name.clone();
        let pick: Pick = {
            let idx = idx.clone();
            Rc::new(move |app, k| {
                if k == idx.get() || k >= n {
                    return;
                }
                let value = text_value(form::COLORS[k]);
                match app.settings_account_set(&name, &["color"], value, false) {
                    Ok(()) => {
                        idx.set(k);
                        mark(k);
                        app.apply_acct_css();
                    }
                    Err(e) => app.toast(&e, true),
                }
            })
        };
        for (j, d) in dots.iter().enumerate() {
            let (pick, me) = (pick.clone(), weak(self));
            on_click(d, move || {
                if let Some(app) = me.upgrade() {
                    pick(&app, j);
                }
            });
        }
        let (p2, i2) = (pick.clone(), idx.clone());
        let step: Step = Rc::new(move |app, d| {
            let cur = if i2.get() == usize::MAX { 0 } else { i2.get() as i32 };
            p2(app, (cur + d).rem_euclid(n as i32) as usize)
        });
        let activate: Act = Rc::new(move |app| pick(app, if idx.get() == usize::MAX { 0 } else { (idx.get() + 1) % n }));
        p.push(Spec {
            title: "Color",
            desc: "The dot in the sidebar and on the unified inbox. The ring follows the theme.",
            control: Some(bx.upcast()),
            hint: Hint::Choice,
            activate: Some(activate),
            step: Some(step),
            ..Default::default()
        });
    }

    fn settings_sync(self: &Rc<Self>, p: &mut Page) {
        let cfg = self.cfg.borrow().clone();
        p.group("Sync");
        let every: Vec<(String, String)> = [1, 2, 5, 10, 15, 30, 60].iter().map(|m| (minutes(&m.to_string()), m.to_string())).collect();
        let every = with_current(every, cfg.sync_minutes.to_string(), minutes);
        p.choice("Look for mail every", "A full sync of every account. New inbox mail arrives at once anyway, by IMAP IDLE.", every, &cfg.sync_minutes.to_string(), |a, v| {
            let m: u64 = v.parse().unwrap_or(5);
            a.cfg.borrow_mut().sync_minutes = m;
            a.settings_save("sync_minutes", m as i64);
            a.settings_restart_all();
        });
        let keep: Vec<(String, String)> = [200, 500, 1000, 2000, 5000].iter().map(|n| (n.to_string(), n.to_string())).collect();
        let keep = with_current(keep, cfg.cache_limit.to_string(), str::to_owned);
        p.choice("Keep offline", "The newest messages per folder, readable without a connection.", keep, &cfg.cache_limit.to_string(), |a, v| {
            let n: u32 = v.parse().unwrap_or(1000);
            a.cfg.borrow_mut().cache_limit = n;
            a.settings_save("cache_limit", n as i64);
            a.settings_restart_all();
        });
        p.button("Sync now", "Every account, the open folder first. Ctrl+R does the same.", "", "Sync", "", |a| {
            a.refresh_all();
            a.toast("syncing every account…", false);
        });
        p.group("Notifications");
        p.toggle("New mail", "A desktop notification for new inbox mail while brief is in the background.", cfg.notify, |a, on| {
            a.cfg.borrow_mut().notify = on;
            a.settings_save("notify", on);
        });
        p.group("Offline cache");
        let dir = store::path().parent().map(|d| d.display().to_string()).unwrap_or_default();
        p.info("Size on disk", &dir, &human_size(store::disk_size() as usize));
        p.button("Clear the cache", "Drops the offline mail and fetches it again. Accounts, settings and allowed senders stay.", "", "Clear…", "danger", |a| {
            a.confirm(
                "clear the cache?",
                "Drops every cached message and folder and downloads them again. Nothing on the server changes, your settings and allowed senders stay.",
                "clear",
                |a| a.settings_clear_cache(),
            );
        });
    }

    /// Logo, name and version on top of About.
    fn settings_hero(&self) {
        let hero = gtk::Box::new(gtk::Orientation::Horizontal, 18);
        hero.add_css_class("set-hero");
        hero.append(&widgets::logo(56));
        let text = gtk::Box::new(gtk::Orientation::Vertical, 2);
        text.set_valign(gtk::Align::Center);
        text.append(&label("brief", "set-h1"));
        text.append(&label(&format!("version {}", env!("CARGO_PKG_VERSION")), "set-version"));
        text.append(&label("A keyboard-first mail client. Rust and GTK4.", "set-lead"));
        hero.append(&text);
        self.settings.body.append(&hero);
    }

    fn settings_about(self: &Rc<Self>, p: &mut Page) {
        p.group("Updates");
        let state = self.settings.update.borrow().clone();
        match state {
            Update::Newer(tag, url) => p.button(
                &format!("brief {} is available", tag.trim_start_matches('v')),
                &format!("You have {}.", env!("CARGO_PKG_VERSION")),
                "ok",
                "Download",
                "primary",
                move |_| open_uri(&url),
            ),
            other => {
                let (desc, class) = match other {
                    Update::Checking => ("Asking GitHub…".to_owned(), ""),
                    Update::Latest(tag) => (format!("{} is the newest release.", tag.trim_start_matches('v')), "ok"),
                    Update::Failed(e) => (format!("Could not check: {e}"), "error"),
                    _ => ("Asks GitHub for the newest release, only when you press it.".to_owned(), ""),
                };
                p.button("Check for updates", &desc, class, "Check", "", |a| a.settings_check_update());
            }
        }
        p.group("Files");
        let files = [
            ("Config file", config::path(), "Every setting, in plain TOML. brief edits single lines and keeps your comments."),
            ("Log file", crate::log::path(), "Warnings and errors: sync, sign-in and sending. Never passwords or tokens."),
            ("Cache folder", store::path().parent().map(|d| d.to_path_buf()).unwrap_or_default(), "The offline mail, sender icons and the log."),
        ];
        for (title, path, what) in files {
            if !p.hit(title, what) {
                continue;
            }
            let shown = path.display().to_string();
            let desc = format!("{what}\n{shown}");
            p.button(title, &desc, "set-path", "Open", "", move |a| {
                if !path.exists() && path.extension().is_some() {
                    let _ = std::fs::create_dir_all(path.parent().unwrap_or(&path));
                    let _ = std::fs::write(&path, "");
                }
                if path.exists() {
                    open_uri(&path.to_string_lossy());
                } else {
                    a.toast(&format!("{} does not exist yet", path.display()), true);
                }
            });
        }
        p.group("Links");
        for (title, desc, url) in [
            ("Source code", "github.com/elyesghazel/brief", REPO.to_owned()),
            ("Releases", "Every version with its changes and downloads.", RELEASES.to_owned()),
            ("Report a problem", "Or ask for something. The log file helps.", format!("{REPO}/issues")),
        ] {
            p.link(title, desc, None, "", move |_| open_uri(&url));
        }
        p.group("License");
        p.info("MIT License", "Copyright (c) 2026 Elyes Ghazel. Free to use, change and share.", "MIT");
    }

    // ── changes ──────────────────────────────────────────────

    /// Writes a top-level key; a failure shows, the change stays for this session.
    fn settings_save(&self, key: &str, value: impl Into<toml::Value>) {
        if let Err(e) = config::set(key, value) {
            self.toast(&format!("{e} (applies until restart)"), true);
        }
    }

    fn set_font(&self, v: &str) {
        let v = v.trim();
        self.cfg.borrow_mut().font = if v.is_empty() { crate::platform::FONT.to_owned() } else { v.to_owned() };
        self.apply_font();
        let r = if v.is_empty() { config::unset("font") } else { config::set("font", v) };
        if let Err(e) = r {
            self.toast(&e, true);
        }
    }

    /// New sync settings: every worker restarts once the choice settled.
    fn settings_restart_all(self: &Rc<Self>) {
        let s = &self.settings;
        let g = s.restart_gen.get() + 1;
        s.restart_gen.set(g);
        let me = Rc::downgrade(self);
        glib::timeout_add_local_once(Duration::from_millis(900), move || {
            let Some(app) = me.upgrade() else { return };
            if app.settings.restart_gen.get() == g {
                let all: Vec<usize> = (0..app.accounts.borrow().len()).collect();
                app.restart_workers(all, None, |a, _| a.reload_list());
            }
        });
    }

    /// Writes one field of an account, applies it in memory and, with `restart`, restarts its
    /// sync with it.
    fn settings_account_set(self: &Rc<Self>, name: &str, path: &[&str], value: Option<toml::Value>, restart: bool) -> Result<(), String> {
        let i = self.acct_idx(name).ok_or_else(|| format!("{name} is gone"))?;
        config::set_account(name, path, value.clone())?;
        apply_field(&mut self.accounts.borrow_mut()[i], path, value.as_ref());
        if let Some(acc) = self.cfg.borrow_mut().accounts.iter_mut().find(|a| a.name == name) {
            apply_field(acc, path, value.as_ref());
        }
        if restart {
            self.settings.tests.borrow_mut().remove(&self.accounts.borrow()[i].email);
            self.restart_workers(vec![i], None, |_, _| {});
            self.settings_refresh();
        }
        Ok(())
    }

    fn settings_set_password(self: &Rc<Self>, name: &str, pass: String) -> Result<(), String> {
        let acc = self.accounts.borrow().iter().find(|a| a.name == name).cloned().ok_or_else(|| format!("{name} is gone"))?;
        if pass.is_empty() {
            return Err("type the new password".into());
        }
        if !acc.password.is_empty() {
            self.settings_account_set(name, &["password"], Some(toml::Value::String(pass)), true)?;
        } else {
            if !config::keyring_store(&acc.email, &pass) {
                config::secrets_file_store(&acc.email, &pass).map_err(|e| format!("could not store the password: {e}"))?;
            }
            if let Some(i) = self.acct_idx(name) {
                self.settings.tests.borrow_mut().remove(&acc.email);
                self.restart_workers(vec![i], None, |_, _| {});
            }
        }
        // the field never shows a password, not even the new one
        self.settings_refresh();
        self.toast(&format!("password for {name} saved, reconnecting…"), false);
        Ok(())
    }

    /// A new label. The cache is keyed by it, so the rows move along once the worker stopped.
    fn settings_rename(self: &Rc<Self>, old: &str, new: String) -> Result<(), String> {
        let new = new.trim().to_owned();
        if new.is_empty() {
            return Err("the label cannot be empty".into());
        }
        if new == old {
            return Ok(());
        }
        if self.accounts.borrow().iter().any(|a| a.name == new) {
            return Err(format!("there is already an account called {new}"));
        }
        let i = self.acct_idx(old).ok_or_else(|| format!("{old} is gone"))?;
        let email = self.accounts.borrow()[i].email.clone();
        if self.restarting.borrow().contains(&email) {
            return Err(format!("{old} is reconnecting, try again in a moment"));
        }
        config::set_account(old, &["name"], Some(toml::Value::String(new.clone())))?;
        let (o, n) = (old.to_owned(), new.clone());
        let between: super::CacheJob = Box::new(move |s: &mut Store| s.rename_account(&o, &n).map_err(|e| e.to_string()));
        let (o, n) = (old.to_owned(), new.clone());
        self.restart_workers(vec![i], Some(between), move |app, r| app.settings_renamed(&email, &o, &n, r));
        Ok(())
    }

    fn settings_renamed(self: &Rc<Self>, email: &str, old: &str, new: &str, result: Result<(), String>) {
        if let Err(e) = result {
            // the account syncs again under its new label
            crate::log::error(&format!("[{new}] moving the cache from {old}: {e}"));
        }
        for a in self.accounts.borrow_mut().iter_mut().filter(|a| a.email == email) {
            a.name = new.to_owned();
        }
        for a in self.cfg.borrow_mut().accounts.iter_mut().filter(|a| a.name == old) {
            a.name = new.to_owned();
        }
        {
            let mut view = self.view.borrow_mut();
            if let View::Folder { acct, .. } = &mut *view
                && *acct == old
            {
                *acct = new.to_owned();
            }
        }
        self.marked.borrow_mut().retain(|k| k.0 != old);
        if self.current.borrow().as_ref().is_some_and(|(r, _)| r.acct == old) {
            self.clear_reader();
        }
        self.mates.borrow_mut().clear();
        if self.settings.account.borrow().as_deref() == Some(old) {
            self.settings.account.replace(Some(new.to_owned()));
        }
        self.rebuild_side();
        self.reload_list();
        self.update_path();
        self.settings_refresh();
        self.toast(&format!("{old} is now {new}"), false);
    }

    fn settings_test(self: &Rc<Self>, name: &str) {
        let Some(acc) = self.accounts.borrow().iter().find(|a| a.name == name).cloned() else { return };
        self.settings.tests.borrow_mut().insert(acc.email.clone(), None);
        self.settings_refresh();
        let (tx, rx) = async_channel::unbounded::<Progress>();
        let a2 = acc.clone();
        std::thread::spawn(move || match a2.secret() {
            Ok(secret) => form::run_checks(&a2, &secret, &tx),
            Err(e) => {
                let _ = tx.send_blocking(Progress::Fail(Item::SignIn, e));
            }
        });
        let app = self.clone();
        glib::spawn_future_local(async move {
            let mut done = Vec::new();
            let mut result = None;
            while let Ok(p) = rx.recv().await {
                match p {
                    Progress::Ok(item, detail) => done.push(format!("{} ✓ {detail}", if item == Item::Smtp { "SMTP" } else { "IMAP" })),
                    Progress::Fail(item, e) => {
                        let provider = acc.is_oauth().then(|| Provider::from_name(&acc.oauth.provider)).flatten();
                        let server = if item == Item::Smtp { &acc.smtp } else { &acc.imap };
                        let (what, _, _) = form::explain(item, &e, server, provider, &form::service(&acc.email, provider, None));
                        crate::log::warn(&format!("[{}] connection test: {e}", acc.name));
                        let detail: String = e.chars().take(140).collect();
                        result = Some((false, format!("✗ {what} {detail}")));
                        break;
                    }
                    _ => {}
                }
            }
            let result = result.unwrap_or_else(|| (true, done.join("   ")));
            app.settings.tests.borrow_mut().insert(acc.email.clone(), Some(result));
            app.settings_refresh();
        });
    }

    fn settings_clear_cache(self: &Rc<Self>) {
        let all: Vec<usize> = (0..self.accounts.borrow().len()).collect();
        let started = self.restart_workers(all, Some(Box::new(|s: &mut Store| s.clear().map_err(|e| e.to_string()))), |app, r| {
            match r {
                Ok(()) => app.toast("cache cleared, syncing again…", false),
                Err(e) => app.toast(&format!("could not clear the cache: {e}"), true),
            }
            app.clear_reader();
            app.marked.borrow_mut().clear();
            app.mates.borrow_mut().clear();
            app.reload_list();
            app.rebuild_side();
            app.update_title();
            app.settings_refresh();
        });
        if !started {
            self.toast("an account is still reconnecting, try again in a moment", true);
        }
    }

    fn settings_check_update(self: &Rc<Self>) {
        if *self.settings.update.borrow() == Update::Checking {
            return;
        }
        self.settings.update.replace(Update::Checking);
        self.settings_refresh();
        let (tx, rx) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let _ = tx.send_blocking(latest_release());
        });
        let app = self.clone();
        glib::spawn_future_local(async move {
            let Ok(r) = rx.recv().await else { return };
            let state = match r {
                Ok((tag, url)) if newer(&tag, env!("CARGO_PKG_VERSION")) => Update::Newer(tag, url),
                Ok((tag, _)) => Update::Latest(tag),
                Err(e) => {
                    crate::log::warn(&format!("update check: {e}"));
                    Update::Failed(e)
                }
            };
            app.settings.update.replace(state);
            app.settings_refresh();
        });
    }

    // ── keys and pointer ─────────────────────────────────────

    pub fn settings_hints(&self) -> Vec<(&'static str, &'static str)> {
        let s = &self.settings;
        if s.in_filter.get() {
            return vec![("type", "to find"), ("Enter", "results"), ("Esc", "clear")];
        }
        let rows = s.rows.borrow();
        if let Some(i) = s.editing.borrow().as_ref().map(|(i, _)| *i) {
            let multi = rows.get(i).and_then(|r| r.edit.as_ref()).is_some_and(|e| e.multiline);
            return if multi { vec![("Ctrl+Enter", "keep"), ("Esc", "cancel")] } else { vec![("Enter", "keep"), ("Esc", "cancel")] };
        }
        let mut v = vec![("j k", "move")];
        match rows.get(s.cursor.get()).map(|r| r.hint).unwrap_or_default() {
            Hint::Toggle => v.push(("Enter", "switch")),
            Hint::Choice => v.push(("h l", "change")),
            Hint::Text | Hint::Multi => v.push(("Enter", "edit")),
            Hint::Number => v.extend([("h l", "adjust"), ("Enter", "type")]),
            Hint::Button => v.push(("Enter", "press")),
            Hint::Open => v.push(("Enter", "open")),
            Hint::Info => {}
        }
        v.extend([("Tab", "section"), ("/", "find")]);
        v.push(("Esc", if s.account.borrow().is_some() { "back" } else { "close" }));
        v
    }

    pub fn settings_key(self: &Rc<Self>, k: &str) -> bool {
        let s = &self.settings;
        if s.in_filter.get() {
            match k {
                "Escape" if s.filter.text().is_empty() => s.park_focus(),
                "Escape" => s.filter.set_text(""),
                "Return" | "KP_Enter" | "Down" | "Tab" | "C-j" | "C-n" => {
                    s.park_focus();
                    self.settings_select(0);
                }
                "C-k" | "C-p" => self.open_palette_main(),
                _ => return false,
            }
            return true;
        }
        let editing = s.editing.borrow().as_ref().map(|(i, _)| *i);
        if let Some(i) = editing {
            let multi = s.rows.borrow().get(i).and_then(|r| r.edit.as_ref()).is_some_and(|e| e.multiline);
            match k {
                "Escape" => self.settings_finish_edit(false, true),
                "C-Return" | "C-KP_Enter" => self.settings_finish_edit(true, true),
                "Return" | "KP_Enter" | "Tab" | "S-Tab" if !multi => self.settings_finish_edit(true, true),
                _ => return false,
            }
            return true;
        }
        let cur = s.cursor.get();
        let n = s.rows.borrow().len();
        let (step, open) = {
            let rows = s.rows.borrow();
            let r = rows.get(cur);
            (r.and_then(|r| r.step.clone()), r.is_some_and(|r| r.hint == Hint::Open))
        };
        match k {
            "j" | "Down" | "C-n" | "C-j" => self.settings_select(cur + 1),
            "k" | "Up" | "C-p" => self.settings_select(cur.saturating_sub(1)),
            "C-d" | "Page_Down" => self.settings_select(cur + 8),
            "C-u" | "Page_Up" => self.settings_select(cur.saturating_sub(8)),
            "g" | "Home" => self.settings_select(0),
            "G" | "End" => self.settings_select(n.saturating_sub(1)),
            "h" | "Left" => match step {
                Some(f) => f(self, -1),
                None if s.account.borrow().is_some() => self.settings_back(),
                None => {}
            },
            "l" | "Right" => match step {
                Some(f) => f(self, 1),
                None if open => self.settings_activate(s.page_gen.get(), cur),
                None => {}
            },
            "Return" | "KP_Enter" | "space" => self.settings_activate(s.page_gen.get(), cur),
            "Tab" | "C-Page_Down" | "]" => self.settings_go((s.section.get() + 1) % SECTIONS.len()),
            "S-Tab" | "C-Page_Up" | "[" => self.settings_go((s.section.get() + SECTIONS.len() - 1) % SECTIONS.len()),
            d if d.len() == 1 && ('1'..='7').contains(&d.chars().next().unwrap()) => self.settings_go(d.parse::<usize>().unwrap() - 1),
            "/" | "C-f" => {
                s.filter.grab_focus();
                s.filter.set_position(-1);
            }
            "Escape" | "q" => {
                if !s.filter.text().is_empty() {
                    s.filter.set_text("");
                } else if s.account.borrow().is_some() {
                    self.settings_back();
                } else {
                    self.close_settings();
                }
            }
            "," | "C-," => self.close_settings(),
            "C-k" | ":" => self.open_palette_main(),
            "?" | "F1" => self.show_help(),
            "C-r" | "F5" => self.refresh_all(),
            "A" => self.open_form(),
            // nothing else reaches a field that is not being edited
            _ => {}
        }
        true
    }

    fn settings_select(self: &Rc<Self>, i: usize) {
        let s = &self.settings;
        let n = s.rows.borrow().len();
        if n == 0 {
            return;
        }
        s.cursor.set(i.min(n - 1));
        self.settings_mark();
        self.settings_scroll_later();
        self.update_hints();
    }

    /// Highlights the current row.
    fn settings_mark(&self) {
        let s = &self.settings;
        for (j, r) in s.rows.borrow().iter().enumerate() {
            if j == s.cursor.get() {
                r.widget.add_css_class("cur");
            } else {
                r.widget.remove_css_class("cur");
            }
        }
    }

    fn settings_scroll_later(self: &Rc<Self>) {
        let me = Rc::downgrade(self);
        glib::idle_add_local_once(move || {
            if let Some(app) = me.upgrade() {
                app.settings_scroll();
            }
        });
    }

    /// Scrolls the current row into view, with some room around it.
    fn settings_scroll(&self) {
        let s = &self.settings;
        let rows = s.rows.borrow();
        let Some(r) = rows.get(s.cursor.get()) else { return };
        let Some(p) = r.widget.compute_point(&s.body, &graphene::Point::new(0.0, 0.0)) else { return };
        let adj = s.scroll.vadjustment();
        let (y, h) = (p.y() as f64, r.widget.height() as f64);
        let (top, page) = (adj.value(), adj.page_size());
        if s.cursor.get() == 0 {
            adj.set_value(0.0);
        } else if y < top + 48.0 {
            adj.set_value((y - 72.0).max(0.0));
        } else if y + h > top + page - 24.0 {
            adj.set_value(y + h - page + 72.0);
        }
    }

    /// Enter on a row, or a click on a link row.
    fn settings_activate(self: &Rc<Self>, page_gen: u64, i: usize) {
        let s = &self.settings;
        if page_gen != s.page_gen.get() {
            return;
        }
        let (edit, run) = {
            let rows = s.rows.borrow();
            let Some(r) = rows.get(i) else { return };
            (r.edit.is_some(), r.activate.clone())
        };
        if edit {
            self.settings_begin_edit(i, true);
        } else if let Some(run) = run {
            run(self);
        }
    }

    /// A press on a row: select it, ending an edit elsewhere.
    fn settings_pick(self: &Rc<Self>, page_gen: u64, i: usize) {
        let s = &self.settings;
        if page_gen != s.page_gen.get() {
            return;
        }
        if s.editing.borrow().as_ref().is_some_and(|(e, _)| *e != i) {
            let has_field = s.rows.borrow().get(i).is_some_and(|r| r.edit.is_some());
            self.settings_finish_edit(true, !has_field);
        }
        if s.in_filter.get() {
            s.park_focus();
        }
        if s.cursor.get() != i {
            s.cursor.set(i);
            self.settings_mark();
            self.update_hints();
        }
    }

    fn settings_focus_in(self: &Rc<Self>, page_gen: u64, i: usize) {
        let s = &self.settings;
        if page_gen != s.page_gen.get() || s.editing.borrow().as_ref().is_some_and(|(e, _)| *e == i) {
            return;
        }
        self.settings_finish_edit(true, false);
        s.cursor.set(i);
        self.settings_mark();
        self.settings_begin_edit(i, false);
    }

    fn settings_focus_out(self: &Rc<Self>, page_gen: u64, i: usize) {
        let s = &self.settings;
        if page_gen == s.page_gen.get() && s.editing.borrow().as_ref().is_some_and(|(e, _)| *e == i) {
            self.settings_finish_edit(true, false);
        }
    }

    fn settings_begin_edit(self: &Rc<Self>, i: usize, grab: bool) {
        let s = &self.settings;
        let (field, value) = {
            let rows = s.rows.borrow();
            let Some(e) = rows.get(i).and_then(|r| r.edit.as_ref()) else { return };
            (e.field.clone(), (e.get)())
        };
        s.editing.replace(Some((i, value)));
        if grab {
            field.grab_focus();
            if let Some(e) = field.downcast_ref::<gtk::Entry>() {
                e.set_position(-1);
            }
        }
        self.set_mode(Mode::Settings);
    }

    /// Ends an edit: keeps the value (`commit`) or puts the old one back. `park` takes the focus
    /// out of the field.
    pub fn settings_finish_edit(self: &Rc<Self>, commit: bool, park: bool) {
        let s = &self.settings;
        let Some((i, before)) = s.editing.take() else { return };
        let field = s.rows.borrow().get(i).and_then(|r| r.edit.as_ref()).map(|e| e.field.clone());
        if let Some(ed) = field.and_then(|f| f.dynamic_cast::<gtk::Editable>().ok()) {
            ed.select_region(0, 0);
            ed.set_position(0);
        }
        let fns = s.rows.borrow().get(i).and_then(|r| r.edit.as_ref()).map(|e| (e.get.clone(), e.set.clone(), e.commit.clone()));
        if let Some((get, set, save)) = fns {
            let now = get();
            if !commit {
                set(&before);
            } else if now != before
                && let Err(e) = save(self, now)
            {
                set(&before);
                self.toast(&e, true);
            }
        }
        if park {
            s.park_focus();
        }
        if self.mode.get() == Mode::Settings {
            self.set_mode(Mode::Settings);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare() {
        assert!(newer("v0.2.0", "0.1.0"));
        assert!(newer("0.1.10", "0.1.9"));
        assert!(newer("v1.0.0", "0.9.9"));
        assert!(!newer("v0.1.0", "0.1.0"));
        assert!(!newer("0.0.9", "0.1.0"));
        assert!(!newer("garbage", "0.1.0"));
    }

    #[test]
    fn written_fields_apply() {
        let (imap, smtp) = config::guess_servers("me@example.com");
        let mut acc = Account {
            name: "A".into(),
            email: "me@example.com".into(),
            display_name: String::new(),
            user: String::new(),
            imap,
            smtp,
            password_cmd: String::new(),
            password: String::new(),
            auth: config::Auth::Password,
            oauth: config::OAuth::default(),
            signature: String::new(),
            color: String::new(),
        };
        apply_field(&mut acc, &["imap", "port"], Some(&toml::Value::Integer(143)));
        apply_field(&mut acc, &["imap", "security"], Some(&toml::Value::String("starttls".into())));
        apply_field(&mut acc, &["signature"], Some(&toml::Value::String("-- \nMe".into())));
        apply_field(&mut acc, &["display_name"], None);
        assert_eq!((acc.imap.port, acc.imap.security), (143, Security::Starttls));
        assert_eq!(acc.signature, "-- \nMe");
        assert_eq!(text_value("  "), None);
    }
}
