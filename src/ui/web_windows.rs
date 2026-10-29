//! Windows: HTML mail in Microsoft Edge WebView2. The view is a native child window of brief's
//! toplevel, kept over a placeholder in the reader's paper. GTK cannot draw above it, so it hides
//! whenever the placeholder is unmapped or an overlay (palette, help, form, confirm) is up.
//!
//! WebView2Loader.dll ships next to brief.exe and is loaded on the first HTML mail, not linked:
//! brief starts as fast as before, and without the DLL or the Edge WebView2 Runtime HTML mail
//! falls back to text.

use super::super::open_uri;
use gtk::prelude::*;
use gtk::{gdk, glib};
use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant};
use webview2_com::Microsoft::Web::WebView2::Win32::*;
use webview2_com::{
    AcceleratorKeyPressedEventHandler, ContextMenuRequestedEventHandler, CreateCoreWebView2ControllerCompletedHandler,
    CreateCoreWebView2EnvironmentCompletedHandler, ExecuteScriptCompletedHandler, FocusChangedEventHandler, LaunchingExternalUriSchemeEventHandler,
    NavigationStartingEventHandler,
    NewWindowRequestedEventHandler, WebResourceRequestedEventHandler, take_pwstr,
};
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
use windows::Win32::System::LibraryLoader::{GetModuleFileNameW, GetProcAddress, LoadLibraryW};
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::Shell::SHCreateMemStream;
use windows::Win32::UI::WindowsAndMessaging::{GWL_STYLE, GetWindowLongPtrW, SetWindowLongPtrW, WS_CLIPCHILDREN};
use windows::core::{BOOL, HRESULT, HSTRING, Interface, PCWSTR, PWSTR, s, w};

/// The mail is served from here; every other http(s) request is blocked unless remote content
/// is allowed.
const HOST: &str = "https://mail.brief.invalid/";
const MISSING: &str = "Install the Microsoft Edge WebView2 Runtime to see HTML mail · o opens it in the browser";

type LinkFn = Rc<dyn Fn(&str)>;
type Hook = Rc<dyn Fn()>;
type CreateEnv = unsafe extern "system" fn(PCWSTR, PCWSTR, *mut c_void, *mut c_void) -> HRESULT;

unsafe extern "C" {
    fn gdk_win32_surface_get_handle(surface: *mut gdk::ffi::GdkSurface) -> *mut c_void;
}

enum State {
    Idle,
    Starting,
    Ready(View),
    Failed,
}

#[derive(Clone)]
struct View {
    env: ICoreWebView2Environment,
    ctl: ICoreWebView2Controller,
    web: ICoreWebView2,
    parent: HWND,
}

struct Inner {
    state: RefCell<State>,
    slot: gtk::Box,
    /// The document on screen, its URL and whether it may load remote content.
    html: RefCell<String>,
    url: RefCell<String>,
    remote: Cell<bool>,
    loads: Cell<u32>,
    zoom: Cell<f64>,
    /// Bounds in device pixels while shown.
    shown: Cell<Option<(i32, i32, i32, i32)>>,
    focused: Cell<bool>,
    /// A context menu is open in the view.
    menu: Cell<bool>,
    on_link: RefCell<LinkFn>,
    on_key: RefCell<LinkFn>,
    on_focus: RefCell<Hook>,
    on_failed: RefCell<Hook>,
}

pub struct Html(Rc<Inner>);

impl Html {
    pub fn new() -> Html {
        let slot = gtk::Box::new(gtk::Orientation::Vertical, 0);
        slot.set_vexpand(true);
        slot.set_hexpand(true);
        Html(Rc::new(Inner {
            state: RefCell::new(State::Idle),
            slot,
            html: RefCell::new(String::new()),
            url: RefCell::new(String::new()),
            remote: Cell::new(false),
            loads: Cell::new(0),
            zoom: Cell::new(1.0),
            shown: Cell::new(None),
            focused: Cell::new(false),
            menu: Cell::new(false),
            on_link: RefCell::new(Rc::new(|uri: &str| open_uri(uri))),
            on_key: RefCell::new(Rc::new(|_: &str| {})),
            on_focus: RefCell::new(Rc::new(|| {})),
            on_failed: RefCell::new(Rc::new(|| {})),
        }))
    }

