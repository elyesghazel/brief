//! The reading pane: header with avatar and actions, the remote content banner, attachment chips,
//! and the body - HTML in a WebKit view (scripts off, remote loads blocked by a CSP until allowed)
//! or text in a TextView.

use super::widgets::{Avatar, emoji, icon_button};
use super::{App, Key, find_links, human_size, key, label, long_date, open_uri};
use crate::avatar;
use crate::imap::Cmd;
use crate::message::{self, Parsed};
use crate::store::Row;
use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;
use webkit6::prelude::*;

pub struct Reader {
    pub root: gtk::Stack,
    subject: gtk::Label,
    avatar: Avatar,
    from: gtk::Label,
    addr: gtk::Label,
    date: gtk::Label,
    meta: gtk::Label,
    star: gtk::Button,
    banner: gtk::Box,
    banner_text: gtk::Label,
    trust_btn: gtk::Button,
    chips: gtk::Box,
    chips_scroll: gtk::ScrolledWindow,
    note: gtk::Label,
    content: gtk::Stack,
    pub body: gtk::TextView,
    pub scroll: gtk::ScrolledWindow,
    paper: gtk::Box,
    web: RefCell<Option<webkit6::WebView>>,
    /// Remote content allowed for the message on screen.
    pub remote_ok: RefCell<Option<Key>>,
    zoom: Cell<f64>,
    buttons: Vec<(gtk::Button, &'static str)>,
}

impl Reader {
    pub fn new() -> Reader {
        let head = gtk::Box::new(gtk::Orientation::Vertical, 0);
        head.add_css_class("reader-head");
        let subject = label("", "r-subject");
        subject.set_wrap(true);
        subject.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        subject.set_selectable(true);
        subject.set_focusable(false);
        head.append(&subject);

        let who = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        who.add_css_class("r-who");
        let avatar = Avatar::new(42);
        who.append(&avatar.root);
        let names = gtk::Box::new(gtk::Orientation::Vertical, 2);
        names.set_valign(gtk::Align::Center);
        let line = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let from = label("", "r-from");
        from.set_ellipsize(gtk::pango::EllipsizeMode::End);
        let addr = label("", "r-addr");
        addr.set_ellipsize(gtk::pango::EllipsizeMode::End);
        addr.set_hexpand(true);
        addr.set_width_chars(1);
        line.append(&from);
        line.append(&addr);
        names.append(&line);
        let meta = label("", "r-meta");
        meta.set_ellipsize(gtk::pango::EllipsizeMode::End);
        meta.set_width_chars(1);
        names.append(&meta);
        names.set_hexpand(true);
        who.append(&names);
        let date = label("", "r-date");
        date.set_valign(gtk::Align::Start);
        who.append(&date);
        head.append(&who);

        let bar = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        bar.add_css_class("r-toolbar");
        let mut buttons = Vec::new();
        let mut add = |icon: &str, text: &str, tip: &str, action: &'static str| {
            let b = icon_button(icon, text, tip);
            b.add_css_class("tool");
            bar.append(&b);
            buttons.push((b.clone(), action));
            b
        };
        add("mail-reply-sender-symbolic", "Reply", "Reply (r)", "reply");
        add("mail-reply-all-symbolic", "", "Reply all (R)", "reply-all");
        add("mail-forward-symbolic", "", "Forward (f)", "forward");
        let sep = gtk::Separator::new(gtk::Orientation::Vertical);
        sep.add_css_class("tool-sep");
        bar.append(&sep);
        add("package-x-generic-symbolic", "", "Archive (e)", "archive");
        add("user-trash-symbolic", "", "Delete (d)", "delete");
        add("mail-mark-junk-symbolic", "", "Spam (!)", "spam");
        let star = add("non-starred-symbolic", "", "Star (s)", "star");
        add("mail-unread-symbolic", "", "Mark unread (u)", "unread");
        let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        spacer.set_hexpand(true);
        bar.append(&spacer);
        add("web-browser-symbolic", "", "Open in browser (o)", "browser");
        head.append(&bar);

