//! Updates from GitHub Releases: `latest` asks for the newest one, `fetch` downloads it in the
//! background and checks it against the signed SHA256SUMS, `finish` installs it when brief quits.
//! Nothing here runs without a verified file, and only a newer version than the running one is
//! ever offered.
//!
//! Windows runs the Inno Setup installer silently; a Linux tarball install swaps its binary.
//! Anything else (the portable zip, /usr, a cargo build) only hears that a release exists.

use ring::{digest, signature};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const LATEST_API: &str = "https://api.github.com/repos/elyesghazel/brief/releases/latest";
pub const RELEASES: &str = "https://github.com/elyesghazel/brief/releases";
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");
const AGENT: &str = concat!("brief/", env!("CARGO_PKG_VERSION"), " (update check)");
/// Ed25519. CI signs SHA256SUMS with the private half (Actions secret BRIEF_UPDATE_KEY).
const PUBLIC_KEY: [u8; 32] = [
    0x84, 0xf2, 0xc3, 0x59, 0x4b, 0xa1, 0x95, 0x16, 0xdb, 0x0c, 0x19, 0xf3, 0xb3, 0x1d, 0xdb, 0x3b, 0x41, 0xad, 0xf2, 0x98, 0x18, 0xb6, 0x01, 0x30, 0x9d, 0x81, 0x03,
    0xd9, 0x3c, 0x49, 0x65, 0x71,
];
const MAX_DOWNLOAD: u64 = 1 << 29;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    pub tag: String,
    pub url: String,
    /// Name and download URL of every file.
    pub assets: Vec<(String, String)>,
}

impl Release {
    pub fn version(&self) -> &str {
        self.tag.trim().trim_start_matches(['v', 'V'])
    }

    fn asset(&self, name: &str) -> Option<&str> {
        self.assets.iter().find(|(n, _)| n == name).map(|(_, u)| u.as_str())
    }
}

/// How this copy of brief gets replaced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Install {
    Setup { all_users: bool },
    Tarball { exe: PathBuf },
    Manual,
}

/// A verified update, waiting to be installed.
#[derive(Debug)]
pub struct Ready {
    how: Install,
    /// The installer, or the new binary.
    file: PathBuf,
}

impl Ready {
    /// An install for all users asks for admin rights, which needs someone at the screen.
    fn silent(&self) -> bool {
        !matches!(self.how, Install::Setup { all_users: true })
    }
}

static READY: Mutex<Option<Ready>> = Mutex::new(None);
static AUTO: AtomicBool = AtomicBool::new(false);
static RELAUNCH: AtomicBool = AtomicBool::new(false);

/// "v0.2.0" is newer than "0.1.0".
pub fn newer(tag: &str, current: &str) -> bool {
    let parse = |s: &str| -> Vec<u64> { s.trim().trim_start_matches(['v', 'V']).split(['.', '-', '+']).take(3).map(|p| p.parse().unwrap_or(0)).collect() };
    parse(tag) > parse(current)
}

fn agent(timeout: u64) -> ureq::Agent {
    ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(timeout))).build().into()
}

/// The newest release; GitHub leaves out drafts and pre-releases.
pub fn latest() -> Result<Release, String> {
    let mut resp = agent(10).get(LATEST_API).header("User-Agent", AGENT).header("Accept", "application/vnd.github+json").call().map_err(|e| e.to_string())?;
    let body = resp.body_mut().with_config().limit(1 << 20).read_to_string().map_err(|e| e.to_string())?;
    parse_release(&body)
}

fn parse_release(body: &str) -> Result<Release, String> {
    let v: serde_json::Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    let text = |v: &serde_json::Value, k: &str| v.get(k).and_then(|t| t.as_str()).map(str::to_owned);
    let tag = text(&v, "tag_name").ok_or("no release found")?;
    let url = text(&v, "html_url").unwrap_or_else(|| RELEASES.to_owned());
    let assets = v.get("assets").and_then(|a| a.as_array()).map(Vec::as_slice).unwrap_or_default();
    let assets = assets.iter().filter_map(|a| Some((text(a, "name")?, text(a, "browser_download_url")?))).collect();
    Ok(Release { tag, url, assets })
}

/// Sees whether a folder takes new files.
fn writable(dir: &Path) -> bool {
    let probe = dir.join(".brief-update-probe");
    let ok = std::fs::write(&probe, b"").is_ok();
    let _ = std::fs::remove_file(&probe);
    ok
}

