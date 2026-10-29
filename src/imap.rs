//! One IMAP worker thread per account, plus one IDLE thread watching its inbox.
//!
//! The UI sends `Cmd`s and gets `Event`s back. Workers write everything into the cache first, the
//! events only say what changed, and the UI re-reads the cache.

use crate::config::{Account, Security};
use crate::message;
use crate::store::{Header, Store};
use imap::extensions::idle::{SetReadTimeout, WaitOutcome};
use imap::types::Flag;
use imap_proto::NameAttribute;
use std::collections::{HashSet, VecDeque};
use std::io::{self, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

#[derive(Debug)]
pub enum Cmd {
    /// Folder list, counts, inbox and special folders. `manual` = the user asked for it.
    SyncAll { manual: bool },
    /// Re-sync one folder.
    Sync(String),
    /// Fetch one body now (the user opened it).
    Body(String, u32),
    /// `+FLAGS`/`-FLAGS`, e.g. `\Seen`.
    Flag { folder: String, uids: Vec<u32>, flag: &'static str, on: bool },
    /// `\Seen` on every message in the folder, cached or not.
    SeenAll(String),
    Move { folder: String, uids: Vec<u32>, to: String },
    /// Into the trash, or expunged when already in the trash.
    Delete { folder: String, uids: Vec<u32> },
    /// Save a sent message into the Sent folder.
    Append { role: &'static str, raw: Vec<u8> },
    /// Server-side search over the whole folder.
    Search { folder: String, query: String },
    /// The account was removed: log out, drop its cache, end the worker and the IDLE watcher.
    Stop,
    /// Like `Stop`, the cache stays: the worker is restarted with new settings.
    Quit,
}

#[derive(Debug, Clone)]
pub enum Event {
    /// Account idx; `busy` while a sync runs, `text` is the latest step.
    Status { acct: usize, busy: bool, text: String },
    Error { acct: usize, text: String },
    Folders,
    Folder { acct: usize, folder: String },
    Body { acct: usize, folder: String, uid: u32 },
    NewMail { acct: usize, from: String, subject: String, count: usize },
    SearchDone { found: usize },
    /// A sender icon for this domain was downloaded.
    Icon(String),
}

pub type EventTx = async_channel::Sender<Event>;

fn tls_config() -> Arc<rustls::ClientConfig> {
    static CFG: OnceLock<Arc<rustls::ClientConfig>> = OnceLock::new();
    CFG.get_or_init(|| {
        let roots = rustls::RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() };
        Arc::new(rustls::ClientConfig::builder().with_root_certificates(roots).with_no_client_auth())
    })
    .clone()
}

pub enum Stream {
    Plain(TcpStream),
    Tls(Box<rustls::StreamOwned<rustls::ClientConnection, TcpStream>>),
}

impl Stream {
    fn tcp(&self) -> &TcpStream {
        match self {
            Stream::Plain(s) => s,
            Stream::Tls(s) => &s.sock,
        }
    }

    fn wrap(tcp: TcpStream, host: &str) -> io::Result<Stream> {
        let name = rustls::pki_types::ServerName::try_from(host.to_owned()).map_err(io::Error::other)?;
        let conn = rustls::ClientConnection::new(tls_config(), name).map_err(io::Error::other)?;
        Ok(Stream::Tls(Box::new(rustls::StreamOwned::new(conn, tcp))))
    }
}

impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Stream::Plain(s) => s.read(buf),
            Stream::Tls(s) => s.read(buf),
        }
    }
}

impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Stream::Plain(s) => s.write(buf),
            Stream::Tls(s) => s.write(buf),
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        match self {
            Stream::Plain(s) => s.flush(),
            Stream::Tls(s) => s.flush(),
        }
    }
}

impl SetReadTimeout for Stream {
    fn set_read_timeout(&mut self, timeout: Option<Duration>) -> imap::Result<()> {
        self.tcp().set_read_timeout(timeout).map_err(imap::Error::Io)
    }
}

pub type Session = imap::Session<Stream>;