    /// What a clicked link does; the browser until set.
    pub fn on_link(&self, f: impl Fn(&str) + 'static) {
        *self.0.on_link.borrow_mut() = Rc::new(f);
    }

    /// Keys pressed while the view has the keyboard, by brief's key name ("Escape", "C-k").
    pub fn on_key(&self, f: impl Fn(&str) + 'static) {
        *self.0.on_key.borrow_mut() = Rc::new(f);
    }

    /// A click put the keyboard into the view.
    pub fn on_focus(&self, f: impl Fn() + 'static) {
        *self.0.on_focus.borrow_mut() = Rc::new(f);
    }

    /// WebView2 could not start; HTML mail has to show as text from now on.
    pub fn on_failed(&self, f: impl Fn() + 'static) {
        *self.0.on_failed.borrow_mut() = Rc::new(f);
    }

    pub fn available(&self) -> bool {
        !matches!(*self.0.state.borrow(), State::Failed)
    }

    /// Why HTML mail shows as text, when that is not the user's choice.
    pub fn note(&self) -> Option<&'static str> {
        (!self.available()).then_some(MISSING)
    }

    /// Shows `html` in `paper`, starting WebView2 on first use.
    pub fn load(&self, paper: &gtk::Box, html: &str, zoom: f64, remote: bool) {
        let me = &self.0;
        *me.html.borrow_mut() = html.to_owned();
        me.remote.set(remote);
        me.loads.set(me.loads.get() + 1);
        me.zoom.set(zoom);
        if me.slot.parent().is_none() {
            paper.append(&me.slot);
            let weak = Rc::downgrade(me);
            me.slot.connect_map(move |_| sync_weak(&weak));
            let weak = Rc::downgrade(me);
            me.slot.connect_unmap(move |_| sync_weak(&weak));
        }
        let state = std::mem::replace(&mut *me.state.borrow_mut(), State::Starting);
        match state {
            State::Idle => {
                let weak = Rc::downgrade(me);
                glib::idle_add_local_once(move || {
                    if let Some(me) = weak.upgrade() {
                        start(&me);
                    }
                });
            }
            State::Ready(v) => {
                *me.state.borrow_mut() = State::Ready(v.clone());
                navigate(me, &v);
            }
            other => *me.state.borrow_mut() = other,
        }
    }

    pub fn set_zoom(&self, zoom: f64) {
        self.0.zoom.set(zoom);
        if let Some(v) = view(&self.0) {
            unsafe {
                let _ = v.ctl.SetZoomFactor(zoom);
            }
        }
    }

    /// Runs a script in the page; works with page scripts off.
    pub fn run_js(&self, js: &str) {
        let Some(v) = view(&self.0) else { return };
        let done = ExecuteScriptCompletedHandler::create(Box::new(|r, _| {
            if let Err(e) = r {
                eprintln!("brief: webview2 script failed: {e}");
            }
            Ok(())
        }));
        unsafe {
            let _ = v.web.ExecuteScript(&HSTRING::from(js), &done);
        }
    }
}