pub fn how() -> Install {
    let Ok(exe) = std::env::current_exe() else { return Install::Manual };
    let Some(dir) = exe.parent() else { return Install::Manual };
    if cfg!(windows) {
        if !dir.join("unins000.exe").exists() {
            return Install::Manual;
        }
        let mine = dirs::data_local_dir().is_some_and(|d| dir.starts_with(d));
        return Install::Setup { all_users: !mine };
    }
    let packaged = dir.starts_with("/usr") && !dir.starts_with("/usr/local");
    let built = dir.ancestors().any(|d| d.ends_with("target"));
    if cfg!(target_os = "linux") && !packaged && !built && writable(dir) { Install::Tarball { exe } } else { Install::Manual }
}

fn asset_name(how: &Install, version: &str) -> Option<String> {
    match how {
        Install::Setup { .. } => Some(format!("brief-{version}-setup-x86_64.exe")),
        Install::Tarball { .. } => Some(format!("brief-{version}-linux-x86_64.tar.gz")),
        Install::Manual => None,
    }
}

/// brief can install this release by itself; older ones have no signature.
pub fn can_install(rel: &Release) -> bool {
    let name = asset_name(&how(), rel.version());
    rel.asset("SHA256SUMS.sig").is_some() && rel.asset("SHA256SUMS").is_some() && name.is_some_and(|n| rel.asset(&n).is_some())
}

fn dir() -> PathBuf {
    crate::store::path().parent().map(Path::to_path_buf).unwrap_or_else(std::env::temp_dir).join("update")
}

/// Removes what an earlier update left behind; the previous binary stays.
pub fn clean() {
    let Ok(files) = std::fs::read_dir(dir()) else { return };
    for f in files.flatten() {
        let path = f.path();
        if path.is_dir() {
            let _ = std::fs::remove_dir_all(path);
        } else if f.file_name() != "brief.previous" {
            let _ = std::fs::remove_file(path);
        }
    }
}

fn download(url: &str, limit: u64) -> Result<ureq::http::Response<ureq::Body>, String> {
    let resp = agent(900).get(url).header("User-Agent", AGENT).call().map_err(|e| e.to_string())?;
    if resp.body().content_length().is_some_and(|n| n > limit) {
        return Err("the download is too big".into());
    }
    Ok(resp)
}

fn fetch_small(url: &str) -> Result<Vec<u8>, String> {
    let mut resp = download(url, 1 << 16)?;
    resp.body_mut().with_config().limit(1 << 16).read_to_vec().map_err(|e| e.to_string())
}

/// `sums` is what CI signed with the private key.
fn verify(sums: &[u8], sig: &[u8], key: &[u8]) -> Result<(), String> {
    signature::UnparsedPublicKey::new(&signature::ED25519, key).verify(sums, sig).map_err(|_| "the release's signature is wrong".to_owned())
}

/// The hash of `name` in a sha256sum listing.
fn sum_of<'a>(sums: &'a str, name: &str) -> Option<&'a str> {
    sums.lines().find_map(|l| l.split_once(char::is_whitespace).filter(|(_, n)| n.trim_start().trim_start_matches('*') == name).map(|(h, _)| h))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Copies `from` to `to` unless it fails the hash, which then leaves nothing behind.
fn save_checked(mut from: impl Read, to: &Path, want: &str) -> Result<(), String> {
    let part = to.with_extension("part");
    let mut out = std::fs::File::create(&part).map_err(|e| e.to_string())?;
    let mut hash = digest::Context::new(&digest::SHA256);
    let mut buf = vec![0; 1 << 16];
    let mut total = 0;
    let copied = loop {
        match from.read(&mut buf) {
            Ok(0) => break Ok(()),
            Ok(n) => {
                total += n as u64;
                if total > MAX_DOWNLOAD {
                    break Err("the download is too big".to_owned());
                }
                hash.update(&buf[..n]);
                if let Err(e) = out.write_all(&buf[..n]) {
                    break Err(e.to_string());
                }
            }
            Err(e) => break Err(e.to_string()),
        }
    };
    drop(out);
    let checked = copied.and_then(|()| if hex(hash.finish().as_ref()) == want.to_lowercase() { Ok(()) } else { Err("the download does not match its checksum".to_owned()) });
    let done = checked.and_then(|()| std::fs::rename(&part, to).map_err(|e| e.to_string()));
    if done.is_err() {
        let _ = std::fs::remove_file(&part);
    }
    done
}

