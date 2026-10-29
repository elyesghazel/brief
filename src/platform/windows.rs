//! Windows: ShellExecute, the Credential Manager, `cmd /C`, toasts via PowerShell.

use std::os::windows::process::CommandExt as _;
use std::path::Path;
use std::process::{Command, Stdio};
use windows_sys::Win32::Security::Credentials::{CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC, CREDENTIALW, CredFree, CredReadW, CredWriteW};
use windows_sys::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole, GetStdHandle, STD_OUTPUT_HANDLE};
use windows_sys::Win32::UI::Shell::ShellExecuteW;
use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

/// Native title bar: windows float here.
pub const DECORATIONS: bool = true;
pub const FONT: &str = "Inter, Segoe UI, sans-serif";

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// brief.exe is a GUI program, so started from a terminal it has no stdout. Borrow the parent's
/// console so `brief sync` and friends print there. Redirected output already works.
pub fn attach_console() {
    unsafe {
        if GetStdHandle(STD_OUTPUT_HANDLE).is_null() {
            AttachConsole(ATTACH_PARENT_PROCESS);
        }
    }
}

pub fn prepare_gui() {}

/// No single instance: GApplication would need a D-Bus session bus, which Windows lacks.
pub fn app_flags() -> gtk::gio::ApplicationFlags {
    gtk::gio::ApplicationFlags::NON_UNIQUE
}

/// Runs a command line through cmd.exe without flashing a console window.
pub fn shell(cmd: &str) -> Command {
    let mut c = Command::new("cmd");
    c.raw_arg(format!("/S /C \"{cmd}\"")).creation_flags(CREATE_NO_WINDOW);
    c
}

/// The editor for Ctrl+E on `path`: the configured command, else %EDITOR% in a console window,
/// else Notepad.
pub fn editor(configured: &str, path: &Path) -> Command {
    let env = std::env::var("EDITOR").unwrap_or_default();
    let cmd = if configured.is_empty() { env.as_str() } else { configured };
    if cmd.is_empty() {
        let mut c = Command::new("notepad");
        c.arg(path);
        return c;
    }
    // a console editor gets its own window because brief has no console to share
    let mut c = Command::new("cmd");
    c.raw_arg(format!("/S /C \"{cmd} \"{}\"\"", path.display()));
    c
}

/// A toast through Windows PowerShell, so brief needs no WinRT bindings. Title and body travel
/// in the environment, never through the script text.
pub fn notify(summary: &str, body: &str) {
    const SCRIPT: &str = "$m=[Windows.UI.Notifications.ToastNotificationManager,Windows.UI.Notifications,ContentType=WindowsRuntime];\
        $x=$m::GetTemplateContent([Windows.UI.Notifications.ToastTemplateType]::ToastText02);\
        $t=$x.GetElementsByTagName('text');\
        [void]$t.Item(0).AppendChild($x.CreateTextNode($env:BRIEF_TITLE));\
        [void]$t.Item(1).AppendChild($x.CreateTextNode($env:BRIEF_BODY));\
        $m::CreateToastNotifier('{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}\\WindowsPowerShell\\v1.0\\powershell.exe').Show([Windows.UI.Notifications.ToastNotification]::new($x))";
    let _ = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", SCRIPT])
        .env("BRIEF_TITLE", summary)
        .env("BRIEF_BODY", body)
        .creation_flags(CREATE_NO_WINDOW)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

/// Opens a URL or file with its default program.
pub fn open(target: &str) {
    let (verb, file) = (wide("open"), wide(target));
    unsafe {
        ShellExecuteW(std::ptr::null_mut(), verb.as_ptr(), file.as_ptr(), std::ptr::null(), std::ptr::null(), SW_SHOWNORMAL);
    }
}

fn target(service: &str, email: &str) -> Vec<u16> {
    wide(&format!("{service}:{email}"))
}

/// `service` is "brief" for passwords, "brief-oauth" for sign-in tokens.
pub fn keyring_get(service: &str, email: &str) -> Option<String> {
    let target = target(service, email);
    let mut cred: *mut CREDENTIALW = std::ptr::null_mut();
    unsafe {
        if CredReadW(target.as_ptr(), CRED_TYPE_GENERIC, 0, &mut cred) == 0 {
            return None;
        }
        let c = &*cred;
        let blob = std::slice::from_raw_parts(c.CredentialBlob, c.CredentialBlobSize as usize);
        let pass = String::from_utf8(blob.to_vec()).ok();
        CredFree(cred as *const _);
        pass
    }
}

/// Stores a secret in the Credential Manager as `<service>:<email>`.
pub fn keyring_store(service: &str, email: &str, pass: &str) -> bool {
    let mut target = target(service, email);
    let mut user = wide(email);
    let mut blob = pass.as_bytes().to_vec();
    let cred = CREDENTIALW {
        Type: CRED_TYPE_GENERIC,
        TargetName: target.as_mut_ptr(),
        CredentialBlobSize: blob.len() as u32,
        CredentialBlob: blob.as_mut_ptr(),
        Persist: CRED_PERSIST_LOCAL_MACHINE,
        UserName: user.as_mut_ptr(),
        ..unsafe { std::mem::zeroed() }
    };
    unsafe { CredWriteW(&cred, 0) != 0 }
}

/// Empty: brief reads the Credential Manager itself, nothing to run.
pub fn keyring_cmd(_email: &str) -> String {
    String::new()
}

/// The profile folder is already private to the user.
pub fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    std::fs::write(path, bytes)
}