        // remote content banner
        let banner = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        banner.add_css_class("banner");
        banner.append(&emoji("shield", 18));
        let banner_text = label("", "banner-text");
        banner_text.set_hexpand(true);
        banner_text.set_ellipsize(gtk::pango::EllipsizeMode::End);
        banner_text.set_width_chars(1);
        banner.append(&banner_text);
        let load_btn = icon_button("image-x-generic-symbolic", "Load", "Load remote content for this message (I)");
        load_btn.add_css_class("tool");
        banner.append(&load_btn);
        let trust_btn = gtk::Button::with_label("Always for sender");
        trust_btn.add_css_class("tool");
        trust_btn.set_focusable(false);
        banner.append(&trust_btn);
        buttons.push((load_btn, "load"));
        buttons.push((trust_btn.clone(), "trust"));

        // one row of chips, scrolling sideways when there are many (a FlowBox got cut off)
        let chips = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        chips.add_css_class("chips");
        let chips_scroll = gtk::ScrolledWindow::builder()
            .child(&chips)
            .hscrollbar_policy(gtk::PolicyType::Automatic)
            .vscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .build();
        chips_scroll.add_css_class("chips-scroll");
        let note = label("", "r-note");
        note.set_ellipsize(gtk::pango::EllipsizeMode::End);
        note.set_width_chars(1);

        let body = gtk::TextView::new();
        body.add_css_class("body");
        body.set_editable(false);
        body.set_cursor_visible(false);
        body.set_wrap_mode(gtk::WrapMode::WordChar);
        let scroll = gtk::ScrolledWindow::builder().child(&body).hscrollbar_policy(gtk::PolicyType::Never).vexpand(true).build();
        let paper = gtk::Box::new(gtk::Orientation::Vertical, 0);
        paper.add_css_class("paper");
        paper.set_overflow(gtk::Overflow::Hidden);
        paper.set_vexpand(true);
        let content = gtk::Stack::new();
        content.set_vexpand(true);
        content.add_named(&scroll, Some("text"));
        content.add_named(&paper, Some("html"));

        let msg = gtk::Box::new(gtk::Orientation::Vertical, 0);
        msg.append(&head);
        msg.append(&banner);
        msg.append(&chips_scroll);
        msg.append(&note);
        msg.append(&content);

        let empty = gtk::Box::new(gtk::Orientation::Vertical, 6);
        empty.add_css_class("empty");
        empty.set_valign(gtk::Align::Center);
        empty.append(&emoji("unified", 56));
        let t = label("Nothing selected", "empty-title");
        t.set_xalign(0.5);
        let d = label("j / k to move, Enter to read, c to write\n? shows every key", "");
        d.set_xalign(0.5);
        d.set_justify(gtk::Justification::Center);
        empty.append(&t);
        empty.append(&d);

        let root = gtk::Stack::new();
        root.set_vexpand(true);
        root.add_named(&empty, Some("empty"));
        root.add_named(&msg, Some("msg"));
        Reader {
            root,
            subject,
            avatar,
            from,
            addr,
            date,
            meta,
            star,
            banner,
            banner_text,
            trust_btn,
            chips,
            chips_scroll,
            note,
            content,
            body,
            scroll,
            paper,
            web: RefCell::new(None),
            remote_ok: RefCell::new(None),
            zoom: Cell::new(1.0),
            buttons,
        }
    }