fn tcp_connect(host: &str, port: u16) -> Result<TcpStream, String> {
    let addrs: Vec<_> = (host, port).to_socket_addrs().map_err(|e| format!("{host}: {e}"))?.collect();
    let mut last = format!("{host}: no address");
    for addr in addrs {
        match TcpStream::connect_timeout(&addr, Duration::from_secs(12)) {
            Ok(s) => {
                let _ = s.set_read_timeout(Some(Duration::from_secs(90)));
                let _ = s.set_write_timeout(Some(Duration::from_secs(30)));
                return Ok(s);
            }
            Err(e) => last = format!("{host}:{port}: {e}"),
        }
    }
    Err(last)
}

/// Reads the plain-text greeting, asks for STARTTLS and returns the socket ready for the handshake.
fn starttls(mut tcp: TcpStream) -> io::Result<TcpStream> {
    let mut reader = io::BufReader::new(tcp.try_clone()?);
    let mut line = String::new();
    io::BufRead::read_line(&mut reader, &mut line)?;
    tcp.write_all(b"b0 STARTTLS\r\n")?;
    loop {
        line.clear();
        if io::BufRead::read_line(&mut reader, &mut line)? == 0 {
            return Err(io::Error::other("connection closed"));
        }
        if let Some(rest) = line.strip_prefix("b0 ") {
            return if rest.starts_with("OK") { Ok(tcp) } else { Err(io::Error::other(rest.trim().to_owned())) };
        }
    }
}

/// SASL XOAUTH2 for `AUTHENTICATE`. A non-empty challenge is the server's error report, which
/// wants an empty answer before the final NO.
struct XOAuth2(String);

impl imap::Authenticator for XOAuth2 {
    type Response = String;
    fn process(&self, challenge: &[u8]) -> String {
        if challenge.is_empty() { self.0.clone() } else { String::new() }
    }
}

/// Connects and logs in with a password, or an access token for OAuth accounts.
pub fn connect(acc: &Account, secret: &str) -> Result<Session, String> {
    open(acc, secret).map_err(|(_, e)| e)
}

/// Like `connect`, with the secret looked up; an OAuth token the server refuses is refreshed once.
pub fn login(acc: &Account) -> Result<Session, String> {
    match open(acc, &acc.secret()?) {
        Ok(s) => Ok(s),
        Err((true, _)) if acc.is_oauth() => {
            crate::oauth::invalidate(acc);
            connect(acc, &acc.secret()?)
        }
        Err((_, e)) => Err(e),
    }
}

/// The error says whether the server refused the login.
fn open(acc: &Account, secret: &str) -> Result<Session, (bool, String)> {
    let s = &acc.imap;
    let tcp = tcp_connect(&s.host, s.port).map_err(|e| (false, e))?;
    let err = |e: imap::Error| (false, format!("{}: {e}", s.host));
    let mut client = match s.security {
        Security::Tls => {
            let mut c = imap::Client::new(Stream::wrap(tcp, &s.host).map_err(|e| (false, e.to_string()))?);
            c.read_greeting().map_err(err)?;
            c
        }
        Security::Starttls => {
            let tcp = starttls(tcp).map_err(|e| (false, format!("{}: STARTTLS: {e}", s.host)))?;
            let mut c = imap::Client::new(Stream::wrap(tcp, &s.host).map_err(|e| (false, e.to_string()))?);
            c.greeting_read = true;
            c
        }
        Security::None => {
            let mut c = imap::Client::new(Stream::Plain(tcp));
            c.read_greeting().map_err(err)?;
            c
        }
    };
    client.greeting_read = true;
    let result = if acc.is_oauth() {
        client.authenticate("XOAUTH2", &XOAuth2(crate::oauth::xoauth2(acc.login(), secret)))
    } else {
        client.login(acc.login(), secret)
    };
    result.map_err(|(e, _)| match e {
        imap::Error::No(r) => (true, format!("login refused: {}", r.information)),
        imap::Error::Bad(r) => (true, format!("login refused: {}", r.information)),
        e => (false, format!("login: {e}")),
    })
}

