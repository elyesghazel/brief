//! The composer's rich text editor: a TextView with tags for the inline styles and line kinds,
//! bullets, numbers and quote bars drawn beside the text, images as paintables. It reads into and
//! loads from `crate::rich::Doc`.

use super::App;
use crate::rich::{self, Block, Doc, Line, Run, Style};
use crate::smtp::Inline;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, gdk_pixbuf, gio, glib, graphene};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

const INLINE: [&str; 4] = ["b", "i", "u", "s"];
const BLOCKS: [(&str, Block); 5] = [("h1", Block::H1), ("h2", Block::H2), ("quote", Block::Quote), ("ul", Block::Bullet), ("ol", Block::Number)];
/// Link tags are named `link:<url>`.
const LINK: &str = "link:";
/// Longest side of an image as sent; larger ones are scaled down.
const SEND_MAX: i32 = 1600;
/// Widest an image shows in the editor.
const SHOW_MAX: i32 = 560;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct Editor {
        /// Bullet and number color, quote bar color.
        pub colors: RefCell<Option<(gdk::RGBA, gdk::RGBA)>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Editor {
        const NAME: &'static str = "BriefEditor";
        type Type = super::Editor;
        type ParentType = gtk::TextView;
    }

    impl ObjectImpl for Editor {}
    impl WidgetImpl for Editor {}

    impl TextViewImpl for Editor {
        fn snapshot_layer(&self, layer: gtk::TextViewLayer, snapshot: gtk::Snapshot) {
            self.parent_snapshot_layer(layer, snapshot.clone());
            if layer == gtk::TextViewLayer::BelowText {
                draw_marks(&self.obj(), &snapshot);
            }
        }
    }

    /// Bullets and numbers left of list lines, a bar left of quotes, for the visible lines only.
    fn draw_marks(view: &super::Editor, snapshot: &gtk::Snapshot) {
        let Some((mark, bar)) = view.imp().colors.borrow().clone() else { return };
        let table = view.buffer().tag_table();
        let (Some(ul), Some(ol), Some(quote)) = (table.lookup("ul"), table.lookup("ol"), table.lookup("quote")) else { return };
        let vis = view.visible_rect();
        let (mut it, _) = view.line_at_y(vis.y());
        let mut number: Option<u32> = None;
        loop {
            let (y, h) = view.line_yrange(&it);
            if y > vis.y() + vis.height() {
                break;
            }
            let loc = view.iter_location(&it);
            if it.has_tag(&quote) {
                snapshot.append_color(&bar, &graphene::Rect::new(loc.x() as f32 - 14.0, y as f32, 2.0, h as f32));
            }
            let text = if it.has_tag(&ol) {
                let n = number.map_or_else(
                    || {
                        let mut p = it.clone();
                        let mut n = 1;
                        while p.backward_line() && p.has_tag(&ol) {
                            n += 1;
                        }
                        n
                    },
                    |n| n + 1,
                );
                number = Some(n);
                Some(format!("{n}."))
            } else {
                number = None;
                it.has_tag(&ul).then(|| "•".to_owned())
            };
            if let Some(t) = text {
                let layout = view.create_pango_layout(Some(&t));
                let (w, lh) = layout.pixel_size();
                snapshot.save();
                snapshot.translate(&graphene::Point::new((loc.x() - 8 - w) as f32, (loc.y() + (loc.height() - lh) / 2) as f32));
                snapshot.append_layout(&layout, &mark);
                snapshot.restore();
            }
            if !it.forward_line() {
                break;
            }
        }
    }
}

glib::wrapper! {
    pub struct Editor(ObjectSubclass<imp::Editor>)
        @extends gtk::TextView, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Scrollable;
}

/// An image in the message: the bytes that go out and the smaller texture shown.
pub struct Image {
    pub cid: String,
    pub mime: String,
    pub data: Vec<u8>,
    pub texture: gdk::Texture,
}

