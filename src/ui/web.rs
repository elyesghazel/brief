//! The HTML view: WebKitGTK where it exists. Without it (Windows, or built without the `webkit`
//! feature) `AVAILABLE` is false, HTML mail shows as text and `o` opens it in the browser.

pub use imp::*;

#[cfg(all(feature = "webkit", unix))]
mod imp {
    use super::super::open_uri;
    use gtk::prelude::*;
    use gtk::{gdk, gio};
    use std::cell::RefCell;
    use webkit6::prelude::*;

    pub const AVAILABLE: bool = true;

    #[derive(Default)]
    pub struct Html {
        view: RefCell<Option<webkit6::WebView>>,
    }

    impl Html {
        pub fn new() -> Html {
            Html::default()
        }

        /// Shows `html` in `paper`, making the view on first use.
        pub fn load(&self, paper: &gtk::Box, html: &str, zoom: f64) {
            let web = self.view(paper);
            web.set_zoom_level(zoom);
            web.load_html(html, Some("about:blank"));
        }

        pub fn set_zoom(&self, zoom: f64) {
            if let Some(w) = self.view.borrow().as_ref() {
                w.set_zoom_level(zoom);
            }
        }

        pub fn run_js(&self, js: &str) {
            if let Some(w) = self.view.borrow().as_ref() {
                w.evaluate_javascript(js, None, None, None::<&gio::Cancellable>, |_| {});
            }
        }

        /// The WebKit view: ephemeral session, no page scripts, links open outside.
        fn view(&self, paper: &gtk::Box) -> webkit6::WebView {
            if let Some(w) = self.view.borrow().as_ref() {
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
            paper.append(&holder);
            *self.view.borrow_mut() = Some(web.clone());
            web
        }
    }
}

#[cfg(not(all(feature = "webkit", unix)))]
mod imp {
    pub const AVAILABLE: bool = false;

    pub struct Html;

    impl Html {
        pub fn new() -> Html {
            Html
        }

        pub fn load(&self, _paper: &gtk::Box, _html: &str, _zoom: f64) {}

        pub fn set_zoom(&self, _zoom: f64) {}

        pub fn run_js(&self, _js: &str) {}
    }
}