    pub fn wire(&self, app: &Rc<App>) {
        for (b, action) in &self.buttons {
            let me = Rc::downgrade(app);
            let action = *action;
            b.connect_clicked(move |_| {
                let Some(app) = me.upgrade() else { return };
                match action {
                    "reply" => app.compose_reply(false),
                    "reply-all" => app.compose_reply(true),
                    "forward" => app.compose_forward(),
                    "archive" => app.move_to_role("archive", "archived"),
                    "delete" => app.delete(),
                    "spam" => app.move_to_role("junk", "marked as spam"),
                    "star" => app.toggle_star(),
                    "unread" => app.toggle_read(),
                    "browser" => app.open_in_browser(false),
                    "load" => app.load_remote(false),
                    "trust" => app.load_remote(true),
                    _ => {}
                }
            });
        }

        // links in text mail open on click
        let click = gtk::GestureClick::new();
        let tv = self.body.clone();
        click.connect_released(move |g, _, x, y| {
            if tv.buffer().has_selection() {
                return;
            }
            let (bx, by) = tv.window_to_buffer_coords(gtk::TextWindowType::Widget, x as i32, y as i32);
            let Some(iter) = tv.iter_at_location(bx, by) else { return };
            let Some(tag) = tv.buffer().tag_table().lookup("link") else { return };
            if iter.has_tag(&tag) {
                let mut start = iter;
                let mut end = iter;
                start.backward_to_tag_toggle(Some(&tag));
                end.forward_to_tag_toggle(Some(&tag));
                open_uri(tv.buffer().text(&start, &end, false).as_str());
                g.set_state(gtk::EventSequenceState::Claimed);
            }
        });
        self.body.add_controller(click);
    }

    /// The WebKit view, made on first use: ephemeral session, no page scripts, links open
    /// outside.
    fn web(&self) -> webkit6::WebView {
        if let Some(w) = self.web.borrow().as_ref() {
            return w.clone();
        }
        let session = webkit6::NetworkSession::new_ephemeral();
        let web = webkit6::WebView::builder().network_session(&session).build();
        if let Some(s) = WebViewExt::settings(&web) {
            s.set_enable_javascript_markup(false);
            s.set_enable_developer_extras(false);
            s.set_enable_page_cache(false);
            s.set_enable_html5_local_storage(false);
            s.set_enable_html5_database(false);
            s.set_enable_webgl(false);
            s.set_enable_webaudio(false);
            s.set_enable_media(false);
            s.set_media_playback_requires_user_gesture(true);
            s.set_allow_modal_dialogs(false);
            s.set_javascript_can_open_windows_automatically(false);
            s.set_default_font_family("sans-serif");
        }
        web.set_background_color(&gdk::RGBA::new(1.0, 1.0, 1.0, 1.0));
        web.set_vexpand(true);
        web.connect_decide_policy(|_, decision, kind| {
            match kind {
                webkit6::PolicyDecisionType::NavigationAction | webkit6::PolicyDecisionType::NewWindowAction => {
                    let Some(nav) = decision.downcast_ref::<webkit6::NavigationPolicyDecision>() else { return false };
                    let Some(action) = nav.navigation_action() else { return false };
                    let uri = action.request().and_then(|r| r.uri()).map(|u| u.to_string()).unwrap_or_default();
                    if uri.starts_with("about:") || uri.starts_with("data:") {
                        return false;
                    }
                    // every real navigation goes to the browser; the view never leaves the mail
                    if action.navigation_type() == webkit6::NavigationType::LinkClicked || kind == webkit6::PolicyDecisionType::NewWindowAction {
                        open_uri(&uri);
                    }
                    decision.ignore();
                    true
                }
                _ => false,
            }
        });
        // no "reload", "inspect" and friends in the right-click menu
        web.connect_context_menu(|_, menu, _| {
            let keep = [webkit6::ContextMenuAction::CopyLinkToClipboard, webkit6::ContextMenuAction::Copy, webkit6::ContextMenuAction::OpenLink];
            for item in menu.items() {
                if !keep.contains(&item.stock_action()) {
                    menu.remove(&item);
                }
            }
            false
        });
        // a wide mail must never force the window wider (it would clip in a tiled layout)
        let holder = gtk::ScrolledWindow::builder()
            .child(&web)
            .hscrollbar_policy(gtk::PolicyType::External)
            .vscrollbar_policy(gtk::PolicyType::External)
            .vexpand(true)
            .hexpand(true)
            .build();
        self.paper.append(&holder);
        *self.web.borrow_mut() = Some(web.clone());
        web
    }