pub struct Rich {
    pub view: Editor,
    /// The scroller with the editor; the link popover hangs off it.
    pub area: gtk::Box,
    pub bar: gtk::Box,
    tools: gtk::Box,
    buttons: Vec<(&'static str, gtk::Button)>,
    mode: gtk::Label,
    /// Rich mode; off = a plain text editor as before.
    pub on: Cell<bool>,
    /// Inline tags for the next typed text after Ctrl+B etc. without a selection.
    pending: RefCell<Option<Vec<String>>>,
    /// Set while brief itself fills the buffer.
    quiet: Cell<bool>,
    insert_at: Cell<i32>,
    joined: Cell<bool>,
    pub images: RefCell<Vec<Image>>,
    accent: RefCell<String>,
    pub link_pop: gtk::Popover,
    link_text: gtk::Entry,
    link_url: gtk::Entry,
    link_range: RefCell<Option<(gtk::TextMark, gtk::TextMark)>>,
}

fn tool(child: &impl IsA<gtk::Widget>, tip: &str) -> gtk::Button {
    let b = gtk::Button::new();
    b.add_css_class("fmt");
    b.set_focusable(false);
    b.set_tooltip_text(Some(tip));
    b.set_child(Some(child));
    b
}

fn sep() -> gtk::Separator {
    let s = gtk::Separator::new(gtk::Orientation::Vertical);
    s.add_css_class("fmt-sep");
    s
}

/// A unique content id for an inline image.
fn new_cid() -> String {
    static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    format!("img{n}.{t:x}@brief")
}

/// The bytes to send and the decoded picture. Small PNG, JPEG and GIF go out as they are;
/// anything over 1600px, rotated by EXIF or in another format is re-encoded (JPEG at quality 85,
/// PNG when it has transparency), scaled to 1600px on the long side.
pub fn prepare(bytes: &[u8]) -> Result<(Vec<u8>, String, gdk_pixbuf::Pixbuf), String> {
    let loader = gdk_pixbuf::PixbufLoader::new();
    loader.write(bytes).and_then(|_| loader.close()).map_err(|_| "not an image brief can read".to_owned())?;
    let raw = loader.pixbuf().ok_or("not an image brief can read")?;
    let mime = loader.format().and_then(|f| f.mime_types().first().map(|m| m.to_string())).unwrap_or_default();
    let rotated = raw.option("orientation").is_some_and(|o| o != "1");
    let pb = raw.apply_embedded_orientation().unwrap_or(raw);
    let (w, h) = (pb.width(), pb.height());
    if w.max(h) <= SEND_MAX && !rotated && matches!(mime.as_str(), "image/png" | "image/jpeg" | "image/gif") {
        return Ok((bytes.to_vec(), mime, pb));
    }
    let scaled = if w.max(h) > SEND_MAX {
        let f = SEND_MAX as f64 / w.max(h) as f64;
        pb.scale_simple(((w as f64 * f).round() as i32).max(1), ((h as f64 * f).round() as i32).max(1), gdk_pixbuf::InterpType::Hyper).ok_or("image too large")?
    } else {
        pb
    };
    let (kind, mime, opts): (&str, &str, &[(&str, &str)]) =
        if scaled.has_alpha() { ("png", "image/png", &[]) } else { ("jpeg", "image/jpeg", &[("quality", "85")]) };
    let data = scaled.save_to_bufferv(kind, opts).map_err(|e| e.to_string())?;
    Ok((data, mime.into(), scaled))
}

/// The editor's copy, at most `SHOW_MAX` wide.
fn shown(pb: &gdk_pixbuf::Pixbuf) -> gdk::Texture {
    let pb = if pb.width() > SHOW_MAX {
        pb.scale_simple(SHOW_MAX, (pb.height() * SHOW_MAX / pb.width()).max(1), gdk_pixbuf::InterpType::Bilinear).unwrap_or_else(|| pb.clone())
    } else {
        pb.clone()
    };
    #[allow(deprecated)]
    gdk::Texture::for_pixbuf(&pb)
}

fn line_start(it: &gtk::TextIter) -> gtk::TextIter {
    let mut s = it.clone();
    s.set_line_offset(0);
    s
}

/// The start of the line after `it`'s, so a range up to it includes the newline.
fn after_line(it: &gtk::TextIter) -> gtk::TextIter {
    let mut e = it.clone();
    if !e.ends_line() {
        e.forward_to_line_end();
    }
    e.forward_char();
    e
}

fn link_of(tag: &gtk::TextTag) -> Option<String> {
    tag.name().and_then(|n| n.strip_prefix(LINK).map(str::to_owned))
}

impl Rich {
    pub fn new() -> Rich {
        let view: Editor = glib::Object::new();
        view.add_css_class("editor");
        view.set_wrap_mode(gtk::WrapMode::WordChar);
        view.set_accepts_tab(false);
        view.set_has_tooltip(true);
        let buf = view.buffer();
        let table = buf.tag_table();
        let tag = |name: &str, f: &dyn Fn(&gtk::TextTag)| {
            let t = gtk::TextTag::new(Some(name));
            f(&t);
            table.add(&t);
        };
        tag("b", &|t| t.set_weight(700));
        tag("i", &|t| t.set_style(gtk::pango::Style::Italic));
        tag("u", &|t| t.set_underline(gtk::pango::Underline::Single));
        tag("s", &|t| t.set_strikethrough(true));
        tag("h1", &|t| {
            t.set_scale(1.5);
            t.set_weight(700);
            t.set_pixels_above_lines(6);
            t.set_pixels_below_lines(2);
        });
        tag("h2", &|t| {
            t.set_scale(1.22);
            t.set_weight(700);
            t.set_pixels_above_lines(4);
            t.set_pixels_below_lines(1);
        });
        tag("quote", &|t| t.set_left_margin(20));
        tag("ul", &|t| t.set_left_margin(26));
        tag("ol", &|t| t.set_left_margin(30));

        let scroll = gtk::ScrolledWindow::builder().child(&view).vexpand(true).hscrollbar_policy(gtk::PolicyType::Never).build();
        let area = gtk::Box::new(gtk::Orientation::Vertical, 0);
        area.append(&scroll);

        let bar = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        bar.add_css_class("compose-tools");
        let tools = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        let mut buttons = Vec::new();
        let icon = |name: &str| gtk::Image::from_icon_name(name);
        let text = |t: &str| {
            let l = gtk::Label::new(Some(t));
            l.add_css_class("fmt-text");
            l
        };
        let groups: [Vec<(&'static str, gtk::Widget, &str)>; 4] = [
            vec![
                ("b", icon("format-text-bold-symbolic").upcast(), "Bold  Ctrl+B"),
                ("i", icon("format-text-italic-symbolic").upcast(), "Italic  Ctrl+I"),
                ("u", icon("format-text-underline-symbolic").upcast(), "Underline  Ctrl+U"),
                ("s", icon("format-text-strikethrough-symbolic").upcast(), "Strikethrough  Ctrl+Shift+X"),
                ("link", icon("insert-link-symbolic").upcast(), "Link  Ctrl+L"),
            ],
            vec![
                ("h1", text("H1").upcast(), "Heading  Ctrl+Alt+1"),
                ("h2", text("H2").upcast(), "Subheading  Ctrl+Alt+2"),
                ("ul", icon("view-list-bullet-symbolic").upcast(), "Bulleted list  Ctrl+Shift+8"),
                ("ol", icon("view-list-ordered-symbolic").upcast(), "Numbered list  Ctrl+Shift+7"),
                ("quote", text("❝").upcast(), "Quote  Ctrl+Shift+9"),
            ],
            vec![
                ("image", icon("insert-image-symbolic").upcast(), "Image  Ctrl+Shift+P, or paste / drop one"),
                ("emoji", icon("face-smile-symbolic").upcast(), "Emoji  Ctrl+."),
            ],
            vec![("clear", icon("edit-clear-symbolic").upcast(), "Clear formatting  Ctrl+\\")],
        ];
        for (i, g) in groups.into_iter().enumerate() {
            if i > 0 {
                tools.append(&sep());
            }
            for (name, child, tip) in g {
                let b = tool(&child, tip);
                tools.append(&b);
                buttons.push((name, b));
            }
        }
        bar.append(&tools);
        let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        spacer.set_hexpand(true);
        bar.append(&spacer);
        let mode = text("rich text");
        let mode_btn = tool(&mode, "Rich or plain text  Ctrl+Shift+M");
        mode_btn.add_css_class("fmt-mode");
        bar.append(&mode_btn);
        buttons.push(("mode", mode_btn));

        let link_pop = gtk::Popover::new();
        link_pop.add_css_class("link-pop");
        link_pop.set_has_arrow(false);
        link_pop.set_position(gtk::PositionType::Bottom);
        let grid = gtk::Grid::new();
        grid.set_row_spacing(4);
        grid.set_column_spacing(10);
        let link_text = gtk::Entry::new();
        let link_url = gtk::Entry::new();
        link_url.set_placeholder_text(Some("https://… or name@example.com"));
        for (row, (name, e)) in [("Text", &link_text), ("Link", &link_url)].into_iter().enumerate() {
            e.add_css_class("field");
            e.set_width_chars(36);
            let l = super::label(name, "form-label");
            grid.attach(&l, 0, row as i32, 1, 1);
            grid.attach(e, 1, row as i32, 1, 1);
        }
        let hint = super::label("Enter applies · empty link removes it · Ctrl+Enter opens", "form-hint");
        grid.attach(&hint, 0, 2, 2, 1);
        link_pop.set_child(Some(&grid));
        link_pop.set_parent(&area);

        Rich {
            view,
            area,
            bar,
            tools,
            buttons,
            mode,
            on: Cell::new(true),
            pending: RefCell::new(None),
            quiet: Cell::new(false),
            insert_at: Cell::new(0),
            joined: Cell::new(false),
            images: RefCell::new(Vec::new()),
            accent: RefCell::new("#888888".into()),
            link_pop,
            link_text,
            link_url,
            link_range: RefCell::new(None),
        }
    }

    pub fn wire(&self, app: &Rc<App>) {
        let buf = self.view.buffer();
        let me = Rc::downgrade(app);
        buf.connect_insert_text(move |_, it, _| {
            if let Some(app) = me.upgrade() {
                app.compose.rich.insert_at.set(it.offset());
            }
        });
        let me = Rc::downgrade(app);
        buf.connect_local("insert-text", true, move |args| {
            let it = args.get(1).and_then(|v| v.get::<gtk::TextIter>().ok());
            if let (Some(app), Some(it)) = (me.upgrade(), it) {
                app.compose.rich.after_insert(it.offset());
            }
            None
        });
        let me = Rc::downgrade(app);
        buf.connect_delete_range(move |_, s, e| {
            if let Some(app) = me.upgrade() {
                app.compose.rich.joined.set(s.line() != e.line());
            }
        });
        let me = Rc::downgrade(app);
        buf.connect_local("delete-range", true, move |args| {
            let it = args.get(1).and_then(|v| v.get::<gtk::TextIter>().ok());
            if let (Some(app), Some(it)) = (me.upgrade(), it) {
                let r = &app.compose.rich;
                if r.joined.take() && r.on.get() && !r.quiet.get() {
                    let block = r.block_at(&it);
                    r.set_lines_block(&it, &it, block);
                }
            }
            None
        });
        let me = Rc::downgrade(app);
        buf.connect_mark_set(move |buf, _, mark| {
            if let Some(app) = me.upgrade()
                && *mark == buf.get_insert()
            {
                let r = &app.compose.rich;
                if !r.quiet.get() {
                    r.pending.take();
                }
                r.update_bar();
            }
        });
        let me = Rc::downgrade(app);
        buf.connect_changed(move |_| {
            if let Some(app) = me.upgrade() {
                app.compose.rich.update_bar();
            }
        });
        for (name, b) in &self.buttons {
            let me = Rc::downgrade(app);
            let name = *name;
            b.connect_clicked(move |_| {
                if let Some(app) = me.upgrade() {
                    app.rich_cmd(name);
                }
            });
        }
        // Ctrl+click opens a link
        let click = gtk::GestureClick::new();
        click.set_button(1);
        click.set_propagation_phase(gtk::PropagationPhase::Capture);
        let me = Rc::downgrade(app);
        click.connect_pressed(move |g, _, x, y| {
            let Some(app) = me.upgrade() else { return };
            if !g.current_event_state().contains(gdk::ModifierType::CONTROL_MASK) {
                return;
            }
            if let Some(url) = app.compose.rich.link_at_point(x, y) {
                g.set_state(gtk::EventSequenceState::Claimed);
                app.open_link(&url);
            }
        });
        self.view.add_controller(click);
        let me = Rc::downgrade(app);
        self.view.connect_query_tooltip(move |_, x, y, kbd, tip| {
            let Some(app) = me.upgrade() else { return false };
            match app.compose.rich.link_at_point(x as f64, y as f64).filter(|_| !kbd) {
                Some(url) => {
                    tip.set_text(Some(&format!("{url}\nCtrl+click opens · Ctrl+L edits")));
                    true
                }
                None => false,
            }
        });
        // dropped images go in at the drop point, other files become attachments
        let drop = gtk::DropTarget::new(glib::Type::INVALID, gdk::DragAction::COPY);
        drop.set_types(&[gdk::FileList::static_type(), gdk::Texture::static_type()]);
        drop.set_propagation_phase(gtk::PropagationPhase::Capture);
        let me = Rc::downgrade(app);
        drop.connect_drop(move |_, value, x, y| {
            let Some(app) = me.upgrade() else { return false };
            app.rich_drop(value, x, y)
        });
        self.view.add_controller(drop);
        // the popover is its own surface: the window's key handler does not see its keys
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let me = Rc::downgrade(app);
        keys.connect_key_pressed(move |_, key, _, mods| match me.upgrade() {
            Some(app) if app.rich_key(&super::keys::name(key, mods)) == Some(true) => glib::Propagation::Stop,
            _ => glib::Propagation::Proceed,
        });
        self.link_pop.add_controller(keys);
        let me = Rc::downgrade(app);
        self.link_pop.connect_closed(move |_| {
            if let Some(app) = me.upgrade() {
                let r = &app.compose.rich;
                if let Some((s, e)) = r.link_range.take() {
                    r.view.buffer().delete_mark(&s);
                    r.view.buffer().delete_mark(&e);
                }
                r.view.grab_focus();
            }
        });
    }

    /// Theme colors for links, quotes, bullets and the quote bar.
    pub fn recolor(&self, accent: &str, muted: &str, ghost: &str) {
        *self.accent.borrow_mut() = accent.to_owned();
        let table = self.view.buffer().tag_table();
        table.foreach(|t| {
            if link_of(t).is_some() {
                t.set_foreground(Some(accent));
            }
        });
        if let Some(q) = table.lookup("quote") {
            q.set_foreground(Some(muted));
        }
        let rgba = |s: &str| gdk::RGBA::parse(s).unwrap_or(gdk::RGBA::new(0.5, 0.5, 0.5, 1.0));
        *self.view.imp().colors.borrow_mut() = Some((rgba(muted), rgba(ghost)));
        self.view.queue_draw();
    }

    fn buf(&self) -> gtk::TextBuffer {
        self.view.buffer()
    }

    fn cursor(&self) -> gtk::TextIter {
        let buf = self.buf();
        buf.iter_at_mark(&buf.get_insert())
    }

    fn link_tag(&self, url: &str) -> gtk::TextTag {
        let table = self.buf().tag_table();
        let name = format!("{LINK}{url}");
        table.lookup(&name).unwrap_or_else(|| {
            let t = gtk::TextTag::new(Some(&name));
            t.set_foreground(Some(&self.accent.borrow()));
            t.set_underline(gtk::pango::Underline::Single);
            table.add(&t);
            t
        })
    }

    fn link_tags(&self) -> Vec<gtk::TextTag> {
        let mut out = Vec::new();
        self.buf().tag_table().foreach(|t| {
            if link_of(t).is_some() {
                out.push(t.clone());
            }
        });
        out
    }

    fn link_at(&self, it: &gtk::TextIter) -> Option<gtk::TextTag> {
        it.tags().into_iter().find(|t| link_of(t).is_some())
    }

    fn link_at_point(&self, x: f64, y: f64) -> Option<String> {
        let (bx, by) = self.view.window_to_buffer_coords(gtk::TextWindowType::Widget, x as i32, y as i32);
        let it = self.view.iter_at_location(bx, by)?;
        self.link_at(&it).and_then(|t| link_of(&t))
    }

    /// The line kind at `it`'s line.
    pub fn block_at(&self, it: &gtk::TextIter) -> Block {
        self.block_of_char(&line_start(it))
    }

    /// The line kind tagged on the character at `it`.
    fn block_of_char(&self, it: &gtk::TextIter) -> Block {
        let table = self.buf().tag_table();
        BLOCKS.iter().find(|(n, _)| table.lookup(n).is_some_and(|t| it.has_tag(&t))).map_or(Block::Para, |(_, b)| *b)
    }

    /// Gives the lines from `s`'s to `e`'s (newlines included) the kind `block`.
    fn set_lines_block(&self, s: &gtk::TextIter, e: &gtk::TextIter, block: Block) {
        let buf = self.buf();
        let (a, b) = (line_start(s), after_line(e));
        for (name, kind) in BLOCKS {
            if kind != block {
                buf.remove_tag_by_name(name, &a, &b);
            }
        }
        if let Some((name, _)) = BLOCKS.iter().find(|(_, k)| *k == block) {
            buf.apply_tag_by_name(name, &a, &b);
        }
    }

    /// The inline tags text typed between `s` and `e` takes: those of the character before (at
    /// a line start the one after); a link only continues inside it.
    fn typing_tags(&self, s: &gtk::TextIter, e: &gtk::TextIter) -> Vec<gtk::TextTag> {
        let src = if s.starts_line() {
            if e.ends_line() {
                return Vec::new();
            }
            e.clone()
        } else {
            let mut p = s.clone();
            p.backward_char();
            p
        };
        src.tags()
            .into_iter()
            .filter(|t| {
                let n = t.name().map(|n| n.to_string()).unwrap_or_default();
                INLINE.contains(&n.as_str()) || (n.starts_with(LINK) && !s.starts_line() && e.has_tag(t))
            })
            .collect()
    }

    /// The inline styles at the cursor, as tag names.
    fn current(&self) -> Vec<String> {
        if let Some(p) = self.pending.borrow().clone() {
            return p;
        }
        let c = self.cursor();
        self.typing_tags(&c, &c).iter().filter_map(|t| t.name().map(|n| n.to_string())).collect()
    }

    fn after_insert(&self, end: i32) {
        if self.quiet.get() || !self.on.get() {
            return;
        }
        let buf = self.buf();
        let table = buf.tag_table();
        let (s, e) = (buf.iter_at_offset(self.insert_at.get()), buf.iter_at_offset(end));
        let tags: Vec<gtk::TextTag> = match self.pending.take() {
            Some(names) => names.iter().filter_map(|n| table.lookup(n)).collect(),
            None => self.typing_tags(&s, &e),
        };
        // the kind of the line typed into: on its newline, or before the text on the last line
        let mut nl = e.clone();
        if !nl.ends_line() {
            nl.forward_to_line_end();
        }
        let block = if !nl.is_end() {
            self.block_of_char(&nl)
        } else if !s.starts_line() {
            let mut p = s.clone();
            p.backward_char();
            self.block_of_char(&p)
        } else {
            Block::Para
        };
        buf.remove_all_tags(&s, &e);
        for t in &tags {
            buf.apply_tag(t, &s, &e);
        }
        let (s, e) = (buf.iter_at_offset(self.insert_at.get()), buf.iter_at_offset(end));
        self.set_lines_block(&s, &e, block);
    }

    pub fn update_bar(&self) {
        if !self.on.get() || !self.bar.is_visible() {
            return;
        }
        let cur = self.current();
        let c = self.cursor();
        let block = self.block_at(&c);
        let link = self.link_at(&c).is_some();
        for (name, b) in &self.buttons {
            let on = match *name {
                "b" | "i" | "u" | "s" => cur.iter().any(|n| n == name),
                "link" => link,
                "h1" => block == Block::H1,
                "h2" => block == Block::H2,
                "ul" => block == Block::Bullet,
                "ol" => block == Block::Number,
                "quote" => block == Block::Quote,
                _ => false,
            };
            if on {
                b.add_css_class("on");
            } else {
                b.remove_css_class("on");
            }
        }
    }

    /// Shows the formatting buttons only in rich mode.
    pub fn set_on(&self, on: bool) {
        self.on.set(on);
        self.tools.set_visible(on);
        self.mode.set_text(if on { "rich text" } else { "plain text" });
        if !on {
            self.pending.take();
            let buf = self.buf();
            buf.remove_all_tags(&buf.start_iter(), &buf.end_iter());
        }
        self.update_bar();
    }

    pub fn toggle_inline(&self, name: &str) {
        let buf = self.buf();
        let Some(tag) = buf.tag_table().lookup(name) else { return };
        if let Some((s, e)) = buf.selection_bounds() {
            let mut it = s.clone();
            let all = s.has_tag(&tag) && {
                it.forward_to_tag_toggle(Some(&tag));
                it >= e
            };
            if all {
                buf.remove_tag(&tag, &s, &e);
            } else {
                buf.apply_tag(&tag, &s, &e);
            }
        } else {
            let mut cur = self.current();
            match cur.iter().position(|n| n == name) {
                Some(i) => {
                    cur.remove(i);
                }
                None => cur.push(name.to_owned()),
            }
            *self.pending.borrow_mut() = Some(cur);
        }
        self.update_bar();
    }

    /// Removes inline styles and links from the selection, or for the next typed text.
    pub fn clear(&self) {
        let buf = self.buf();
        match buf.selection_bounds() {
            Some((s, e)) => {
                for n in INLINE {
                    buf.remove_tag_by_name(n, &s, &e);
                }
                for t in self.link_tags() {
                    buf.remove_tag(&t, &s, &e);
                }
            }
            None => *self.pending.borrow_mut() = Some(Vec::new()),
        }
        self.update_bar();
    }

    /// An empty last line has no character to carry a line tag; give it a newline.
    fn ensure_taggable(&self) {
        let c = self.cursor();
        if c.is_end() && c.starts_line() {
            let buf = self.buf();
            self.quiet.set(true);
            buf.insert(&mut buf.end_iter(), "\n");
            self.quiet.set(false);
            let mut c = buf.end_iter();
            c.backward_char();
            buf.place_cursor(&c);
        }
    }

    /// Turns the selected lines (or the cursor's) into `block`, or back to text when they all are.
    pub fn toggle_block(&self, block: Block) {
        let buf = self.buf();
        let had_sel = buf.has_selection();
        if !had_sel {
            self.ensure_taggable();
        }
        let (s, mut e) = buf.selection_bounds().unwrap_or_else(|| (self.cursor(), self.cursor()));
        if had_sel && e.starts_line() && e.line() > s.line() {
            e.backward_char();
        }
        let mut it = line_start(&s);
        let mut all = true;
        while it.line() <= e.line() {
            all &= self.block_at(&it) == block;
            if !it.forward_line() {
                break;
            }
        }
        self.set_lines_block(&s, &e, if all { Block::Para } else { block });
        self.view.queue_draw();
        self.update_bar();
    }

    fn line_empty(it: &gtk::TextIter) -> bool {
        line_start(it).ends_line()
    }

    /// Enter: an empty list or quote line ends the list, a heading ends at its end.
    pub fn on_return(&self) -> bool {
        let buf = self.buf();
        if buf.has_selection() {
            return false;
        }
        let c = self.cursor();
        let block = self.block_at(&c);
        match block {
            Block::Para => false,
            Block::Bullet | Block::Number | Block::Quote if Self::line_empty(&c) => {
                self.set_lines_block(&c, &c, Block::Para);
                self.view.queue_draw();
                self.update_bar();
                true
            }
            _ => {
                buf.begin_user_action();
                buf.insert_at_cursor("\n");
                let c = self.cursor();
                if matches!(block, Block::H1 | Block::H2) {
                    if Self::line_empty(&c) {
                        self.set_lines_block(&c, &c, Block::Para);
                    }
                } else {
                    self.ensure_taggable();
                    let c = self.cursor();
                    self.set_lines_block(&c, &c, block);
                }
                buf.end_user_action();
                self.view.scroll_mark_onscreen(&buf.get_insert());
                self.view.queue_draw();
                true
            }
        }
    }

    /// Backspace at the start of a list, quote or heading line makes it text first.
    pub fn on_backspace(&self) -> bool {
        let c = self.cursor();
        if self.buf().has_selection() || !c.starts_line() || self.block_at(&c) == Block::Para {
            return false;
        }
        self.set_lines_block(&c, &c, Block::Para);
        self.view.queue_draw();
        self.update_bar();
        true
    }

    /// Space after `-`, `*`, `1.`, `>`, `#` or `##` at the start of a text line starts that block.
    pub fn on_space(&self) -> bool {
        let c = self.cursor();
        if self.buf().has_selection() || self.block_at(&c) != Block::Para {
            return false;
        }
        let s = line_start(&c);
        let block = match self.buf().text(&s, &c, false).as_str() {
            "-" | "*" => Block::Bullet,
            "1." => Block::Number,
            ">" => Block::Quote,
            "#" => Block::H1,
            "##" => Block::H2,
            _ => return false,
        };
        let buf = self.buf();
        buf.begin_user_action();
        buf.delete(&mut s.clone(), &mut c.clone());
        self.ensure_taggable();
        let c = self.cursor();
        self.set_lines_block(&c, &c, block);
        buf.end_user_action();
        self.view.queue_draw();
        self.update_bar();
        true
    }

    // ── links ────────────────────────────────────────────

    /// The link popover for the selection, the link under the cursor, or a new link.
    pub fn open_link(&self) {
        let buf = self.buf();
        let (s, e) = match buf.selection_bounds() {
            Some(r) => r,
            None => {
                let c = self.cursor();
                let mut before = c.clone();
                before.backward_char();
                match self.link_at(&c).or_else(|| self.link_at(&before)) {
                    Some(t) => {
                        let (mut s, mut e) = (c.clone(), c.clone());
                        if !s.starts_tag(Some(&t)) {
                            s.backward_to_tag_toggle(Some(&t));
                        }
                        if !e.ends_tag(Some(&t)) {
                            e.forward_to_tag_toggle(Some(&t));
                        }
                        (s, e)
                    }
                    None => (c.clone(), c),
                }
            }
        };
        let url = self.link_at(&s).filter(|t| e.has_tag(t) || e.ends_tag(Some(t))).and_then(|t| link_of(&t)).unwrap_or_default();
        self.link_text.set_text(&buf.text(&s, &e, false));
        self.link_url.set_text(&url);
        *self.link_range.borrow_mut() = Some((buf.create_mark(None, &s, true), buf.create_mark(None, &e, false)));
        let r = self.view.iter_location(&s);
        let (x, y) = self.view.buffer_to_window_coords(gtk::TextWindowType::Widget, r.x(), r.y());
        let p = self.view.compute_point(&self.area, &graphene::Point::new(x as f32, y as f32)).unwrap_or(graphene::Point::zero());
        self.link_pop.set_pointing_to(Some(&gdk::Rectangle::new(p.x() as i32, p.y() as i32, 1, r.height())));
        self.link_pop.popup();
        if self.link_text.text().is_empty() {
            self.link_text.grab_focus();
        } else {
            self.link_url.grab_focus();
        }
    }

    /// Enter in the popover: links the range, retitles it, or with an empty URL unlinks it.
    pub fn apply_link(&self) -> Result<(), String> {
        let buf = self.buf();
        let Some((sm, em)) = self.link_range.borrow().clone() else { return Ok(()) };
        let raw = self.link_url.text().trim().to_owned();
        let url = if raw.is_empty() { None } else { Some(rich::safe_url(&raw).ok_or_else(|| format!("not a web or mail link: {raw}"))?) };
        let (mut s, mut e) = (buf.iter_at_mark(&sm), buf.iter_at_mark(&em));
        let text = match self.link_text.text().to_string() {
            t if t.is_empty() => raw.clone(),
            t => t,
        };
        buf.begin_user_action();
        if url.is_some() && text != buf.text(&s, &e, false).as_str() {
            let tags: Vec<gtk::TextTag> = self.typing_tags(&s, &e).into_iter().filter(|t| link_of(t).is_none()).collect();
            let block = self.block_at(&s);
            buf.delete(&mut s, &mut e);
            self.quiet.set(true);
            buf.insert(&mut s, &text);
            self.quiet.set(false);
            s = buf.iter_at_mark(&sm);
            e = buf.iter_at_mark(&em);
            for t in &tags {
                buf.apply_tag(t, &s, &e);
            }
            self.set_lines_block(&s, &e, block);
            s = buf.iter_at_mark(&sm);
            e = buf.iter_at_mark(&em);
        }
        for t in self.link_tags() {
            buf.remove_tag(&t, &s, &e);
        }
        if let Some(url) = &url {
            buf.apply_tag(&self.link_tag(url), &s, &e);
        }
        buf.end_user_action();
        buf.place_cursor(&e);
        self.link_pop.popdown();
        Ok(())
    }

    pub fn popover_url(&self) -> Option<String> {
        rich::safe_url(&self.link_url.text())
    }

    // ── images ───────────────────────────────────────────

    /// Puts an image at the cursor.
    pub fn insert_image(&self, bytes: &[u8]) -> Result<(), String> {
        let (data, mime, pb) = prepare(bytes)?;
        let texture = shown(&pb);
        self.images.borrow_mut().push(Image { cid: new_cid(), mime, data, texture: texture.clone() });
        let buf = self.buf();
        buf.begin_user_action();
        buf.delete_selection(true, true);
        let block = self.block_at(&self.cursor());
        let mut it = self.cursor();
        buf.insert_paintable(&mut it, &texture);
        let c = self.cursor();
        self.set_lines_block(&c, &c, block);
        buf.end_user_action();
        self.view.grab_focus();
        Ok(())
    }

    fn cid_of(&self, p: &gdk::Paintable) -> Option<String> {
        self.images.borrow().iter().find(|i| i.texture.upcast_ref::<gdk::Paintable>() == p).map(|i| i.cid.clone())
    }

    // ── buffer <-> document ──────────────────────────────

    /// The buffer as a document.
    pub fn read(&self) -> Doc {
        let buf = self.buf();
        let mut doc = Vec::new();
        for n in 0..buf.line_count() {
            let Some(start) = buf.iter_at_line(n) else { continue };
            let mut end = start.clone();
            if !end.ends_line() {
                end.forward_to_line_end();
            }
            let mut line = Line::new(self.block_at(&start));
            let mut it = start;
            while it < end {
                let mut next = it.clone();
                next.forward_to_tag_toggle(None::<&gtk::TextTag>);
                if next > end {
                    next = end.clone();
                }
                let mut style = Style::default();
                for t in it.tags() {
                    match t.name().as_deref() {
                        Some("b") => style.bold = true,
                        Some("i") => style.italic = true,
                        Some("u") => style.underline = true,
                        Some("s") => style.strike = true,
                        _ => style.link = style.link.or_else(|| link_of(&t)),
                    }
                }
                let text = buf.slice(&it, &next, true);
                if text.contains('\u{fffc}') {
                    let mut p = it.clone();
                    let mut chunk = String::new();
                    for ch in text.chars() {
                        if ch == '\u{fffc}' {
                            line.push(&std::mem::take(&mut chunk), &style);
                            if let Some(cid) = p.paintable().and_then(|pt| self.cid_of(&pt)) {
                                line.runs.push(Run::Image(cid));
                            }
                        } else {
                            chunk.push(ch);
                        }
                        p.forward_char();
                    }
                    line.push(&chunk, &style);
                } else {
                    line.push(&text, &style);
                }
                it = next;
            }
            doc.push(line);
        }
        doc
    }

    /// Replaces the buffer with `doc` (not undoable). `images` holds the pictures it refers to.
    pub fn load(&self, doc: &Doc, images: Vec<Image>) {
        let buf = self.buf();
        self.quiet.set(true);
        buf.begin_irreversible_action();
        buf.set_text("");
        *self.images.borrow_mut() = images;
        let mut it = buf.start_iter();
        for (i, line) in doc.iter().enumerate() {
            let start = it.offset();
            for run in &line.runs {
                match run {
                    Run::Text(t, style) => {
                        let from = it.offset();
                        buf.insert(&mut it, t);
                        let end = it.offset();
                        let s = buf.iter_at_offset(from);
                        let mut names: Vec<String> = [(style.bold, "b"), (style.italic, "i"), (style.underline, "u"), (style.strike, "s")]
                            .into_iter()
                            .filter(|(on, _)| *on)
                            .map(|(_, n)| n.to_owned())
                            .collect();
                        if let Some(url) = &style.link {
                            names.push(self.link_tag(url).name().unwrap_or_default().to_string());
                        }
                        for n in names {
                            buf.apply_tag_by_name(&n, &s, &it);
                        }
                        it = buf.iter_at_offset(end);
                    }
                    Run::Image(cid) => {
                        let tex = self.images.borrow().iter().find(|im| &im.cid == cid).map(|im| im.texture.clone());
                        if let Some(tex) = tex {
                            buf.insert_paintable(&mut it, &tex);
                        }
                    }
                }
            }
            if i + 1 < doc.len() {
                buf.insert(&mut it, "\n");
            }
            if line.block != Block::Para {
                let end = it.offset();
                let s = buf.iter_at_offset(start);
                self.set_lines_block(&s, &s, line.block);
                it = buf.iter_at_offset(end);
            }
        }
        buf.end_irreversible_action();
        self.quiet.set(false);
        self.pending.take();
        self.view.queue_draw();
    }

    /// Plain text in, as it was before rich text.
    pub fn load_plain(&self, text: &str) {
        self.quiet.set(true);
        let buf = self.buf();
        buf.begin_irreversible_action();
        buf.set_text(text);
        buf.end_irreversible_action();
        self.images.borrow_mut().clear();
        self.quiet.set(false);
    }

    pub fn plain_text(&self) -> String {
        let buf = self.buf();
        buf.text(&buf.start_iter(), &buf.end_iter(), false).into()
    }

    /// The images `doc` uses, for sending.
    pub fn inline_parts(&self, doc: &Doc) -> Vec<Inline> {
        let used = rich::images(doc);
        let mut out: Vec<Inline> = Vec::new();
        for im in self.images.borrow().iter() {
            if used.contains(&im.cid.as_str()) && !out.iter().any(|o| o.cid == im.cid) {
                out.push(Inline { cid: im.cid.clone(), mime: im.mime.clone(), data: im.data.clone() });
            }
        }
        out
    }

    /// Decodes received images (content id, mime, bytes) for `load`.
    pub fn images_from(parts: &[(String, String, Vec<u8>)]) -> Vec<Image> {
        parts
            .iter()
            .filter_map(|(cid, mime, data)| {
                let loader = gdk_pixbuf::PixbufLoader::new();
                loader.write(data).ok()?;
                loader.close().ok()?;
                let pb = loader.pixbuf()?;
                Some(Image { cid: cid.clone(), mime: mime.clone(), data: data.clone(), texture: shown(&pb) })
            })
            .collect()
    }
}

impl App {
    /// One formatting command, from a key, the toolbar or the palette.
    pub fn rich_cmd(self: &Rc<Self>, cmd: &str) {
        let r = &self.compose.rich;
        match cmd {
            "mode" => return self.toggle_rich(),
            "bar" => return self.toggle_toolbar(),
            "image" => return self.pick_image(),
            _ if !r.on.get() => {
                self.toast("plain text mode: Ctrl+Shift+M switches to rich text", false);
                return;
            }
            "b" | "i" | "u" | "s" => r.toggle_inline(cmd),
            "h1" => r.toggle_block(Block::H1),
            "h2" => r.toggle_block(Block::H2),
            "ul" => r.toggle_block(Block::Bullet),
            "ol" => r.toggle_block(Block::Number),
            "quote" => r.toggle_block(Block::Quote),
            "para" => {
                let c = r.cursor();
                let (s, e) = r.buf().selection_bounds().unwrap_or((c.clone(), c));
                r.set_lines_block(&s, &e, Block::Para);
                r.view.queue_draw();
                r.update_bar();
            }
            "clear" => r.clear(),
            "link" => {
                r.view.grab_focus();
                r.open_link();
                return;
            }
            "open" => {
                let c = r.cursor();
                let mut before = c.clone();
                before.backward_char();
                match r.link_at(&c).or_else(|| r.link_at(&before)).and_then(|t| link_of(&t)) {
                    Some(url) => self.open_link(&url),
                    None => self.toast("no link at the cursor", false),
                }
            }
            "emoji" => {
                r.view.grab_focus();
                r.view.emit_insert_emoji();
                return;
            }
            _ => {}
        }
        r.view.grab_focus();
    }

