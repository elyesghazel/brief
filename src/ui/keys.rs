//! Every key goes through one capture-phase handler on the window. Text fields get their keys
//! unless it is a command chord; in normal mode nothing reaches the widgets.

use super::{App, Mode, Pane};
use crate::imap::Cmd;
use crate::store::View;
use gtk::prelude::*;
use gtk::{gdk, glib};
use std::rc::Rc;

/// "j", "G", "C-r", "C-S-r", "Return", "S-Tab", "space"...
pub fn name(key: gdk::Key, mods: gdk::ModifierType) -> String {
    let ctrl = mods.contains(gdk::ModifierType::CONTROL_MASK);
    let shift = mods.contains(gdk::ModifierType::SHIFT_MASK);
    let alt = mods.contains(gdk::ModifierType::ALT_MASK);
    let mut out = String::new();
    if ctrl {
        out += "C-";
    }
    if alt {
        out += "M-";
    }
    match key.to_unicode().filter(|c| !c.is_control() && *c != ' ') {
        Some(c) if ctrl || alt => {
            if shift {
                out += "S-";
            }
            out.extend(c.to_lowercase());
        }
        Some(c) => out.push(c),
        None => {
            if shift {
                out += "S-";
            }
            let n = key.name().map(|n| n.to_string()).unwrap_or_default();
            out += match n.as_str() {
                "ISO_Left_Tab" => "Tab",
                "KP_Enter" => "Return",
                other => other,
            };
        }
    }
    out
}

/// The digit on a key's unshifted level, if it has one.
fn digit(code: u32) -> Option<char> {
    let keys = gdk::Display::default()?.map_keycode(code)?;
    keys.into_iter().filter(|(k, _)| k.level() == 0).find_map(|(_, v)| v.to_unicode().filter(char::is_ascii_digit))
}

fn is_text_focus(app: &App) -> bool {
    let Some(f) = gtk::prelude::GtkWindowExt::focus(&app.win) else { return false };
    f.is::<gtk::Text>() || f.downcast_ref::<gtk::TextView>().is_some_and(|t| t.is_editable())
}

pub fn install(app: &Rc<App>) {
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    let me = Rc::downgrade(app);
    keys.connect_key_pressed(move |_, key, code, mods| {
        let Some(app) = me.upgrade() else { return glib::Propagation::Proceed };
        let mut k = name(key, mods);
        // Ctrl+Shift+7/8/9 by position, whatever symbol Shift puts on the digit in this layout
        let ctrl_shift = gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::SHIFT_MASK;
        if mods & (ctrl_shift | gdk::ModifierType::ALT_MASK) == ctrl_shift
            && let Some(d) = digit(code).or(match k.as_str() {
                // no keymap (Broadway): the US symbols
                "C-S-&" => Some('7'),
                "C-S-*" => Some('8'),
                "C-S-(" => Some('9'),
                _ => None,
            })
        {
            k = format!("C-S-{d}");
        }
        if std::env::var_os("BRIEF_DEBUG_KEYS").is_some() {
            eprintln!("key {k} code {code} mode {:?} pane {:?}", app.mode.get(), app.focus.get());
        }
        if handle(&app, &k) { glib::Propagation::Stop } else { glib::Propagation::Proceed }
    });
    app.win.add_controller(keys);
}

/// Returns true when the key was used.
fn handle(app: &Rc<App>, k: &str) -> bool {
    if matches!(k, "Shift_L" | "Shift_R" | "Control_L" | "Control_R" | "Alt_L" | "Alt_R" | "Super_L" | "Super_R") {
        return false;
    }
    if k == "C-q" {
        app.win.close();
        return true;
    }
    match app.mode.get() {
        Mode::Help => {
            app.hide_overlay();
            true
        }
        Mode::Palette => app.palette_key(k),
        Mode::Confirm => app.confirm_key(k),
        Mode::Form => app.form_key(k),
        Mode::Compose => app.compose_key(k),
        Mode::Search => match k {
            "Escape" => {
                if app.search.text().is_empty() {
                    app.set_mode(Mode::Normal);
                    app.set_focus(Pane::List);
                } else {
                    app.search.set_text("");
                }
                true
            }
            "Down" | "C-j" | "C-n" | "Tab" => {
                app.set_mode(Mode::Normal);
                app.set_focus(Pane::List);
                app.select_index(0);
                true
            }
            "C-k" | "C-p" => {
                app.open_palette_main();
                true
            }
            _ => false,
        },
        Mode::Normal => {
            if is_text_focus(app) && !k.starts_with("C-") && k != "Escape" {
                return false;
            }
            normal(app, k)
        }
    }
}