    fn set_star(&self, on: bool) {
        let icon = if on { "starred-symbolic" } else { "non-starred-symbolic" };
        if let Some(img) = self.star.child().and_then(|b| b.first_child()).and_downcast::<gtk::Image>() {
            img.set_icon_name(Some(icon));
        }
        if on {
            self.star.add_css_class("on");
        } else {
            self.star.remove_css_class("on");
        }
    }
}

impl App {
    pub fn clear_reader(&self) {
        *self.current.borrow_mut() = None;
        self.rd.root.set_visible_child_name("empty");
    }

    pub fn show_message(self: &Rc<Self>, row: Row) {
        let raw = self.store.borrow().raw(&row.acct, &row.folder, row.uid);
        let same = self.current.borrow().as_ref().is_some_and(|(r, p)| key(r) == key(&row) && p.is_some());
        let parsed = if same {
            self.current.borrow().as_ref().and_then(|(_, p)| p.clone())
        } else {
            raw.as_deref().and_then(message::parse).map(|mut p| {
                if p.date == 0 {
                    p.date = row.date;
                }
                Rc::new(p)
            })
        };
        if !same {
            self.html_as_text.set(false);
            *self.rd.remote_ok.borrow_mut() = None;
        }
        let rd = &self.rd;
        rd.root.set_visible_child_name("msg");
        rd.subject.set_text(&row.subject);
        rd.from.set_text(if row.from_name.is_empty() { &row.from_addr } else { &row.from_name });
        rd.addr.set_text(if row.from_name.is_empty() { "" } else { &row.from_addr });
        rd.date.set_text(&long_date(row.date));
        rd.avatar.set(&self.icons, &row.from_name, &row.from_addr);
        rd.set_star(row.flagged);
        while let Some(c) = rd.chips.first_child() {
            rd.chips.remove(&c);
        }
        rd.banner.set_visible(false);
        match &parsed {
            Some(p) => {
                let mut meta = format!("to {}", p.to.join(", "));
                if !p.cc.is_empty() {
                    meta += &format!("  ·  cc {}", p.cc.join(", "));
                }
                rd.meta.set_text(&meta);
                rd.meta.set_tooltip_text(Some(&meta));
                for (i, a) in p.attachments.iter().enumerate() {
                    let b = gtk::Button::new();
                    b.add_css_class("chip");
                    b.set_focusable(false);
                    b.set_tooltip_text(Some("Click to open, a for more"));
                    let bx = gtk::Box::new(gtk::Orientation::Horizontal, 6);
                    bx.append(&emoji("clip", 16));
                    let n = label(&a.name, "chip-name");
                    n.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
                    n.set_max_width_chars(28);
                    bx.append(&n);
                    bx.append(&label(&human_size(a.size), "chip-size"));
                    b.set_child(Some(&bx));
                    let me = Rc::downgrade(self);
                    b.connect_clicked(move |_| {
                        if let Some(app) = me.upgrade() {
                            app.attachment_action(i, false);
                        }
                    });
                    rd.chips.append(&b);
                }
                rd.chips_scroll.set_visible(!p.attachments.is_empty());
                let use_html = p.html.is_some() && self.cfg.borrow().html && !self.html_as_text.get();
                if use_html {
                    self.show_html(&row, p);
                } else {
                    let text = p.text.clone();
                    let note = match (p.html.is_some(), p.from_html) {
                        (true, true) => "HTML mail as text · H shows the HTML",
                        (true, false) => "Text version · H shows the HTML",
                        _ => "",
                    };
                    rd.note.set_text(note);
                    rd.note.set_visible(!note.is_empty());
                    self.render_body(&text);
                    rd.content.set_visible_child_name("text");
                }
            }
            None => {
                rd.meta.set_text(&format!("to {}", row.to));
                rd.chips_scroll.set_visible(false);
                rd.note.set_visible(true);
                rd.note.set_text("loading…");
                rd.body.buffer().set_text(&row.preview);
                rd.content.set_visible_child_name("text");
                self.links.borrow_mut().clear();
                if let Some(i) = self.acct_idx(&row.acct) {
                    self.send(i, Cmd::Body(row.folder.clone(), row.uid));
                }
            }
        }
        if !same {
            rd.scroll.vadjustment().set_value(0.0);
        }
        let unseen = !row.seen;
        *self.current.borrow_mut() = Some((row.clone(), parsed));
        if unseen {
            // read after a short look, so skimming with j/k does not mark everything
            let me = Rc::downgrade(self);
            let k = key(&row);
            glib::timeout_add_local_once(Duration::from_millis(700), move || {
                let Some(app) = me.upgrade() else { return };
                let still = app.current.borrow().as_ref().is_some_and(|(r, _)| key(r) == k);
                if still {
                    app.set_seen(&[row], true);
                }
            });
        }
    }