fn view(me: &Inner) -> Option<View> {
    match &*me.state.borrow() {
        State::Ready(v) => Some(v.clone()),
        _ => None,
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// `CreateCoreWebView2EnvironmentWithOptions` from the WebView2Loader.dll next to brief.exe.
fn loader() -> Result<CreateEnv, String> {
    unsafe {
        let mut buf = [0u16; 1024];
        let n = GetModuleFileNameW(None, &mut buf) as usize;
        let exe = std::path::PathBuf::from(String::from_utf16_lossy(&buf[..n]));
        let dll = exe.with_file_name("WebView2Loader.dll");
        let path = wide(&dll.to_string_lossy());
        let lib = LoadLibraryW(PCWSTR(path.as_ptr())).map_err(|e| format!("cannot load {}: {e}", dll.display()))?;
        let f = GetProcAddress(lib, s!("CreateCoreWebView2EnvironmentWithOptions")).ok_or("WebView2Loader.dll lacks CreateCoreWebView2EnvironmentWithOptions")?;
        Ok(std::mem::transmute::<unsafe extern "system" fn() -> isize, CreateEnv>(f))
    }
}

fn fail(me: &Rc<Inner>, why: &str) {
    eprintln!("brief: webview2 unavailable, HTML mail shows as text: {why}");
    *me.state.borrow_mut() = State::Failed;
    me.slot.set_visible(false);
    let hook = me.on_failed.borrow().clone();
    glib::idle_add_local_once(move || hook());
}

/// Creates the environment, then the controller; both finish later on the GTK thread.
fn start(me: &Rc<Inner>) {
    let started = Instant::now();
    let Some(surface) = me.slot.native().and_then(|n| n.surface()) else { return fail(me, "no native window") };
    let parent = HWND(unsafe { gdk_win32_surface_get_handle(surface.as_ptr()) });
    let create = match loader() {
        Ok(f) => f,
        Err(e) => return fail(me, &e),
    };
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        // OpenGL wants the children clipped out of the toplevel's drawing
        let style = GetWindowLongPtrW(parent, GWL_STYLE);
        SetWindowLongPtrW(parent, GWL_STYLE, style | WS_CLIPCHILDREN.0 as isize);
    }
    let data = dirs::data_local_dir().unwrap_or_else(std::env::temp_dir).join("brief").join("webview2");
    let weak = Rc::downgrade(me);
    let on_env = CreateCoreWebView2EnvironmentCompletedHandler::create(Box::new(move |r, env| {
        let Some(me) = weak.upgrade() else { return Ok(()) };
        let env = match (r, env) {
            (Ok(()), Some(env)) => env,
            (r, _) => {
                fail(&me, &format!("no environment: {:?}", r.err()));
                return Ok(());
            }
        };
        let weak = Rc::downgrade(&me);
        let env2 = env.clone();
        let on_ctl = CreateCoreWebView2ControllerCompletedHandler::create(Box::new(move |r, ctl| {
            let Some(me) = weak.upgrade() else { return Ok(()) };
            match (r, ctl) {
                (Ok(()), Some(ctl)) => match setup(&me, env2, ctl, parent) {
                    Ok(v) => {
                        let mut version = PWSTR::null();
                        unsafe {
                            let _ = v.env.BrowserVersionString(&mut version);
                        }
                        eprintln!("brief: webview2 {} ready after {} ms", take_pwstr(version), started.elapsed().as_millis());
                        *me.state.borrow_mut() = State::Ready(v.clone());
                        navigate(&me, &v);
                        sync(&me);
                    }
                    Err(e) => fail(&me, &format!("setup: {e}")),
                },
                (r, _) => fail(&me, &format!("no controller: {:?}", r.err())),
            }
            Ok(())
        }));
        if let Err(e) = unsafe { env.CreateCoreWebView2Controller(parent, &on_ctl) } {
            fail(&me, &format!("no controller: {e}"));
        }
        Ok(())
    }));
    let data = HSTRING::from(data.as_os_str());
    let hr = unsafe { create(PCWSTR::null(), PCWSTR(data.as_ptr()), std::ptr::null_mut(), on_env.as_raw()) };
    if let Err(e) = hr.ok() {
        return fail(me, &format!("no runtime: {e}"));
    }
    // follow the placeholder: every painted frame (pane drags, resizes, overlays) and map/unmap
    if let Some(clock) = me.slot.root().and_then(|r| r.frame_clock()) {
        let weak = Rc::downgrade(me);
        clock.connect_after_paint(move |_| sync_weak(&weak));
    }
    // a click back in GTK takes the keyboard from the view; Ctrl+C copies from it
    if let Some(win) = me.slot.root() {
        let click = gtk::GestureClick::new();
        click.set_button(0);
        click.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(me);
        click.connect_pressed(move |_, _, _, _| {
            if let Some(me) = weak.upgrade() {
                refocus(&me);
            }
        });
        win.add_controller(click);
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(me);
        keys.connect_key_pressed(move |c, key, _, mods| {
            let Some(me) = weak.upgrade() else { return glib::Propagation::Proceed };
            let text = c.widget().and_then(|w| w.root()).and_then(|r| r.focus()).is_some_and(|f| f.is::<gtk::Text>() || f.downcast_ref::<gtk::TextView>().is_some_and(|t| t.is_editable()));
            let ctrl_c = mods & (gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::SHIFT_MASK | gdk::ModifierType::ALT_MASK) == gdk::ModifierType::CONTROL_MASK && key.to_lower() == gdk::Key::c;
            if ctrl_c && !text && me.shown.get().is_some() {
                copy(&me);
            }
            glib::Propagation::Proceed
        });
        win.add_controller(keys);
    }
}

/// Locks the view down and hooks its events.
fn setup(me: &Rc<Inner>, env: ICoreWebView2Environment, ctl: ICoreWebView2Controller, parent: HWND) -> windows::core::Result<View> {
    unsafe {
        let web = ctl.CoreWebView2()?;
        let s = web.Settings()?;
        s.SetIsScriptEnabled(false)?;
        s.SetAreDefaultScriptDialogsEnabled(false)?;
        s.SetIsStatusBarEnabled(false)?;
        s.SetAreDevToolsEnabled(false)?;
        s.SetIsZoomControlEnabled(false)?;
        s.SetIsWebMessageEnabled(false)?;
        s.SetIsBuiltInErrorPageEnabled(false)?;
        // the rest is newer than some runtimes; each is best effort
        if let Ok(s) = s.cast::<ICoreWebView2Settings3>() {
            let _ = s.SetAreBrowserAcceleratorKeysEnabled(false);
        }
        if let Ok(s) = s.cast::<ICoreWebView2Settings4>() {
            let _ = s.SetIsPasswordAutosaveEnabled(false);
            let _ = s.SetIsGeneralAutofillEnabled(false);
        }
        if let Ok(s) = s.cast::<ICoreWebView2Settings5>() {
            let _ = s.SetIsPinchZoomEnabled(false);
        }
        if let Ok(s) = s.cast::<ICoreWebView2Settings6>() {
            let _ = s.SetIsSwipeNavigationEnabled(false);
        }
        if let Ok(s) = s.cast::<ICoreWebView2Settings8>() {
            // SmartScreen would send the mail's URLs to Microsoft
            let _ = s.SetIsReputationCheckingRequired(false);
        }
        if let Ok(c) = ctl.cast::<ICoreWebView2Controller2>() {
            let _ = c.SetDefaultBackgroundColor(COREWEBVIEW2_COLOR { A: 255, R: 255, G: 255, B: 255 });
        }
        ctl.SetIsVisible(false)?;
        ctl.SetZoomFactor(me.zoom.get())?;
        let mut token = 0i64;

        // the mail itself comes from memory; other http(s) only once remote content is allowed
        web.AddWebResourceRequestedFilter(w!("*"), COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL)?;
        let weak = Rc::downgrade(me);
        let env2 = env.clone();
        web.add_WebResourceRequested(
            &WebResourceRequestedEventHandler::create(Box::new(move |_, args| {
                let (Some(me), Some(args)) = (weak.upgrade(), args) else { return Ok(()) };
                let mut uri = PWSTR::null();
                args.Request()?.Uri(&mut uri)?;
                let uri = take_pwstr(uri);
                let (status, body, headers) = if uri.starts_with(HOST) {
                    (200, page(&me.html.borrow()).into_bytes(), "Content-Type: text/html; charset=utf-8\r\nCache-Control: no-store")
                } else if (uri.starts_with("http:") || uri.starts_with("https:")) && !me.remote.get() {
                    (403, Vec::new(), "")
                } else {
                    return Ok(());
                };
                let stream = SHCreateMemStream(Some(&body));
                let reason = if status == 200 { w!("OK") } else { w!("Blocked") };
                let resp = env2.CreateWebResourceResponse(stream.as_ref(), status, reason, &HSTRING::from(headers))?;
                args.SetResponse(&resp)
            })),
            &mut token,
        )?;

        // the view never leaves the mail: clicked links go to `on_link`
        let weak = Rc::downgrade(me);
        web.add_NavigationStarting(
            &NavigationStartingEventHandler::create(Box::new(move |_, args| {
                let (Some(me), Some(args)) = (weak.upgrade(), args) else { return Ok(()) };
                let mut uri = PWSTR::null();
                args.Uri(&mut uri)?;
                let uri = take_pwstr(uri);
                if uri.split('#').next() == Some(me.url.borrow().as_str()) || uri == "about:blank" {
                    return Ok(());
                }
                args.SetCancel(true)?;
                let mut user = BOOL(0);
                args.IsUserInitiated(&mut user)?;
                clicked(&me, uri, user);
                Ok(())
            })),
            &mut token,
        )?;
        // mailto: and other schemes Edge would hand to Windows
        if let Ok(web18) = web.cast::<ICoreWebView2_18>() {
            let weak = Rc::downgrade(me);
            web18.add_LaunchingExternalUriScheme(
                &LaunchingExternalUriSchemeEventHandler::create(Box::new(move |_, args| {
                    let (Some(me), Some(args)) = (weak.upgrade(), args) else { return Ok(()) };
                    args.SetCancel(true)?;
                    let mut uri = PWSTR::null();
                    args.Uri(&mut uri)?;
                    let mut user = BOOL(0);
                    args.IsUserInitiated(&mut user)?;
                    clicked(&me, take_pwstr(uri), user);
                    Ok(())
                })),
                &mut token,
            )?;
        }
        let weak = Rc::downgrade(me);
        web.add_NewWindowRequested(
            &NewWindowRequestedEventHandler::create(Box::new(move |_, args| {
                let (Some(me), Some(args)) = (weak.upgrade(), args) else { return Ok(()) };
                args.SetHandled(true)?;
                let mut uri = PWSTR::null();
                args.Uri(&mut uri)?;
                let mut user = BOOL(0);
                args.IsUserInitiated(&mut user)?;
                clicked(&me, take_pwstr(uri), user);
                Ok(())
            })),
            &mut token,
        )?;

        // right-click: copy and copy link, nothing else
        if let Ok(web11) = web.cast::<ICoreWebView2_11>() {
            let weak = Rc::downgrade(me);
            web11.add_ContextMenuRequested(
                &ContextMenuRequestedEventHandler::create(Box::new(move |_, args| {
                    let (Some(me), Some(args)) = (weak.upgrade(), args) else { return Ok(()) };
                    me.menu.set(true);
                    let items = args.MenuItems()?;
                    let mut n = 0u32;
                    items.Count(&mut n)?;
                    for i in (0..n).rev() {
                        let mut name = PWSTR::null();
                        items.GetValueAtIndex(i)?.Name(&mut name)?;
                        if !matches!(take_pwstr(name).as_str(), "copy" | "copyLinkLocation") {
                            items.RemoveValueAtIndex(i)?;
                        }
                    }
                    items.Count(&mut n)?;
                    if n == 0 {
                        args.SetHandled(true)?;
                    }
                    Ok(())
                })),
                &mut token,
            )?;
        }

        // brief's keys keep working while the view has the keyboard
        let weak = Rc::downgrade(me);
        ctl.add_AcceleratorKeyPressed(
            &AcceleratorKeyPressedEventHandler::create(Box::new(move |_, args| {
                let (Some(me), Some(args)) = (weak.upgrade(), args) else { return Ok(()) };
                let mut kind = COREWEBVIEW2_KEY_EVENT_KIND::default();
                args.KeyEventKind(&mut kind)?;
                if kind != COREWEBVIEW2_KEY_EVENT_KIND_KEY_DOWN && kind != COREWEBVIEW2_KEY_EVENT_KIND_SYSTEM_KEY_DOWN {
                    return Ok(());
                }
                let mut vk = 0u32;
                args.VirtualKey(&mut vk)?;
                let Some(name) = key_name(vk) else { return Ok(()) };
                match name.as_str() {
                    // the view's own copy and select all, then the keyboard goes back
                    "C-c" | "C-Insert" => {
                        let weak = Rc::downgrade(&me);
                        glib::timeout_add_local_once(Duration::from_millis(100), move || {
                            if let Some(me) = weak.upgrade() {
                                refocus(&me);
                            }
                        });
                    }
                    "C-a" | "M-F4" => {}
                    _ => {
                        args.SetHandled(true)?;
                        refocus(&me);
                        let f = me.on_key.borrow().clone();
                        glib::idle_add_local_once(move || f(&name));
                    }
                }
                Ok(())
            })),
            &mut token,
        )?;
        let weak = Rc::downgrade(me);
        ctl.add_GotFocus(
            &FocusChangedEventHandler::create(Box::new(move |_, _| {
                let Some(me) = weak.upgrade() else { return Ok(()) };
                me.focused.set(true);
                (me.on_focus.borrow().clone())();
                after_click(&me);
                Ok(())
            })),
            &mut token,
        )?;
        let weak = Rc::downgrade(me);
        ctl.add_LostFocus(
            &FocusChangedEventHandler::create(Box::new(move |_, _| {
                if let Some(me) = weak.upgrade() {
                    me.focused.set(false);
                }
                Ok(())
            })),
            &mut token,
        )?;
        Ok(View { env, ctl, web, parent })
    }
}

/// A link the user clicked goes to `on_link`; navigations the mail starts on its own go nowhere.
fn clicked(me: &Inner, uri: String, user: BOOL) {
    if user.as_bool() && !uri.starts_with(HOST) {
        let f = me.on_link.borrow().clone();
        glib::idle_add_local_once(move || f(&uri));
    }
}

/// The mail with thin scrollbars, closer to the overlay ones WebKitGTK draws.
fn page(html: &str) -> String {
    const STYLE: &str = "<style>html{scrollbar-width:thin;scrollbar-color:#c4c4c4 transparent}</style>";
    let mut page = html.to_owned();
    // after the policy that prepare_html puts in <head>
    if let Some(at) = page.find("<meta name=\"referrer\"") {
        page.insert_str(at, STYLE);
    }
    page
}

/// Gives the keyboard back to brief shortly after the mouse buttons are up, so a click or a
/// selection in the mail does not swallow j / k. Ctrl+C copies the selection from there; only an
/// open context menu keeps the keyboard in the view.
fn after_click(me: &Rc<Inner>) {
    me.menu.set(false);
    let weak = Rc::downgrade(me);
    let mut idle = 0;
    glib::timeout_add_local(Duration::from_millis(40), move || {
        let Some(me) = weak.upgrade() else { return glib::ControlFlow::Break };
        let held = [VK_LBUTTON, VK_RBUTTON, VK_MBUTTON].iter().any(|b| unsafe { GetAsyncKeyState(b.0 as i32) } < 0);
        idle = if held { 0 } else { idle + 1 };
        if idle < 3 {
            return glib::ControlFlow::Continue;
        }
        if !me.menu.get() {
            refocus(&me);
        }
        glib::ControlFlow::Break
    });
}

/// Ctrl+C in brief while the mail shows: its selection goes to the clipboard.
fn copy(me: &Inner) {
    let Some(v) = view(me) else { return };
    let slot = me.slot.clone();
    let done = ExecuteScriptCompletedHandler::create(Box::new(move |_, json| {
        if let Ok(text) = serde_json::from_str::<String>(&json)
            && !text.is_empty()
        {
            slot.clipboard().set_text(&text);
        }
        Ok(())
    }));
    unsafe {
        let _ = v.web.ExecuteScript(w!("String(window.getSelection())"), &done);
    }
}

/// Hands the keyboard back to the GTK window.
fn refocus(me: &Inner) {
    if !me.focused.get() {
        return;
    }
    if let Some(v) = view(me) {
        me.focused.set(false);
        unsafe {
            let _ = SetFocus(Some(v.parent));
        }
    }
}

fn navigate(me: &Inner, v: &View) {
    let url = format!("{HOST}{}", me.loads.get());
    *me.url.borrow_mut() = url.clone();
    unsafe {
        let _ = v.ctl.SetZoomFactor(me.zoom.get());
        let _ = v.web.Navigate(&HSTRING::from(url));
    }
}

fn sync_weak(weak: &Weak<Inner>) {
    if let Some(me) = weak.upgrade() {
        sync(&me);
    }
}

/// Moves the view over the placeholder, or hides it; touches WebView2 only on a change.
fn sync(me: &Inner) {
    let Some(v) = view(me) else { return };
    let place = place(&me.slot);
    if place == me.shown.get() {
        return;
    }
    me.shown.set(place);
    unsafe {
        match place {
            Some((x, y, w, h)) => {
                let _ = v.ctl.SetBounds(RECT { left: x, top: y, right: x + w, bottom: y + h });
                let _ = v.ctl.SetIsVisible(true);
                let _ = v.ctl.NotifyParentWindowPositionChanged();
            }
            None => {
                let _ = v.ctl.SetIsVisible(false);
            }
        }
    }
}

/// The placeholder's rectangle in device pixels of the toplevel, None while it is not on screen
/// or something in an overlay covers it.
fn place(slot: &gtk::Box) -> Option<(i32, i32, i32, i32)> {
    if !slot.is_mapped() {
        return None;
    }
    let native = slot.native()?;
    let surface = native.surface()?;
    let b = slot.compute_bounds(&native)?;
    let mut anc = slot.parent();
    while let Some(w) = anc {
        if let Some(ov) = w.downcast_ref::<gtk::Overlay>() {
            let mut c = ov.first_child();
            while let Some(child) = c {
                if Some(&child) != ov.child().as_ref()
                    && child.is_mapped()
                    && child.compute_bounds(&native).is_some_and(|cb| cb.intersection(&b).is_some())
                {
                    return None;
                }
                c = child.next_sibling();
            }
        }
        anc = w.parent();
    }
    let (tx, ty) = native.surface_transform();
    let scale = surface.scale();
    let x = ((b.x() as f64 + tx) * scale).round() as i32;
    let y = ((b.y() as f64 + ty) * scale).round() as i32;
    let w = (b.width() as f64 * scale).round() as i32;
    let h = (b.height() as f64 * scale).round() as i32;
    (w > 0 && h > 0).then_some((x, y, w, h))
}

/// brief's name for a key WebView2 reports as an accelerator: "Escape", "C-k", "C-S-r", "M-F4".
fn key_name(vk: u32) -> Option<String> {
    let down = |k: VIRTUAL_KEY| unsafe { GetKeyState(k.0 as i32) } < 0;
    let (ctrl, alt, shift) = (down(VK_CONTROL), down(VK_MENU), down(VK_SHIFT));
    let vk = VIRTUAL_KEY(vk as u16);
    let ch = match vk.0 {
        0x41..=0x5A => Some((vk.0 as u8 + 32) as char),
        0x30..=0x39 => Some(vk.0 as u8 as char),
        _ => match vk {
            VK_OEM_PLUS => Some('='),
            VK_OEM_MINUS => Some('-'),
            VK_OEM_PERIOD => Some('.'),
            VK_OEM_1 => Some(';'),
            _ => None,
        },
    };
    let named = match vk {
        VK_ESCAPE => "Escape",
        VK_RETURN => "Return",
        VK_TAB => "Tab",
        VK_BACK => "BackSpace",
        VK_DELETE => "Delete",
        VK_INSERT => "Insert",
        VK_HOME => "Home",
        VK_END => "End",
        VK_PRIOR => "Page_Up",
        VK_NEXT => "Page_Down",
        VK_UP => "Up",
        VK_DOWN => "Down",
        VK_LEFT => "Left",
        VK_RIGHT => "Right",
        VK_ADD => "plus",
        VK_SUBTRACT => "minus",
        VK_SPACE => "space",
        _ if (VK_F1.0..=VK_F12.0).contains(&vk.0) => return Some(fkey(vk.0 - VK_F1.0 + 1, ctrl, alt, shift)),
        _ => "",
    };
    let mut out = String::new();
    if ctrl {
        out += "C-";
    }
    if alt {
        out += "M-";
    }
    match ch {
        // a character key only counts with Ctrl or Alt, as in keys::name
        Some(c) if ctrl || alt => {
            if shift {
                out += "S-";
            }
            out.push(c);
        }
        Some(_) => return None,
        None if named.is_empty() => return None,
        None => {
            if shift {
                out += "S-";
            }
            out += named;
        }
    }
    Some(out)
}

fn fkey(n: u16, ctrl: bool, alt: bool, shift: bool) -> String {
    format!("{}{}{}F{n}", if ctrl { "C-" } else { "" }, if alt { "M-" } else { "" }, if shift { "S-" } else { "" })
}