    /// Keys for the editor; None = not a rich text key.
    pub fn rich_key(self: &Rc<Self>, k: &str) -> Option<bool> {
        let r = &self.compose.rich;
        if r.link_pop.is_visible() {
            match k {
                "Return" => {
                    if let Err(e) = r.apply_link() {
                        self.toast(&e, true);
                    }
                }
                "C-Return" => match r.popover_url() {
                    Some(u) => self.open_link(&u),
                    None => self.toast("no link to open", true),
                },
                "Escape" => r.link_pop.popdown(),
                _ => return Some(false),
            }
            return Some(true);
        }
        let cmd = match k {
            "C-S-m" => "mode",
            "C-S-f" => "bar",
            "C-S-p" => "image",
            _ if !r.view.has_focus() => return None,
            "C-b" => "b",
            "C-i" => "i",
            "C-u" => "u",
            "C-S-x" => "s",
            "C-S-7" => "ol",
            "C-S-8" => "ul",
            "C-S-9" => "quote",
            "C-M-1" => "h1",
            "C-M-2" => "h2",
            "C-M-0" => "para",
            "C-\\" => "clear",
            "C-l" => "link",
            _ if !r.on.get() => return None,
            "Return" | "S-Return" => return Some(r.on_return()),
            "BackSpace" => return Some(r.on_backspace()),
            "space" => return Some(r.on_space()),
            "C-v" => return Some(self.paste_image()),
            _ => return None,
        };
        self.rich_cmd(cmd);
        Some(true)
    }