    pub fn refresh_reader_avatar(&self) {
        if let Some((row, _)) = self.current.borrow().as_ref() {
            self.rd.avatar.set(&self.icons, &row.from_name, &row.from_addr);
        }
    }

    fn sender_key(row: &Row) -> String {
        avatar::org_domain(&row.from_addr).unwrap_or_else(|| row.from_addr.to_lowercase())
    }

    fn show_html(&self, row: &Row, p: &Parsed) {
        let rd = &self.rd;
        let html = p.html.as_deref().unwrap_or("");
        let sender = Self::sender_key(row);
        let trusted = self.store.borrow().is_trusted(&sender);
        let allowed = trusted || rd.remote_ok.borrow().as_ref().is_some_and(|k| *k == key(row));
        let remote = message::count_remote(html);
        if remote > 0 && !allowed {
            let what = if remote == 1 { "1 remote image".to_owned() } else { format!("{remote} remote images") };
            rd.banner_text.set_text(&format!("{what} blocked"));
            rd.banner_text.set_tooltip_text(Some("Remote images can tell the sender when and where you opened the mail. I loads them."));
            rd.trust_btn.set_tooltip_text(Some(&format!("Always load remote content from {sender}")));
            rd.banner.set_visible(true);
        }
        rd.note.set_visible(false);
        let web = rd.web();
        web.set_zoom_level(rd.zoom.get());
        web.load_html(&message::prepare_html(html, &p.inline, allowed), Some("about:blank"));
        rd.content.set_visible_child_name("html");
        // links for L come from the text version
        *self.links.borrow_mut() = find_links(&p.text).into_iter().map(|(s, e)| p.text[s..e].to_owned()).collect();
        let html_links = message::html_to_text(html, 200);
        for (s, e) in find_links(&html_links) {
            let l = html_links[s..e].to_owned();
            if !self.links.borrow().contains(&l) {
                self.links.borrow_mut().push(l);
            }
        }
    }

    /// The banner's buttons and `I`: load remote content now, optionally always for the sender.
    pub fn load_remote(self: &Rc<Self>, always: bool) {
        let Some((row, Some(p))) = self.current.borrow().clone() else { return };
        if always {
            let sender = Self::sender_key(&row);
            self.store.borrow().set_trusted(&sender, true);
            self.toast(&format!("remote content always loads for {sender}"), false);
        }
        *self.rd.remote_ok.borrow_mut() = Some(key(&row));
        self.rd.banner.set_visible(false);
        if p.html.is_some() {
            self.html_as_text.set(false);
            self.show_html(&row, &p);
        }
    }

