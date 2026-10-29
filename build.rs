//! Windows only: puts the app icon and the version info into brief.exe, so Explorer, the taskbar
//! and the installer show them. Elsewhere it does nothing.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=assets/logo/brief.ico");
    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        // FileVersion and ProductVersion come from Cargo.toml
        res.set_icon("assets/logo/brief.ico")
            .set("ProductName", "brief")
            .set("FileDescription", "brief")
            .set("CompanyName", "Elyes Ghazel")
            .set("LegalCopyright", "Copyright (c) 2026 Elyes Ghazel, MIT License")
            .set("OriginalFilename", "brief.exe")
            .set("InternalName", "brief");
        res.compile().expect("cannot embed the windows resources");
    }
}