fn role_of(name: &str, attrs: &[NameAttribute<'_>]) -> &'static str {
    for a in attrs {
        match a {
            NameAttribute::Sent => return "sent",
            NameAttribute::Trash => return "trash",
            NameAttribute::Drafts => return "drafts",
            NameAttribute::Junk => return "junk",
            NameAttribute::Archive => return "archive",
            NameAttribute::All => return "all",
            NameAttribute::Flagged => return "flagged",
            _ => {}
        }
    }
    let leaf = name.rsplit(['/', '.']).next().unwrap_or(name).to_lowercase();
    if name.eq_ignore_ascii_case("INBOX") {
        return "inbox";
    }
    match leaf.as_str() {
        "sent" | "sent items" | "sent messages" | "sent mail" | "gesendet" | "gesendete elemente" | "gesendete objekte" => "sent",
        "trash" | "deleted" | "deleted items" | "deleted messages" | "papierkorb" | "gelöschte elemente" | "bin" => "trash",
        "drafts" | "draft" | "entwürfe" => "drafts",
        "junk" | "spam" | "junk e-mail" | "junk email" => "junk",
        "archive" | "archiv" | "archives" => "archive",
        _ => "",
    }
}

/// Decodes IMAP modified UTF-7 folder names (`&AOQ-` -> `ä`) for display.
pub fn decode_utf7(name: &str) -> String {
    let mut out = String::new();
    let mut rest = name;
    while let Some(i) = rest.find('&') {
        out += &rest[..i];
        rest = &rest[i + 1..];
        let Some(end) = rest.find('-') else {
            out.push('&');
            break;
        };
        let chunk = &rest[..end];
        rest = &rest[end + 1..];
        if chunk.is_empty() {
            out.push('&');
            continue;
        }
        const B64: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+,";
        let mut bits = 0u32;
        let mut n = 0;
        let mut units = Vec::new();
        for c in chunk.chars() {
            let Some(v) = B64.find(c) else { continue };
            bits = (bits << 6) | v as u32;
            n += 6;
            if n >= 16 {
                n -= 16;
                units.push(((bits >> n) & 0xffff) as u16);
            }
        }
        out += &String::from_utf16_lossy(&units);
    }
    out + rest
}

fn uid_set(uids: &[u32]) -> String {
    let mut sorted = uids.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    let mut out = Vec::new();
    let mut i = 0;
    while i < sorted.len() {
        let start = sorted[i];
        let mut end = start;
        while i + 1 < sorted.len() && sorted[i + 1] == end + 1 {
            i += 1;
            end = sorted[i];
        }
        out.push(if start == end { start.to_string() } else { format!("{start}:{end}") });
        i += 1;
    }
    out.join(",")
}

fn flags_of(flags: &[Flag<'_>]) -> (bool, bool, bool) {
    (flags.contains(&Flag::Seen), flags.contains(&Flag::Flagged), flags.contains(&Flag::Answered))
}

const HEADER_QUERY: &str = "(UID FLAGS RFC822.SIZE INTERNALDATE BODY.PEEK[HEADER.FIELDS (FROM TO CC SUBJECT DATE MESSAGE-ID REFERENCES IN-REPLY-TO CONTENT-TYPE)])";

/// The UI's end of a worker.
pub struct Handle {
    tx: Sender<Cmd>,
    idx: Arc<AtomicUsize>,
    thread: std::cell::Cell<Option<std::thread::JoinHandle<()>>>,
}

impl Handle {
    pub fn send(&self, cmd: Cmd) {
        let _ = self.tx.send(cmd);
    }

    /// The account moved up in the list (one before it was removed).
    pub fn set_idx(&self, idx: usize) {
        self.idx.store(idx, Ordering::Relaxed);
    }

    /// Ends the worker and keeps its cache; join the thread to know it wrote its last row. A
    /// running sync stops at its next step.
    pub fn quit(&self) -> Option<std::thread::JoinHandle<()>> {
        self.send(Cmd::Quit);
        self.thread.take()
    }
}

pub struct Worker {
    /// Position of the account, which events carry.
    idx: Arc<AtomicUsize>,
    acc: Account,
    session: Option<Session>,
    store: Store,
    events: EventTx,
    rx: Receiver<Cmd>,
    queue: VecDeque<Cmd>,
    cache_limit: u32,
    /// Folders synced at least once this run; new mail before that is not "new".
    seen_once: HashSet<String>,
    /// The folder the user looks at, kept fresh along with the special folders.
    active: Option<String>,
    failures: u32,
    /// Shared with the IDLE watcher.
    stop: Arc<AtomicBool>,
    /// Stopping drops the cache (the account was removed).
    forget: bool,
}

/// Starts the worker and the IDLE watcher for one account.
pub fn spawn(idx: usize, acc: Account, events: EventTx, cache_limit: u32, sync_minutes: u64) -> Handle {
    let (tx, rx) = mpsc::channel();
    let idx = Arc::new(AtomicUsize::new(idx));
    let worker_idx = idx.clone();
    let idle_tx = tx.clone();
    let idle_acc = acc.clone();
    let stop = Arc::new(AtomicBool::new(false));
    let idle_stop = stop.clone();
    let thread = std::thread::Builder::new()
        .name(format!("imap-{}", acc.name))
        .spawn(move || {
            let store = Store::open().expect("cache");
            let mut w = Worker {
                idx: worker_idx,
                acc,
                session: None,
                store,
                events,
                rx,
                queue: VecDeque::new(),
                cache_limit,
                seen_once: HashSet::new(),
                active: None,
                failures: 0,
                stop,
                forget: false,
            };
            w.run(Duration::from_secs(sync_minutes.max(1) * 60));
        })
        .unwrap();
    std::thread::Builder::new().name("idle".into()).spawn(move || idle_loop(idle_acc, idle_tx, idle_stop)).unwrap();
    tx.send(Cmd::SyncAll { manual: false }).ok();
    Handle { tx, idx, thread: std::cell::Cell::new(Some(thread)) }
}

impl Worker {
    fn idx(&self) -> usize {
        self.idx.load(Ordering::Relaxed)
    }

    fn status(&self, busy: bool, text: impl Into<String>) {
        let _ = self.events.send_blocking(Event::Status { acct: self.idx(), busy, text: text.into() });
    }

    fn emit(&self, e: Event) {
        let _ = self.events.send_blocking(e);
    }

    fn run(&mut self, interval: Duration) {
        let mut next_sync = Instant::now() + interval;
        loop {
            let cmd = match self.queue.pop_front() {
                Some(c) => c,
                None => match self.rx.recv_timeout(next_sync.saturating_duration_since(Instant::now())) {
                    Ok(c) => c,
                    Err(RecvTimeoutError::Timeout) => Cmd::SyncAll { manual: false },
                    Err(RecvTimeoutError::Disconnected) => return,
                },
            };
            if matches!(cmd, Cmd::Stop | Cmd::Quit) {
                self.forget = matches!(cmd, Cmd::Stop);
                return self.shutdown();
            }
            if matches!(cmd, Cmd::SyncAll { .. }) {
                next_sync = Instant::now() + interval;
                // collapse queued duplicate syncs
                self.queue.retain(|c| !matches!(c, Cmd::SyncAll { .. }));
            }
            let manual = matches!(cmd, Cmd::SyncAll { manual: true } | Cmd::Sync(_));
            let label = format!("{cmd:?}").split([' ', '(', '{']).next().unwrap_or_default().to_owned();
            let result = self.handle(cmd);
            if self.stop.load(Ordering::Relaxed) {
                return self.shutdown();
            }
            if let Err(e) = result {
                self.session = None;
                self.failures += 1;
                self.emit(Event::Error { acct: self.idx(), text: e.clone() });
                self.status(false, format!("offline: {e}"));
                crate::log::error(&format!("[{}] {label}: {e}", self.acc.name));
                // back off, but a manual refresh retries right away
                if !manual {
                    next_sync = Instant::now() + Duration::from_secs((15 * self.failures as u64).min(300));
                }
            } else {
                self.failures = 0;
            }
        }
    }

    fn shutdown(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(mut s) = self.session.take() {
            let _ = s.logout();
        }
        // a sync that was running may have written after the UI cleared the cache
        if self.forget {
            self.store.delete_account(&self.acc.name);
        }
    }

    fn session(&mut self) -> Result<&mut Session, String> {
        if self.session.is_none() {
            self.status(true, "connecting");
            self.session = Some(login(&self.acc)?);
        }
        Ok(self.session.as_mut().unwrap())
    }

    fn handle(&mut self, cmd: Cmd) -> Result<(), String> {
        let e = |e: imap::Error| e.to_string();
        match cmd {
            Cmd::SyncAll { manual } => {
                self.status(true, if manual { "refreshing" } else { "syncing" });
                self.sync_folders()?;
                let folders = self.store.folders(&self.acc.name);
                let mut todo: Vec<String> = folders
                    .iter()
                    .filter(|f| matches!(f.role.as_str(), "inbox" | "sent" | "drafts" | "archive" | "trash" | "junk"))
                    .map(|f| f.name.clone())
                    .collect();
                if let Some(a) = &self.active
                    && !todo.contains(a) {
                        todo.insert(1.min(todo.len()), a.clone());
                    }
                for name in &todo {
                    self.status(true, format!("syncing {}", decode_utf7(name)));
                    self.sync_folder(name)?;
                    self.pump()?;
                }
                for f in folders.iter().filter(|f| !todo.contains(&f.name)) {
                    if let Ok(mb) = self.session()?.status(&f.name, "(MESSAGES UNSEEN)") {
                        self.store.set_counts(&self.acc.name, &f.name, mb.unseen.unwrap_or(0), mb.exists);
                    }
                }
                self.emit(Event::Folders);
                for name in &todo {
                    self.prefetch(name, if name.eq_ignore_ascii_case("INBOX") { 150 } else { 40 })?;
                }
                let when = chrono::Local::now().format("%H:%M");
                self.status(false, format!("synced {when}"));
            }
            Cmd::Sync(folder) => {
                self.active = Some(folder.clone());
                self.status(true, format!("syncing {}", decode_utf7(&folder)));
                self.sync_folder(&folder)?;
                self.emit(Event::Folders);
                self.prefetch(&folder, 60)?;
                self.status(false, format!("synced {}", chrono::Local::now().format("%H:%M")));
            }
            Cmd::Body(folder, uid) => self.fetch_bodies(&folder, &[uid])?,
            Cmd::Flag { folder, uids, flag, on } => {
                let s = self.session()?;
                s.select(&folder).map_err(e)?;
                let op = if on { "+FLAGS.SILENT" } else { "-FLAGS.SILENT" };
                s.uid_store(uid_set(&uids), format!("{op} ({flag})")).map_err(e)?;
            }
            Cmd::SeenAll(folder) => {
                let s = self.session()?;
                let mb = s.select(&folder).map_err(e)?;
                if mb.exists > 0 {
                    s.uid_store("1:*", "+FLAGS.SILENT (\\Seen)").map_err(e)?;
                }
                self.store.set_counts(&self.acc.name, &folder, 0, mb.exists);
            }
            Cmd::Move { folder, uids, to } => {
                let s = self.session()?;
                s.select(&folder).map_err(e)?;
                let set = uid_set(&uids);
                if s.uid_mv(&set, &to).is_err() {
                    s.uid_copy(&set, &to).map_err(e)?;
                    s.uid_store(&set, "+FLAGS.SILENT (\\Deleted)").map_err(e)?;
                    if s.uid_expunge(&set).is_err() {
                        s.expunge().map_err(e)?;
                    }
                }
                self.sync_folder(&to)?;
                self.emit(Event::Folders);
            }
            Cmd::Delete { folder, uids } => {
                let trash = self.store.folders(&self.acc.name).into_iter().find(|f| f.role == "trash").map(|f| f.name);
                match trash {
                    Some(t) if t != folder => return self.handle(Cmd::Move { folder, uids, to: t }),
                    _ => {
                        let s = self.session()?;
                        s.select(&folder).map_err(e)?;
                        let set = uid_set(&uids);
                        s.uid_store(&set, "+FLAGS.SILENT (\\Deleted)").map_err(e)?;
                        if s.uid_expunge(&set).is_err() {
                            s.expunge().map_err(e)?;
                        }
                    }
                }
            }
            Cmd::Append { role, raw } => {
                let folder = self.store.folders(&self.acc.name).into_iter().find(|f| f.role == role).map(|f| f.name);
                if let Some(folder) = folder {
                    let s = self.session()?;
                    let mut cmd = s.append(&folder, &raw);
                    cmd.flag(Flag::Seen);
                    if role == "drafts" {
                        cmd.flag(Flag::Draft);
                    }
                    cmd.finish().map_err(e)?;
                    self.sync_folder(&folder)?;
                }
            }
            Cmd::Stop | Cmd::Quit => {}
            Cmd::Search { folder, query } => {
                self.status(true, format!("searching {}", decode_utf7(&folder)));
                let s = self.session()?;
                s.select(&folder).map_err(e)?;
                let mut criteria = String::new();
                for term in query.split_whitespace().filter(|t| !t.contains(':')) {
                    let q = term.replace(['\\', '"'], "");
                    criteria += &format!("OR OR SUBJECT \"{q}\" FROM \"{q}\" BODY \"{q}\" ");
                }
                let found: Vec<u32> = if criteria.is_empty() {
                    Vec::new()
                } else {
                    s.uid_search(criteria.trim()).map_err(e)?.into_iter().collect()
                };
                let known = self.store.flags(&self.acc.name, &folder);
                let missing: Vec<u32> = found.iter().copied().filter(|u| !known.contains_key(u)).collect();
                for chunk in missing.chunks(200) {
                    self.fetch_headers(&folder, chunk)?;
                }
                self.prefetch(&folder, 60)?;
                self.emit(Event::SearchDone { found: found.len() });
                self.status(false, format!("{} found on server", found.len()));
            }
        }
        Ok(())
    }

    /// Moves waiting commands into the queue and runs urgent ones (body fetches, flag changes)
    /// right away, so opening a message never waits for a long sync.
    fn pump(&mut self) -> Result<(), String> {
        while let Ok(c) = self.rx.try_recv() {
            if matches!(c, Cmd::Stop | Cmd::Quit) {
                // cuts a long sync short; run() sees the flag
                self.forget = matches!(c, Cmd::Stop);
                self.stop.store(true, Ordering::Relaxed);
                return Err("stopped".into());
            }
            self.queue.push_back(c);
        }
        let urgent: Vec<Cmd> = {
            let (u, rest): (Vec<_>, Vec<_>) =
                self.queue.drain(..).partition(|c| matches!(c, Cmd::Body(..) | Cmd::Flag { .. } | Cmd::SeenAll(_)));
            self.queue.extend(rest);
            u
        };
        for c in urgent {
            self.handle(c)?;
        }
        Ok(())
    }

    fn sync_folders(&mut self) -> Result<(), String> {
        let names = self.session()?.list(Some(""), Some("*")).map_err(|e| e.to_string())?;
        let list: Vec<(String, String, String)> = names
            .iter()
            .filter(|n| !n.attributes().contains(&NameAttribute::NoSelect))
            .map(|n| (n.name().to_owned(), n.delimiter().unwrap_or("/").to_owned(), role_of(n.name(), n.attributes()).to_owned()))
            .collect();
        // a provider may flag two folders the same way (Gmail: All Mail + Archive); keep one of each
        let mut seen_roles = HashSet::new();
        let list: Vec<_> = list
            .into_iter()
            .map(|(n, d, r)| if r.is_empty() || seen_roles.insert(r.clone()) { (n, d, r) } else { (n, d, String::new()) })
            .collect();
        self.store.set_folders(&self.acc.name, &list);
        self.emit(Event::Folders);
        Ok(())
    }

    fn fetch_headers(&mut self, folder: &str, uids: &[u32]) -> Result<Vec<Header>, String> {
        let fetches = self.session()?.uid_fetch(uid_set(uids), HEADER_QUERY).map_err(|e| e.to_string())?;
        let headers: Vec<Header> = fetches
            .iter()
            .filter_map(|f| {
                let uid = f.uid?;
                let s = message::summary(f.header().unwrap_or_default());
                let (seen, flagged, answered) = flags_of(f.flags());
                Some(Header {
                    uid,
                    msgid: s.msgid,
                    subject: s.subject,
                    from_name: s.from_name,
                    from_addr: s.from_addr,
                    to: s.to,
                    cc: s.cc,
                    // no Date header: when the server got it
                    date: if s.date > 0 { s.date } else { f.internal_date().map_or(0, |d| d.timestamp()) },
                    seen,
                    flagged,
                    answered,
                    attach: s.multipart_mixed,
                    size: f.size.unwrap_or(0),
                    refs: s.refs,
                })
            })
            .collect();
        self.store.put_headers(&self.acc.name, folder, &headers);
        Ok(headers)
    }

    fn sync_folder(&mut self, folder: &str) -> Result<(), String> {
        let acct = self.acc.name.clone();
        let limit = self.cache_limit;
        let mb = self.session()?.select(folder).map_err(|e| e.to_string())?;
        let validity = mb.uid_validity.unwrap_or(0);
        if self.store.uidvalidity(&acct, folder) != validity {
            self.store.clear_folder(&acct, folder);
            self.store.set_uidvalidity(&acct, folder, validity);
        }
        let local = self.store.flags(&acct, folder);
        if mb.exists == 0 {
            self.store.clear_folder(&acct, folder);
            self.store.set_counts(&acct, folder, 0, 0);
            self.seen_once.insert(folder.to_owned());
            self.emit(Event::Folder { acct: self.idx(), folder: folder.into() });
            return Ok(());
        }
        let start = mb.exists.saturating_sub(limit) + 1;
        let fetches = self.session()?.fetch(format!("{start}:*"), "(UID FLAGS)").map_err(|e| e.to_string())?;
        let mut server = std::collections::HashMap::new();
        for f in fetches.iter() {
            if let Some(uid) = f.uid {
                server.insert(uid, flags_of(f.flags()));
            }
        }
        let min = server.keys().min().copied().unwrap_or(0);
        let gone: Vec<u32> = local.keys().copied().filter(|u| *u >= min && !server.contains_key(u)).collect();
        self.store.delete(&acct, folder, &gone);
        let changed: Vec<(u32, bool, bool, bool)> = server
            .iter()
            .filter(|(u, f)| local.get(u).is_some_and(|l| l != *f))
            .map(|(u, f)| (*u, f.0, f.1, f.2))
            .collect();
        self.store.set_flags_bulk(&acct, folder, &changed);
        let mut new: Vec<u32> = server.keys().copied().filter(|u| !local.contains_key(u)).collect();
        new.sort_unstable_by(|a, b| b.cmp(a));
        let first_time = !self.seen_once.contains(folder) && local.is_empty();
        let mut fresh = Vec::new();
        for chunk in new.chunks(250) {
            fresh.extend(self.fetch_headers(folder, chunk)?);
            // show the newest batch at once on a first sync
            self.emit(Event::Folder { acct: self.idx(), folder: folder.into() });
        }
        let unseen = server.values().filter(|f| !f.0).count() as u32;
        self.store.set_counts(&acct, folder, unseen, mb.exists);
        self.store.trim(&acct, folder, limit);
        let was_seen = self.seen_once.contains(folder);
        self.seen_once.insert(folder.to_owned());
        if !first_time && was_seen && folder.eq_ignore_ascii_case("INBOX") {
            let unread: Vec<&Header> = fresh.iter().filter(|h| !h.seen).collect();
            if let Some(h) = unread.first() {
                let from = if h.from_name.is_empty() { h.from_addr.clone() } else { h.from_name.clone() };
                self.emit(Event::NewMail { acct: self.idx(), from, subject: h.subject.clone(), count: unread.len() });
            }
        }
        self.emit(Event::Folder { acct: self.idx(), folder: folder.into() });
        Ok(())
    }

    fn fetch_bodies(&mut self, folder: &str, uids: &[u32]) -> Result<(), String> {
        let acct = self.acc.name.clone();
        let s = self.session()?;
        s.select(folder).map_err(|e| e.to_string())?;
        let fetches = s.uid_fetch(uid_set(uids), "(UID BODY.PEEK[])").map_err(|e| e.to_string())?;
        for f in fetches.iter() {
            let (Some(uid), Some(raw)) = (f.uid, f.body()) else { continue };
            let (preview, attach) = message::digest(raw);
            self.store.set_body(&acct, folder, uid, raw, &preview, attach);
            if uids.len() == 1 {
                self.emit(Event::Body { acct: self.idx(), folder: folder.into(), uid });
            }
        }
        Ok(())
    }

    /// Downloads the newest bodies in the background, so they open instantly and work offline.
    fn prefetch(&mut self, folder: &str, n: u32) -> Result<(), String> {
        let missing = self.store.missing_bodies(&self.acc.name, folder, n);
        for chunk in missing.chunks(15) {
            self.pump()?;
            self.fetch_bodies(folder, chunk)?;
            self.emit(Event::Folder { acct: self.idx(), folder: folder.into() });
        }
        Ok(())
    }
}

/// Watches the inbox over its own connection and asks the worker to sync on any change. Ends
/// with the worker, at the latest one IDLE round after `stop`.
fn idle_loop(acc: Account, tx: Sender<Cmd>, stop: Arc<AtomicBool>) {
    std::thread::sleep(Duration::from_secs(5));
    while !stop.load(Ordering::Relaxed) {
        let result = (|| -> Result<(), String> {
            let mut s = login(&acc)?;
            let caps = s.capabilities().map_err(|e| e.to_string())?;
            if !caps.has_str("IDLE") {
                return Err("no IDLE".into());
            }
            s.select("INBOX").map_err(|e| e.to_string())?;
            loop {
                let outcome = s
                    .idle()
                    .timeout(Duration::from_secs(9 * 60))
                    .keepalive(false)
                    .wait_while(imap::extensions::idle::stop_on_any)
                    .map_err(|e| e.to_string())?;
                if stop.load(Ordering::Relaxed) {
                    let _ = s.logout();
                    return Ok(());
                }
                if let WaitOutcome::MailboxChanged = outcome
                    && tx.send(Cmd::Sync("INBOX".into())).is_err() {
                        return Ok(());
                    }
                s.noop().map_err(|e| e.to_string())?;
            }
        })();
        match result {
            Err(e) if e == "no IDLE" => return,
            Ok(()) => return,
            Err(_) => std::thread::sleep(Duration::from_secs(60)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uid_sets_compress() {
        assert_eq!(uid_set(&[5, 1, 2, 3, 9, 10]), "1:3,5,9:10");
        assert_eq!(uid_set(&[7]), "7");
    }

    #[test]
    fn utf7_names() {
        assert_eq!(decode_utf7("Entw&APw-rfe"), "Entwürfe");
        assert_eq!(decode_utf7("A &- B"), "A & B");
        assert_eq!(decode_utf7("INBOX"), "INBOX");
    }

    #[test]
    fn roles_from_names() {
        assert_eq!(role_of("INBOX", &[]), "inbox");
        assert_eq!(role_of("INBOX.Sent", &[]), "sent");
        assert_eq!(role_of("[Gmail]/Spam", &[]), "junk");
        assert_eq!(role_of("Foo", &[NameAttribute::Trash]), "trash");
    }
}