    pub fn render_body(&self, text: &str) {
        let buf = self.rd.body.buffer();
        buf.set_text("");
        let mut iter = buf.end_iter();
        let mut links = Vec::new();
        for line in text.replace("\r\n", "\n").split_inclusive('\n') {
            let depth = line.chars().take_while(|c| *c == '>' || *c == ' ').filter(|c| *c == '>').count();
            let quote = match depth {
                0 => None,
                1 => Some("quote"),
                _ => Some("quote2"),
            };
            let mut pos = 0;
            for (s, e) in find_links(line) {
                match quote {
                    Some(q) => buf.insert_with_tags_by_name(&mut iter, &line[pos..s], &[q]),
                    None => buf.insert(&mut iter, &line[pos..s]),
                }
                buf.insert_with_tags_by_name(&mut iter, &line[s..e], &["link"]);
                links.push(line[s..e].to_owned());
                pos = e;
            }
            match quote {
                Some(q) => buf.insert_with_tags_by_name(&mut iter, &line[pos..], &[q]),
                None => buf.insert(&mut iter, &line[pos..]),
            }
        }
        links.dedup();
        *self.links.borrow_mut() = links;
    }

    /// Draws the open message again, keeping the HTML/text choice and allowed remote content.
    pub fn rerender(self: &Rc<Self>) {
        let row = self.current.borrow().as_ref().map(|(r, _)| r.clone());
        if let Some(row) = row {
            self.show_message(row);
        }
    }

    fn html_visible(&self) -> Option<webkit6::WebView> {
        (self.rd.content.visible_child_name().as_deref() == Some("html")).then(|| self.rd.web.borrow().clone()).flatten()
    }

    pub fn reader_scroll(&self, pages: f64) {
        if let Some(web) = self.html_visible() {
            let js = if pages.abs() < 0.2 { format!("window.scrollBy(0,{})", 70.0 * pages.signum()) } else { format!("window.scrollBy(0,window.innerHeight*{})", pages * 0.9) };
            web.evaluate_javascript(&js, None, None, None::<&gio::Cancellable>, |_| {});
            return;
        }
        let adj = self.rd.scroll.vadjustment();
        let step = if pages.abs() < 0.2 { 60.0 * pages.signum() } else { adj.page_size() * pages * 0.9 };
        adj.set_value((adj.value() + step).clamp(0.0, adj.upper() - adj.page_size()));
    }

    pub fn reader_top(&self, bottom: bool) {
        if let Some(web) = self.html_visible() {
            let js = if bottom { "window.scrollTo(0,document.body.scrollHeight)" } else { "window.scrollTo(0,0)" };
            web.evaluate_javascript(js, None, None, None::<&gio::Cancellable>, |_| {});
            return;
        }
        let adj = self.rd.scroll.vadjustment();
        adj.set_value(if bottom { adj.upper() } else { 0.0 });
    }

    pub fn zoom(&self, delta: f64) {
        let z = if delta == 0.0 { 1.0 } else { (self.rd.zoom.get() + delta).clamp(0.5, 2.5) };
        self.rd.zoom.set(z);
        if let Some(w) = self.rd.web.borrow().as_ref() {
            w.set_zoom_level(z);
        }
        self.toast(&format!("zoom {:.0}%", z * 100.0), false);
    }

    /// Text tags for plain mail, colored from the theme.
    pub fn setup_tags(&self) {
        let css = crate::theme::css(&self.theme_name.borrow());
        let color = |name: &str| {
            css.lines()
                .filter_map(|l| {
                    let rest = l.trim().strip_prefix("@define-color")?;
                    let (k, v) = rest.trim().split_once(char::is_whitespace)?;
                    (k == name).then(|| v.trim().trim_end_matches(';').to_owned())
                })
                .next_back()
                .unwrap_or_else(|| "#888888".into())
        };
        let table = self.rd.body.buffer().tag_table();
        for (name, fg, underline) in [("link", color("accent"), true), ("quote", color("faint"), false), ("quote2", color("ghost"), false)] {
            let tag = table.lookup(name).unwrap_or_else(|| {
                let t = gtk::TextTag::new(Some(name));
                table.add(&t);
                t
            });
            tag.set_foreground(Some(&fg));
            if underline {
                tag.set_underline(gtk::pango::Underline::Single);
            }
        }
    }
}