fn normal(app: &Rc<App>, k: &str) -> bool {
    // two-key sequences: gg, gi, gs ...
    let pending = std::mem::take(&mut *app.pending.borrow_mut());
    if pending == "g" {
        go(app, k);
        return true;
    }
    let pane = app.focus.get();
    match (k, pane) {
        ("g", _) => *app.pending.borrow_mut() = "g".into(),

        // panes
        ("h" | "Left", Pane::List) => app.set_focus(Pane::Side),
        ("h" | "Left" | "Escape" | "BackSpace", Pane::Reader) => app.set_focus(Pane::List),
        ("l" | "Right" | "Return", Pane::Side) => app.set_focus(Pane::List),
        ("l" | "Right" | "Return", Pane::List) => {
            if app.selected_index().is_none() {
                app.select_index(0);
            }
            app.set_focus(Pane::Reader);
        }
        ("Tab", _) => app.set_focus(match pane {
            Pane::Side => Pane::List,
            Pane::List => Pane::Reader,
            Pane::Reader => Pane::Side,
        }),
        ("S-Tab", _) => app.set_focus(match pane {
            Pane::Side => Pane::Reader,
            Pane::List => Pane::Side,
            Pane::Reader => Pane::List,
        }),

        // moving
        ("j" | "Down", Pane::Side) => app.side_step(1),
        ("k" | "Up", Pane::Side) => app.side_step(-1),
        ("j" | "Down", Pane::List) => app.list_step(1),
        ("k" | "Up", Pane::List) => app.list_step(-1),
        ("j" | "Down", Pane::Reader) => app.reader_scroll(0.1),
        ("k" | "Up", Pane::Reader) => app.reader_scroll(-0.1),
        ("J", _) => app.list_step(1),
        ("K", _) => app.list_step(-1),
        ("n", _) if app.threads() => app.conv_step(1),
        ("p", _) if app.threads() => app.conv_step(-1),
        ("C-d" | "Page_Down", Pane::List) => app.list_step(10),
        ("C-u" | "Page_Up", Pane::List) => app.list_step(-10),
        ("C-d" | "Page_Down" | "space", _) => app.reader_scroll(1.0),
        ("C-u" | "Page_Up" | "S-space", _) => app.reader_scroll(-1.0),
        ("G" | "End", Pane::Reader) => app.reader_top(true),
        ("G" | "End", _) => app.list_step(i64::MAX / 2),
        ("Home", Pane::Reader) => app.reader_top(false),
        ("Home", _) => app.select_index(0),
        ("]", _) => app.side_step(1),
        ("[", _) => app.side_step(-1),
        ("0", _) => app.open_view(View::Unified),
        (d, _) if d.len() == 1 && d.chars().all(|c| c.is_ascii_digit()) => {
            let i = d.parse::<usize>().unwrap() - 1;
            let acct = app.accounts.borrow().get(i).map(|a| a.name.clone());
            if let Some(acct) = acct {
                let folder = app.folder_with_role(i, "inbox").unwrap_or_else(|| "INBOX".into());
                app.open_view(View::Folder { acct, folder });
            }
        }

        // messages
        ("c", _) => app.compose_new(),
        ("r", _) => app.compose_reply(false),
        ("R", _) => app.compose_reply(true),
        ("f", _) => app.compose_forward(),
        ("E", _) => app.compose_edit(),
        ("e" | "y", _) => app.move_to_role("archive", "archived"),
        ("d" | "#" | "Delete", _) => app.delete(),
        ("!", _) => app.move_to_role("junk", "marked as spam"),
        ("s", _) => app.toggle_star(),
        ("u", _) => app.toggle_read(),
        ("U", _) => app.mark_all_read(),
        ("x", _) => {
            app.toggle_mark();
            app.list_step(1);
        }
        ("*", _) => app.mark_all(),
        ("m" | "v", _) => app.open_palette_move(),
        ("o", _) => app.open_in_browser(false),
        ("O", _) => app.open_in_browser(true),
        ("H", _) => {
            app.html_as_text.set(!app.html_as_text.get());
            app.rerender();
        }
        ("L", _) => app.open_palette_links(),
        ("I", _) => app.load_remote(false),
        ("C-plus" | "C-=" | "C-equal", _) => app.zoom(0.1),
        ("C-minus" | "C--", _) => app.zoom(-0.1),
        ("C-0", _) => app.zoom(0.0),
        ("a", _) => app.open_palette_attachments(),
        ("Escape", _) => {
            if !app.marked.borrow().is_empty() {
                app.clear_marks();
                app.toast("", false);
            } else if !app.search.text().is_empty() {
                app.search.set_text("");
            }
        }

        // app
        ("/", _) => {
            app.set_mode(Mode::Search);
            app.search.grab_focus();
        }
        ("C-r" | "F5", _) => app.refresh_all(),
        ("C-S-r", _) => {
            if let View::Folder { acct, folder } = app.view.borrow().clone()
                && let Some(i) = app.acct_idx(&acct) {
                    app.send(i, Cmd::Sync(folder));
                }
        }
        ("C-k" | ":" | "C-p", _) => app.open_palette_main(),
        ("t" | "C-t", _) => app.open_palette_themes(),
        ("?" | "F1", _) => app.show_help(),
        ("A", _) => app.open_form(),
        ("q", Pane::Reader) => app.set_focus(Pane::List),
        ("q", _) => app.win.close(),
        _ => return false,
    }
    true
}

fn go(app: &Rc<App>, k: &str) {
    match k {
        "g" => {
            if app.focus.get() == Pane::Reader {
                app.reader_top(false);
            } else {
                app.select_index(0);
            }
        }
        "u" => app.open_view(View::Unified),
        "i" | "s" | "d" | "a" | "t" | "j" | "f" => {
            let role = match k {
                "i" => "inbox",
                "s" => "sent",
                "d" => "drafts",
                "a" => "archive",
                "t" => "trash",
                "j" => "junk",
                _ => "flagged",
            };
            let acct = match &*app.view.borrow() {
                View::Folder { acct, .. } => app.acct_idx(acct),
                View::Unified => app.selected_index().and_then(|i| app.rows.borrow().get(i).and_then(|r| app.acct_idx(&r.acct))).or(Some(0)),
            };
            let Some(i) = acct else { return };
            match app.folder_with_role(i, role) {
                Some(folder) => {
                    let acct = app.accounts.borrow()[i].name.clone();
                    app.open_view(View::Folder { acct, folder });
                }
                None => app.toast(&format!("no {role} folder"), true),
            }
        }
        _ => {}
    }
}