    /// Ctrl+V with a picture (and no text) on the clipboard.
    fn paste_image(self: &Rc<Self>) -> bool {
        let cb = self.compose.rich.view.clipboard();
        let f = cb.formats();
        if !f.contains_type(gdk::Texture::static_type()) || f.contain_mime_type("text/plain") || f.contain_mime_type("text/plain;charset=utf-8") {
            return false;
        }
        let app = self.clone();
        cb.read_texture_async(None::<&gio::Cancellable>, move |res| match res {
            Ok(Some(tex)) => {
                if let Err(e) = app.compose.rich.insert_image(&tex.save_to_png_bytes()[..]) {
                    app.toast(&e, true);
                }
            }
            _ => app.toast("could not read the image on the clipboard", true),
        });
        true
    }

    fn pick_image(self: &Rc<Self>) {
        if !self.compose.rich.on.get() {
            self.toast("images need rich text: Ctrl+Shift+M", true);
            return;
        }
        let filter = gtk::FileFilter::new();
        filter.set_name(Some("Images"));
        filter.add_pixbuf_formats();
        let filters = gio::ListStore::new::<gtk::FileFilter>();
        filters.append(&filter);
        let dialog = gtk::FileDialog::builder().title("Insert image").modal(true).filters(&filters).build();
        let app = self.clone();
        dialog.open_multiple(Some(&self.win), None::<&gio::Cancellable>, move |res| {
            if let Ok(list) = res {
                for i in 0..list.n_items() {
                    if let Some(path) = list.item(i).and_downcast::<gio::File>().and_then(|f| f.path()) {
                        app.insert_image_file(&path);
                    }
                }
            }
            app.compose.rich.view.grab_focus();
        });
    }