/// Unpacks the tarball and makes sure the new binary starts here at all: right version, no
/// library missing on this system.
fn unpack(tarball: &Path, version: &str, into: &Path) -> Result<PathBuf, String> {
    std::fs::create_dir_all(into).map_err(|e| e.to_string())?;
    let out = Command::new("tar").arg("-xzf").arg(tarball).arg("-C").arg(into).output().map_err(|e| format!("tar: {e}"))?;
    if !out.status.success() {
        return Err(format!("tar: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    let new = into.join(format!("brief-{version}-linux-x86_64/bin/brief"));
    let out = Command::new(&new).arg("--version").output().map_err(|e| format!("the new version does not start: {e}"))?;
    let said = String::from_utf8_lossy(&out.stdout);
    if !out.status.success() || said.trim() != format!("brief {version}") {
        return Err(format!("the new version does not start: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(new)
}

/// Downloads a release and verifies it. Slow; run it off the main thread.
pub fn fetch(rel: &Release) -> Result<Ready, String> {
    let how = how();
    let version = rel.version();
    let name = asset_name(&how, version).ok_or("this install cannot update itself")?;
    let (Some(sums), Some(sig), Some(url)) = (rel.asset("SHA256SUMS"), rel.asset("SHA256SUMS.sig"), rel.asset(&name)) else {
        return Err("the release has no signed files".into());
    };
    let (sums, sig) = (fetch_small(sums)?, fetch_small(sig)?);
    verify(&sums, &sig, &PUBLIC_KEY)?;
    let sums = String::from_utf8_lossy(&sums);
    // the name carries the version, so an old signed listing cannot pass as this release
    let want = sum_of(&sums, &name).ok_or("the release's checksums lack its file")?;
    let dir = dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let file = dir.join(&name);
    let resp = download(url, MAX_DOWNLOAD)?;
    save_checked(resp.into_body().into_reader(), &file, want)?;
    let file = match how {
        Install::Tarball { .. } => unpack(&file, version, &dir.join(version))?,
        _ => file,
    };
    Ok(Ready { how, file })
}

/// Puts `new` in place of `exe` at once, keeping the old one as `backup`.
fn swap(new: &Path, exe: &Path, backup: &Path) -> Result<(), String> {
    let next = exe.with_extension("new");
    let result = std::fs::copy(new, &next).and_then(|_| std::fs::copy(exe, backup)).and_then(|_| std::fs::rename(&next, exe));
    if result.is_err() {
        let _ = std::fs::remove_file(&next);
    }
    result.map_err(|e| format!("cannot replace {}: {e}", exe.display()))
}

fn apply(ready: Ready, relaunch: bool) -> Result<(), String> {
    match ready.how {
        Install::Setup { all_users } => {
            let mut setup = Command::new(&ready.file);
            setup.args(["/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART", "/CLOSEAPPLICATIONS"]).arg(if all_users { "/ALLUSERS" } else { "/CURRENTUSER" });
            if relaunch {
                setup.arg("/RELAUNCH=1");
            }
            setup.spawn().map(drop).map_err(|e| format!("cannot start the installer: {e}"))
        }
        Install::Tarball { exe } => {
            swap(&ready.file, &exe, &dir().join("brief.previous"))?;
            if relaunch {
                // this process still holds the app's D-Bus name for a moment; a start right now
                // would only wake it up
                let start = Command::new("sh").arg("-c").arg("sleep 1; exec \"$0\"").arg(&exe).spawn();
                start.map_err(|e| format!("cannot restart brief: {e}"))?;
            }
            Ok(())
        }
        Install::Manual => Err("this install cannot update itself".into()),
    }
}

/// Keeps a verified update for when brief quits.
pub fn hold(ready: Ready) {
    *READY.lock().unwrap() = Some(ready);
}

/// Whether an update goes in whenever brief closes.
pub fn set_auto(on: bool) {
    AUTO.store(on, Ordering::Relaxed);
}

/// The next quit installs the held update and starts brief again.
pub fn restart_on_quit() {
    RELAUNCH.store(true, Ordering::Relaxed);
}

/// brief is quitting: installs the update it holds, if it may.
pub fn on_quit() {
    let relaunch = RELAUNCH.load(Ordering::Relaxed);
    let Some(ready) = READY.lock().unwrap().take() else { return };
    if !relaunch && !(AUTO.load(Ordering::Relaxed) && ready.silent()) {
        return;
    }
    if let Err(e) = apply(ready, relaunch) {
        crate::log::warn(&format!("update: {e}"));
    }
}

/// `brief update`: the same thing from a terminal. `force` takes the newest release even if it
/// is the running one, `restart` starts brief afterwards. The installer outlives this process,
/// which it has to replace.
pub fn run_cli(force: bool, restart: bool) -> Result<String, String> {
    let rel = latest()?;
    if !force && !newer(&rel.tag, CURRENT) {
        return Ok(format!("brief {CURRENT} is the newest release"));
    }
    if !can_install(&rel) {
        return Err(format!("this install cannot update itself, download {} from {}", rel.version(), rel.url));
    }
    println!("downloading brief {}", rel.version());
    apply(fetch(&rel)?, restart)?;
    Ok(format!("installing brief {}", rel.version()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::rand::SystemRandom;
    use ring::signature::{Ed25519KeyPair, KeyPair};

    #[test]
    fn newer_versions() {
        assert!(newer("v0.2.0", "0.1.0"));
        assert!(newer("0.1.10", "0.1.9"));
        assert!(newer("v1.0.0", "0.9.9"));
        assert!(!newer("v0.1.0", "0.1.0"));
        assert!(!newer("0.0.9", "0.1.0"));
        assert!(!newer("garbage", "0.1.0"));
    }

    #[test]
    fn release_from_github() {
        let body = r#"{"tag_name":"v0.2.0","html_url":"https://x/r","assets":[{"name":"SHA256SUMS","browser_download_url":"https://x/s"}]}"#;
        let r = parse_release(body).unwrap();
        assert_eq!(r.version(), "0.2.0");
        assert_eq!(r.asset("SHA256SUMS"), Some("https://x/s"));
        assert_eq!(r.asset("other"), None);
        assert!(parse_release("{}").is_err());
    }

    #[test]
    fn signature_decides() {
        let rng = SystemRandom::new();
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
        let pair = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
        let sig = pair.sign(b"sums");
        let key = pair.public_key().as_ref();
        assert!(verify(b"sums", sig.as_ref(), key).is_ok());
        assert!(verify(b"sumz", sig.as_ref(), key).is_err());
        assert!(verify(b"sums", &[0; 64], key).is_err());
        assert!(verify(b"sums", sig.as_ref(), &PUBLIC_KEY).is_err());
    }

    #[test]
    fn checksum_listing() {
        let sums = "aa  brief-1-setup-x86_64.exe\nbb *brief-1-linux-x86_64.tar.gz\n";
        assert_eq!(sum_of(sums, "brief-1-setup-x86_64.exe"), Some("aa"));
        assert_eq!(sum_of(sums, "brief-1-linux-x86_64.tar.gz"), Some("bb"));
        assert_eq!(sum_of(sums, "brief-2-setup-x86_64.exe"), None);
    }

    #[test]
    fn download_must_match() {
        let dir = std::env::temp_dir().join(format!("brief-update-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("f.bin");
        let good = hex(digest::digest(&digest::SHA256, b"data").as_ref());
        assert!(save_checked(&b"tampered"[..], &file, &good).is_err());
        assert!(!file.exists() && !dir.join("f.part").exists());
        save_checked(&b"data"[..], &file, &good).unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), b"data");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn swap_keeps_the_old_binary() {
        let dir = std::env::temp_dir().join(format!("brief-swap-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (new, exe, backup) = (dir.join("n"), dir.join("brief"), dir.join("b"));
        std::fs::write(&new, "2").unwrap();
        std::fs::write(&exe, "1").unwrap();
        swap(&new, &exe, &backup).unwrap();
        assert_eq!(std::fs::read(&exe).unwrap(), b"2");
        assert_eq!(std::fs::read(&backup).unwrap(), b"1");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn every_install_has_a_file_or_none() {
        assert_eq!(asset_name(&Install::Setup { all_users: false }, "1.0.0").unwrap(), "brief-1.0.0-setup-x86_64.exe");
        assert_eq!(asset_name(&Install::Tarball { exe: "x".into() }, "1.0.0").unwrap(), "brief-1.0.0-linux-x86_64.tar.gz");
        assert_eq!(asset_name(&Install::Manual, "1.0.0"), None);
    }
}
