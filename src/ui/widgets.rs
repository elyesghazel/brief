//! Small building blocks: emoji icons, sender avatars, icon buttons.

use crate::avatar;
use gtk::prelude::*;
use gtk::{gdk, gdk_pixbuf, glib, pango};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::mpsc::Sender;

/// The emoji for a folder role. It renders in whatever color emoji font fontconfig picks, which
/// is Apple's when ttf-apple-emoji is installed (see `apple_emoji` in platform/unix.rs).
fn emoji_char(name: &str) -> &'static str {
    match name {
        "inbox" => "📥",
        "sent" => "📤",
        "drafts" => "📝",
        "archive" => "🗄️",
        "trash" => "🗑️",
        "junk" => "🚫",
        "flagged" => "⭐",
        "all" => "📬",
        "unified" => "📨",
        "shield" => "🛡️",
        "clip" => "📎",
        _ => "📁",
    }
}

/// The app icon, drawn from the bundled 64px PNG so it is sharp on HiDPI too.
pub fn logo(px: i32) -> gtk::Image {
    let bytes = glib::Bytes::from_static(include_bytes!("../../assets/icons/hicolor/64x64/apps/ch.elyesghazel.brief.png"));
    let img = gtk::Image::from_paintable(gdk::Texture::from_bytes(&bytes).ok().as_ref());
    img.set_pixel_size(px);
    img.add_css_class("logo");
    img
}

/// An emoji icon about `px` pixels tall.
pub fn emoji(name: &str, px: i32) -> gtk::Label {
    let l = gtk::Label::new(Some(emoji_char(name)));
    let attrs = pango::AttrList::new();
    attrs.insert(pango::AttrSize::new_size_absolute(px * pango::SCALE * 4 / 5));
    l.set_attributes(Some(&attrs));
    l.add_css_class("emoji");
    l
}

pub fn icon_button(icon: &str, text: &str, tip: &str) -> gtk::Button {
    let b = gtk::Button::new();
    b.set_focusable(false);
    b.set_tooltip_text(Some(tip));
    let bx = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    bx.append(&gtk::Image::from_icon_name(icon));
    if !text.is_empty() {
        bx.append(&gtk::Label::new(Some(text)));
    }
    b.set_child(Some(&bx));
    b
}

/// Decoded favicons by organisation domain; None = known to have none.
pub struct Icons {
    textures: RefCell<HashMap<String, Option<gdk::Texture>>>,
    asked: RefCell<HashSet<String>>,
    fetch: Option<Sender<String>>,
}

fn decode(bytes: &[u8]) -> Option<gdk::Texture> {
    let loader = gdk_pixbuf::PixbufLoader::new();
    loader.write(bytes).ok()?;
    loader.close().ok()?;
    let pb = loader.pixbuf()?;
    // tiny or blank icons look worse than initials
    if pb.width() < 16 {
        return None;
    }
    let pb = if pb.width() > 96 { pb.scale_simple(96, 96 * pb.height() / pb.width().max(1), gdk_pixbuf::InterpType::Hyper)? } else { pb };
    #[allow(deprecated)]
    Some(gdk::Texture::for_pixbuf(&pb))
}

impl Icons {
    pub fn new(fetch: Option<Sender<String>>) -> Icons {
        Icons { textures: RefCell::new(HashMap::new()), asked: RefCell::new(HashSet::new()), fetch }
    }

    /// The sender's icon if there is one now; asks the fetcher once otherwise.
    pub fn get(&self, addr: &str) -> Option<gdk::Texture> {
        let domain = avatar::org_domain(addr)?;
        if let Some(t) = self.textures.borrow().get(&domain) {
            return t.clone();
        }
        let tex = match avatar::cached(&domain) {
            avatar::Cached::Icon(bytes) => decode(&bytes),
            avatar::Cached::None => None,
            avatar::Cached::Unknown => {
                if let Some(f) = &self.fetch
                    && self.asked.borrow_mut().insert(domain.clone()) {
                        let _ = f.send(domain);
                    }
                return None;
            }
        };
        self.textures.borrow_mut().insert(domain, tex.clone());
        tex
    }

    /// Forget a domain so the next `get` reads the fresh download.
    pub fn arrived(&self, domain: &str) {
        self.textures.borrow_mut().remove(domain);
    }
}

/// Round avatar: the favicon on a white disc, or initials on a colored one.
pub struct Avatar {
    pub root: gtk::Stack,
    initials: gtk::Label,
    image: gtk::Image,
    slot: RefCell<usize>,
}

impl Avatar {
    pub fn new(size: i32) -> Avatar {
        let root = gtk::Stack::new();
        root.add_css_class("avatar");
        root.set_size_request(size, size);
        root.set_valign(gtk::Align::Start);
        root.set_halign(gtk::Align::Center);
        root.set_overflow(gtk::Overflow::Hidden);
        let initials = gtk::Label::new(None);
        initials.add_css_class("avatar-initials");
        let image = gtk::Image::new();
        image.set_pixel_size(size * 2 / 3);
        image.add_css_class("avatar-icon");
        root.add_named(&initials, Some("initials"));
        root.add_named(&image, Some("icon"));
        if size >= 40 {
            root.add_css_class("avatar-large");
        }
        Avatar { root, initials, image, slot: RefCell::new(0) }
    }

    pub fn set(&self, icons: &Icons, name: &str, addr: &str) {
        let old = *self.slot.borrow();
        self.root.remove_css_class(&format!("avatar-c{old}"));
        self.root.remove_css_class("avatar-favicon");
        match icons.get(addr) {
            Some(t) => {
                self.image.set_paintable(Some(&t));
                self.root.add_css_class("avatar-favicon");
                self.root.set_visible_child_name("icon");
            }
            None => {
                let slot = avatar::color_slot(addr);
                *self.slot.borrow_mut() = slot;
                self.root.add_css_class(&format!("avatar-c{slot}"));
                self.initials.set_text(&avatar::initials(name, addr));
                self.root.set_visible_child_name("initials");
            }
        }
    }

    /// Finds the Avatar's widgets again from a row that was built with `new`.
    pub fn from_widget(root: &gtk::Stack) -> Avatar {
        let initials = root.child_by_name("initials").and_downcast::<gtk::Label>().unwrap();
        let image = root.child_by_name("icon").and_downcast::<gtk::Image>().unwrap();
        let slot = (0..8).find(|i| root.has_css_class(&format!("avatar-c{i}"))).unwrap_or(0);
        Avatar { root: root.clone(), initials, image, slot: RefCell::new(slot) }
    }
}