    fn insert_image_file(&self, path: &std::path::Path) {
        let res = std::fs::read(path).map_err(|e| e.to_string()).and_then(|b| self.compose.rich.insert_image(&b));
        if let Err(e) = res {
            self.toast(&format!("{}: {e}", path.file_name().unwrap_or_default().to_string_lossy()), true);
        }
    }

    fn rich_drop(self: &Rc<Self>, value: &glib::Value, x: f64, y: f64) -> bool {
        let r = &self.compose.rich;
        let (bx, by) = r.view.window_to_buffer_coords(gtk::TextWindowType::Widget, x as i32, y as i32);
        if let Some(it) = r.view.iter_at_location(bx, by) {
            r.buf().place_cursor(&it);
        }
        if let Ok(tex) = value.get::<gdk::Texture>() {
            if r.on.get()
                && let Err(e) = r.insert_image(&tex.save_to_png_bytes()[..])
            {
                self.toast(&e, true);
            }
            return true;
        }
        let Ok(files) = value.get::<gdk::FileList>() else { return false };
        for path in files.files().iter().filter_map(|f| f.path()) {
            if r.on.get() && crate::smtp::mime_of(&path.to_string_lossy()).starts_with("image/") {
                self.insert_image_file(&path);
            } else {
                self.compose.files.borrow_mut().push(path);
            }
        }
        self.compose.update_files();
        true
    }

    /// Ctrl+Shift+M: rich text keeps the text and turns `>` lines into quotes; plain text keeps
    /// the plain form (lists as `- `, links as `text <url>`) and drops the rest.
    pub fn toggle_rich(self: &Rc<Self>) {
        let r = &self.compose.rich;
        let buf = r.buf();
        if r.on.get() {
            let doc = r.read();
            let lost = !rich::is_plain(&doc);
            let text = rich::to_plain(&doc, false);
            r.set_on(false);
            buf.begin_user_action();
            buf.set_text(text.trim_end_matches('\n'));
            buf.end_user_action();
            r.images.borrow_mut().clear();
            self.toast(if lost { "plain text: formatting removed (Ctrl+Z brings the text back)" } else { "plain text" }, false);
        } else {
            let doc = rich::from_plain(&r.plain_text(), false);
            r.set_on(true);
            r.load(&doc, Vec::new());
            self.toast("rich text", false);
        }
        buf.place_cursor(&buf.start_iter());
        r.view.grab_focus();
        self.update_hints();
    }

    fn toggle_toolbar(&self) {
        let bar = &self.compose.rich.bar;
        bar.set_visible(!bar.is_visible());
        self.cfg.borrow_mut().toolbar = bar.is_visible();
        crate::config::save_value("toolbar", if bar.is_visible() { "true" } else { "false" });
        self.compose.rich.update_bar();
    }
}
