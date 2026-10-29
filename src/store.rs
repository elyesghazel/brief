//! Offline cache in `~/.cache/brief/mail.db` (SQLite, WAL).
//!
//! Every thread opens its own connection. The UI only ever reads from here, so the window shows
//! the last state instantly on start and the IMAP workers fill in the difference.

use rusqlite::{Connection, OptionalExtension, params};
use std::path::PathBuf;

pub struct Store {
    db: Connection,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Folder {
    pub acct: String,
    pub name: String,
    pub delim: String,
    pub role: String,
    pub unseen: u32,
    pub total: u32,
}

#[derive(Clone, Debug)]
pub struct Header {
    pub uid: u32,
    pub msgid: String,
    pub subject: String,
    pub from_name: String,
    pub from_addr: String,
    pub to: String,
    pub cc: String,
    pub date: i64,
    pub seen: bool,
    pub flagged: bool,
    pub answered: bool,
    pub attach: bool,
    pub size: u32,
    pub refs: String,
}

/// One line of the message list.
#[derive(Clone, Debug)]
pub struct Row {
    pub acct: String,
    pub folder: String,
    pub uid: u32,
    pub subject: String,
    pub from_name: String,
    pub from_addr: String,
    pub to: String,
    pub date: i64,
    pub seen: bool,
    pub flagged: bool,
    pub answered: bool,
    pub attach: bool,
    pub preview: String,
    pub msgid: String,
    pub refs: String,
    /// Messages in the conversation this row stands for; 0 outside thread mode.
    pub count: u32,
    /// "Lea, Tom, me" for a conversation row; empty for a single message.
    pub people: String,
}

/// What the message list shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum View {
    /// Every account's inbox.
    Unified,
    Folder { acct: String, folder: String },
}

pub fn path() -> PathBuf {
    dirs::cache_dir().unwrap_or_else(std::env::temp_dir).join("brief/mail.db")
}

impl Store {
    pub fn open() -> rusqlite::Result<Store> {
        Self::open_at(&path())
    }

    fn open_at(path: &std::path::Path) -> rusqlite::Result<Store> {
        let _ = std::fs::create_dir_all(path.parent().unwrap());
        let db = Connection::open(path)?;
        db.busy_timeout(std::time::Duration::from_secs(10))?;
        // switching to WAL needs a moment alone with the file; workers start together, so retry
        let mut tries = 0;
        loop {
            match db.query_row("PRAGMA journal_mode=WAL", [], |r| r.get::<_, String>(0)) {
                Ok(_) => break,
                Err(_) if tries < 50 => {
                    tries += 1;
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
                Err(e) => return Err(e),
            }
        }
        db.execute_batch(
            "PRAGMA synchronous=NORMAL;
            CREATE TABLE IF NOT EXISTS folders(
                acct TEXT, name TEXT, delim TEXT, role TEXT, unseen INT DEFAULT 0, total INT DEFAULT 0,
                uidvalidity INT DEFAULT 0, PRIMARY KEY(acct, name));
            CREATE TABLE IF NOT EXISTS msgs(
                acct TEXT, folder TEXT, uid INT, msgid TEXT, subject TEXT, from_name TEXT, from_addr TEXT,
                to_addr TEXT, cc_addr TEXT, date INT, seen INT, flagged INT, answered INT, attach INT,
                size INT, refs TEXT, preview TEXT DEFAULT '', raw BLOB, troot TEXT DEFAULT '',
                PRIMARY KEY(acct, folder, uid));
            CREATE INDEX IF NOT EXISTS msgs_date ON msgs(acct, folder, date DESC);
            CREATE TABLE IF NOT EXISTS trusted(domain TEXT PRIMARY KEY);",
        )?;
        migrate(&db)?;
        Ok(Store { db })
    }

    /// Senders (by domain or full address) whose remote images load without asking.
    pub fn is_trusted(&self, who: &str) -> bool {
        self.db.query_row("SELECT 1 FROM trusted WHERE domain = ?1", [who.to_lowercase()], |_| Ok(())).is_ok()
    }

    pub fn set_trusted(&self, who: &str, on: bool) {
        let who = who.to_lowercase();
        let _ = if on {
            self.db.execute("INSERT OR IGNORE INTO trusted(domain) VALUES(?1)", [who])
        } else {
            self.db.execute("DELETE FROM trusted WHERE domain = ?1", [who])
        };
    }

    pub fn folders(&self, acct: &str) -> Vec<Folder> {
        let mut st = self
            .db
            .prepare_cached("SELECT acct, name, delim, role, unseen, total FROM folders WHERE acct = ?1")
            .unwrap();
        let mut out: Vec<Folder> = st
            .query_map([acct], |r| {
                Ok(Folder {
                    acct: r.get(0)?,
                    name: r.get(1)?,
                    delim: r.get(2)?,
                    role: r.get(3)?,
                    unseen: r.get(4)?,
                    total: r.get(5)?,
                })
            })
            .unwrap()
            .flatten()
            .collect();
        out.sort_by_key(|a| (role_rank(&a.role), a.name.to_lowercase()));
        out
    }

    /// Replaces the folder list, keeping counts and uidvalidity of folders that still exist.
    pub fn set_folders(&mut self, acct: &str, list: &[(String, String, String)]) {
        let tx = self.db.transaction().unwrap();
        {
            let mut up = tx
                .prepare_cached(
                    "INSERT INTO folders(acct, name, delim, role) VALUES(?1, ?2, ?3, ?4)
                     ON CONFLICT(acct, name) DO UPDATE SET delim = ?3, role = ?4",
                )
                .unwrap();
            for (name, delim, role) in list {
                up.execute(params![acct, name, delim, role]).unwrap();
            }
            let existing: Vec<String> = tx
                .prepare("SELECT name FROM folders WHERE acct = ?1")
                .unwrap()
                .query_map([acct], |r| r.get(0))
                .unwrap()
                .flatten()
                .collect();
            for name in existing.iter().filter(|n| !list.iter().any(|(l, _, _)| l == *n)) {
                tx.execute("DELETE FROM folders WHERE acct = ?1 AND name = ?2", params![acct, name]).unwrap();
                tx.execute("DELETE FROM msgs WHERE acct = ?1 AND folder = ?2", params![acct, name]).unwrap();
            }
        }
        tx.commit().unwrap();
    }

    pub fn set_counts(&self, acct: &str, folder: &str, unseen: u32, total: u32) {
        let _ = self.db.execute(
            "UPDATE folders SET unseen = ?3, total = ?4 WHERE acct = ?1 AND name = ?2",
            params![acct, folder, unseen, total],
        );
    }

    pub fn add_unseen(&self, acct: &str, folder: &str, delta: i64) {
        let _ = self.db.execute(
            "UPDATE folders SET unseen = MAX(0, unseen + ?3) WHERE acct = ?1 AND name = ?2",
            params![acct, folder, delta],
        );
    }

    pub fn uidvalidity(&self, acct: &str, folder: &str) -> u32 {
        self.db
            .query_row("SELECT uidvalidity FROM folders WHERE acct = ?1 AND name = ?2", params![acct, folder], |r| r.get(0))
            .unwrap_or(0)
    }

    pub fn set_uidvalidity(&self, acct: &str, folder: &str, v: u32) {
        let _ = self.db.execute(
            "UPDATE folders SET uidvalidity = ?3 WHERE acct = ?1 AND name = ?2",
            params![acct, folder, v],
        );
    }

    pub fn clear_folder(&self, acct: &str, folder: &str) {
        let _ = self.db.execute("DELETE FROM msgs WHERE acct = ?1 AND folder = ?2", params![acct, folder]);
    }

    /// uid -> (seen, flagged, answered) of every cached message in the folder.
    pub fn flags(&self, acct: &str, folder: &str) -> std::collections::HashMap<u32, (bool, bool, bool)> {
        let mut st = self
            .db
            .prepare_cached("SELECT uid, seen, flagged, answered FROM msgs WHERE acct = ?1 AND folder = ?2")
            .unwrap();
        st.query_map(params![acct, folder], |r| Ok((r.get(0)?, (r.get(1)?, r.get(2)?, r.get(3)?))))
            .unwrap()
            .flatten()
            .collect()
    }

    pub fn put_headers(&mut self, acct: &str, folder: &str, headers: &[Header]) {
        let tx = self.db.transaction().unwrap();
        {
            let mut st = tx
                .prepare_cached(
                    "INSERT INTO msgs(acct, folder, uid, msgid, subject, from_name, from_addr, to_addr, cc_addr, date,
                        seen, flagged, answered, attach, size, refs, troot)
                     VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)
                     ON CONFLICT(acct, folder, uid) DO UPDATE SET seen = ?11, flagged = ?12, answered = ?13",
                )
                .unwrap();
            for h in headers {
                st.execute(params![
                    acct, folder, h.uid, h.msgid, h.subject, h.from_name, h.from_addr, h.to, h.cc, h.date, h.seen,
                    h.flagged, h.answered, h.attach, h.size, h.refs, root_of(&h.msgid, &h.refs)
                ])
                .unwrap();
            }
        }
        tx.commit().unwrap();
    }

    pub fn set_flags_bulk(&mut self, acct: &str, folder: &str, flags: &[(u32, bool, bool, bool)]) {
        let tx = self.db.transaction().unwrap();
        {
            let mut st = tx
                .prepare_cached(
                    "UPDATE msgs SET seen = ?4, flagged = ?5, answered = ?6 WHERE acct = ?1 AND folder = ?2 AND uid = ?3",
                )
                .unwrap();
            for (uid, seen, flagged, answered) in flags {
                st.execute(params![acct, folder, uid, seen, flagged, answered]).unwrap();
            }
        }
        tx.commit().unwrap();
    }

    /// Sets one flag column ("seen" / "flagged" / "answered") locally.
    pub fn set_flag(&self, acct: &str, folder: &str, uid: u32, col: &str, on: bool) {
        let col = match col {
            "seen" => "seen",
            "flagged" => "flagged",
            _ => "answered",
        };
        let _ = self.db.execute(
            &format!("UPDATE msgs SET {col} = ?4 WHERE acct = ?1 AND folder = ?2 AND uid = ?3"),
            params![acct, folder, uid, on],
        );
    }

    /// Marks every cached message in the folder read and zeroes its unread count.
    pub fn set_folder_seen(&self, acct: &str, folder: &str) {
        let _ = self.db.execute("UPDATE msgs SET seen = 1 WHERE acct = ?1 AND folder = ?2", params![acct, folder]);
        let _ = self.db.execute("UPDATE folders SET unseen = 0 WHERE acct = ?1 AND name = ?2", params![acct, folder]);
    }

    pub fn delete(&mut self, acct: &str, folder: &str, uids: &[u32]) {
        let tx = self.db.transaction().unwrap();
        for uid in uids {
            tx.execute("DELETE FROM msgs WHERE acct = ?1 AND folder = ?2 AND uid = ?3", params![acct, folder, uid]).unwrap();
        }
        tx.commit().unwrap();
    }

    pub fn set_body(&self, acct: &str, folder: &str, uid: u32, raw: &[u8], preview: &str, attach: bool) {
        let _ = self.db.execute(
            "UPDATE msgs SET raw = ?4, preview = ?5, attach = ?6 WHERE acct = ?1 AND folder = ?2 AND uid = ?3",
            params![acct, folder, uid, raw, preview, attach],
        );
    }

    pub fn raw(&self, acct: &str, folder: &str, uid: u32) -> Option<Vec<u8>> {
        self.db
            .query_row(
                "SELECT raw FROM msgs WHERE acct = ?1 AND folder = ?2 AND uid = ?3",
                params![acct, folder, uid],
                |r| r.get::<_, Option<Vec<u8>>>(0),
            )
            .optional()
            .ok()
            .flatten()
            .flatten()
    }

    /// Newest messages without a cached body, small ones only, for background prefetch.
    pub fn missing_bodies(&self, acct: &str, folder: &str, limit: u32) -> Vec<u32> {
        let mut st = self
            .db
            .prepare_cached(
                "SELECT uid FROM (SELECT uid, raw, size, date FROM msgs WHERE acct = ?1 AND folder = ?2
                    ORDER BY date DESC LIMIT ?3) WHERE raw IS NULL AND size < 3000000 ORDER BY date DESC",
            )
            .unwrap();
        st.query_map(params![acct, folder, limit], |r| r.get(0)).unwrap().flatten().collect()
    }

    /// Cached messages beyond the newest `keep`, to trim the cache.
    pub fn trim(&self, acct: &str, folder: &str, keep: u32) {
        let _ = self.db.execute(
            "DELETE FROM msgs WHERE acct = ?1 AND folder = ?2 AND uid NOT IN
               (SELECT uid FROM msgs WHERE acct = ?1 AND folder = ?2 ORDER BY date DESC LIMIT ?3)",
            params![acct, folder, keep],
        );
    }

    /// The message list for a view, newest first. `query` filters subject, sender, recipients and
    /// the cached body text; terms are ANDed.
    pub fn list(&self, view: &View, query: &str, limit: u32) -> Vec<Row> {
        let mut sql = String::from(
            "SELECT m.acct, m.folder, m.uid, m.subject, m.from_name, m.from_addr, m.to_addr, m.date, m.seen, m.flagged,
                    m.answered, m.attach, m.preview, m.msgid, m.refs FROM msgs m ",
        );
        let mut args: Vec<String> = Vec::new();
        match view {
            View::Unified => sql += "JOIN folders f ON f.acct = m.acct AND f.name = m.folder WHERE f.role = 'inbox' ",
            View::Folder { acct, folder } => {
                sql += "WHERE m.acct = ? AND m.folder = ? ";
                args.push(acct.clone());
                args.push(folder.clone());
            }
        }
        for term in query.split_whitespace() {
            let (col, value) = match term.split_once(':') {
                Some(("from", v)) => ("m.from_name || ' ' || m.from_addr", v),
                Some(("to", v)) => ("m.to_addr || ' ' || m.cc_addr", v),
                Some(("subject", v)) => ("m.subject", v),
                _ => ("m.subject || ' ' || m.from_name || ' ' || m.from_addr || ' ' || m.to_addr || ' ' || m.preview", term),
            };
            match term {
                "is:unread" => sql += "AND m.seen = 0 ",
                "is:read" => sql += "AND m.seen = 1 ",
                "is:starred" | "is:flagged" => sql += "AND m.flagged = 1 ",
                "has:attachment" => sql += "AND m.attach = 1 ",
                _ => {
                    sql += &format!("AND ({col}) LIKE ? ESCAPE '\\' ");
                    args.push(format!("%{}%", value.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")));
                }
            }
        }
        sql += &format!("ORDER BY m.date DESC LIMIT {limit}");
        let mut st = self.db.prepare(&sql).unwrap();
        st.query_map(rusqlite::params_from_iter(args.iter()), row).unwrap().flatten().collect()
    }

    /// Messages in inboxes, Sent and archives outside `view` whose thread root is one of `ids`, for
    /// conversations that span folders.
    pub fn thread_mates(&self, ids: &[&str], view: &View) -> Vec<Row> {
        if ids.is_empty() {
            return Vec::new();
        }
        let mut json = String::from("[");
        for (i, id) in ids.iter().enumerate() {
            if i > 0 {
                json.push(',');
            }
            json.push('"');
            for c in id.chars() {
                match c {
                    '"' | '\\' => {
                        json.push('\\');
                        json.push(c);
                    }
                    c if c.is_control() => {}
                    c => json.push(c),
                }
            }
            json.push('"');
        }
        json.push(']');
        let mut st = self
            .db
            .prepare_cached(
                "SELECT m.acct, m.folder, m.uid, m.subject, m.from_name, m.from_addr, m.to_addr, m.date, m.seen, m.flagged,
                    m.answered, m.attach, m.preview, m.msgid, m.refs FROM msgs m
                 JOIN folders f ON f.acct = m.acct AND f.name = m.folder
                 WHERE m.troot IN (SELECT value FROM json_each(?1)) AND f.role IN ('inbox', 'sent', 'archive')
                    AND NOT (f.role = 'inbox' AND ?2 = '') AND NOT (m.acct = ?2 AND m.folder = ?3)",
            )
            .unwrap();
        let (acct, folder) = match view {
            View::Unified => ("", ""),
            View::Folder { acct, folder } => (acct.as_str(), folder.as_str()),
        };
        st.query_map(params![json, acct, folder], row).unwrap().flatten().collect()
    }

    /// Unseen count of every account's inbox.
    pub fn unified_unseen(&self) -> u32 {
        self.db.query_row("SELECT COALESCE(SUM(unseen), 0) FROM folders WHERE role = 'inbox'", [], |r| r.get(0)).unwrap_or(0)
    }

    /// Distinct addresses seen in From/To, for completion in the composer.
    pub fn contacts(&self, prefix: &str, limit: u32) -> Vec<String> {
        let like = format!("%{}%", prefix.replace('%', ""));
        let mut st = self
            .db
            .prepare_cached(
                "SELECT from_name, from_addr, COUNT(*) c FROM msgs WHERE from_addr LIKE ?1 OR from_name LIKE ?1
                 GROUP BY from_addr ORDER BY c DESC LIMIT ?2",
            )
            .unwrap();
        st.query_map(params![like, limit], |r| {
            let name: String = r.get(0)?;
            let addr: String = r.get(1)?;
            Ok(if name.is_empty() || name == addr { addr } else { format!("{name} <{addr}>") })
        })
        .unwrap()
        .flatten()
        .collect()
    }
}

fn row(r: &rusqlite::Row) -> rusqlite::Result<Row> {
    Ok(Row {
        acct: r.get(0)?,
        folder: r.get(1)?,
        uid: r.get(2)?,
        subject: r.get(3)?,
        from_name: r.get(4)?,
        from_addr: r.get(5)?,
        to: r.get(6)?,
        date: r.get(7)?,
        seen: r.get(8)?,
        flagged: r.get(9)?,
        answered: r.get(10)?,
        attach: r.get(11)?,
        preview: r.get(12)?,
        msgid: r.get::<_, Option<String>>(13)?.unwrap_or_default(),
        refs: r.get::<_, Option<String>>(14)?.unwrap_or_default(),
        count: 0,
        people: String::new(),
    })
}

/// The first id of References (the thread's start), else the message's own id.
pub fn root_of<'a>(msgid: &'a str, refs: &'a str) -> &'a str {
    refs.split_whitespace().next().unwrap_or(msgid)
}

/// Adds the thread root column to caches from before threading, in one locked step so workers
/// opening the cache together do not race.
fn migrate(db: &Connection) -> rusqlite::Result<()> {
    let has = |db: &Connection| -> rusqlite::Result<bool> {
        Ok(db.prepare("SELECT 1 FROM pragma_table_info('msgs') WHERE name = 'troot'")?.exists([])?)
    };
    if !has(db)? {
        db.execute_batch("BEGIN IMMEDIATE")?;
        let done: rusqlite::Result<()> = (|| {
            if !has(db)? {
                db.execute_batch(
                    "ALTER TABLE msgs ADD COLUMN troot TEXT DEFAULT '';
                     UPDATE msgs SET troot = CASE WHEN TRIM(COALESCE(refs, '')) = '' THEN COALESCE(msgid, '')
                        ELSE substr(TRIM(refs), 1, instr(TRIM(refs) || ' ', ' ') - 1) END;",
                )?;
            }
            Ok(())
        })();
        db.execute_batch(if done.is_ok() { "COMMIT" } else { "ROLLBACK" })?;
        done?;
    }
    db.execute_batch("CREATE INDEX IF NOT EXISTS msgs_troot ON msgs(troot)")
}

/// Sidebar order: inbox first, then the special folders, then the rest by name.
pub fn role_rank(role: &str) -> u8 {
    match role {
        "inbox" => 0,
        "flagged" => 1,
        "drafts" => 2,
        "sent" => 3,
        "archive" => 4,
        "junk" => 5,
        "trash" => 6,
        "all" => 7,
        _ => 8,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(uid: u32, subject: &str, date: i64) -> Header {
        Header {
            uid,
            msgid: String::new(),
            subject: subject.into(),
            from_name: "Lea".into(),
            from_addr: "lea@example.com".into(),
            to: String::new(),
            cc: String::new(),
            date,
            seen: false,
            flagged: false,
            answered: false,
            attach: false,
            size: 10,
            refs: String::new(),
        }
    }

    #[test]
    fn list_filters_and_sorts() {
        let dir = std::env::temp_dir().join(format!("brief-test-{}", std::process::id()));
        unsafe { std::env::set_var("XDG_CACHE_HOME", &dir) };
        let mut s = Store::open().unwrap();
        s.set_folders("a", &[("INBOX".into(), "/".into(), "inbox".into())]);
        s.put_headers("a", "INBOX", &[header(1, "Hello 100%", 10), header(2, "Invoice", 20)]);
        let view = View::Folder { acct: "a".into(), folder: "INBOX".into() };
        let all = s.list(&view, "", 50);
        assert_eq!(all.iter().map(|r| r.uid).collect::<Vec<_>>(), [2, 1]);
        assert_eq!(s.list(&view, "100%", 50).len(), 1);
        assert_eq!(s.list(&View::Unified, "invoice", 50).len(), 1);
        s.set_flag("a", "INBOX", 2, "seen", true);
        assert_eq!(s.list(&view, "is:unread", 50).len(), 1);
        s.trim("a", "INBOX", 1);
        assert_eq!(s.list(&view, "", 50).len(), 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// List build time on a big cache:
    /// `XDG_CACHE_HOME=<dir> cargo test --release bench_list -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn bench_list() {
        let s = Store::open().unwrap();
        let (acct, folder): (String, String) =
            s.db.query_row("SELECT acct, name FROM folders WHERE role = 'inbox'", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        let view = View::Folder { acct, folder };
        let me = vec!["me@example.com".to_owned()];
        let median = |f: &dyn Fn() -> usize| {
            let mut times: Vec<_> = (0..51)
                .map(|_| {
                    let t = std::time::Instant::now();
                    std::hint::black_box(f());
                    t.elapsed()
                })
                .collect();
            times.sort();
            (f(), times[25])
        };
        // the default cache_limit, and everything
        for limit in [1000, 5000] {
            let (n, plain) = median(&|| s.list(&view, "", limit).len());
            let (c, cold) = median(&|| {
                let rows = s.list(&view, "", limit);
                let mut mates = crate::thread::Mates::default();
                mates.fill(&s, &view, &rows);
                crate::thread::conversations(rows, &mates.rows, &me).0.len()
            });
            let mates = std::cell::RefCell::new(crate::thread::Mates::default());
            let (_, warm) = median(&|| {
                let rows = s.list(&view, "", limit);
                mates.borrow_mut().fill(&s, &view, &rows);
                crate::thread::conversations(rows, &mates.borrow().rows, &me).0.len()
            });
            eprintln!("median of 51: list {n} rows {plain:?}; as {c} conversations {cold:?}, other folders cached {warm:?}");
        }
    }

    #[test]
    fn old_cache_gets_thread_roots() {
        let dir = std::env::temp_dir().join(format!("brief-test-migrate-{}", std::process::id()));
        let path = dir.join("mail.db");
        let _ = std::fs::create_dir_all(&dir);
        {
            let db = Connection::open(&path).unwrap();
            db.execute_batch(
                "CREATE TABLE folders(acct TEXT, name TEXT, delim TEXT, role TEXT, unseen INT DEFAULT 0, total INT DEFAULT 0,
                    uidvalidity INT DEFAULT 0, PRIMARY KEY(acct, name));
                CREATE TABLE msgs(acct TEXT, folder TEXT, uid INT, msgid TEXT, subject TEXT, from_name TEXT, from_addr TEXT,
                    to_addr TEXT, cc_addr TEXT, date INT, seen INT, flagged INT, answered INT, attach INT,
                    size INT, refs TEXT, preview TEXT DEFAULT '', raw BLOB, PRIMARY KEY(acct, folder, uid));
                INSERT INTO folders VALUES('a', 'INBOX', '/', 'inbox', 0, 0, 1), ('a', 'Sent', '/', 'sent', 0, 0, 1);
                INSERT INTO msgs VALUES
                    ('a', 'INBOX', 1, '<a@x>', 'Plan', '', 'l@x', 'me@x', '', 1, 1, 0, 0, 0, 9, '', '', NULL),
                    ('a', 'INBOX', 2, '<c@x>', 'Re: Plan', '', 'l@x', 'me@x', '', 3, 1, 0, 0, 0, 9, '<a@x> <b@x>', '', NULL),
                    ('a', 'Sent', 1, '<b@x>', 'Re: Plan', '', 'me@x', 'l@x', '', 2, 1, 0, 0, 0, 9, '<a@x>', '', NULL),
                    ('a', 'Sent', 2, '<z@x>', 'Other', '', 'me@x', 'l@x', '', 2, 1, 0, 0, 0, 9, NULL, '', NULL);",
            )
            .unwrap();
        }
        let mut s = Store::open_at(&path).unwrap();
        let roots: Vec<String> =
            s.db.prepare("SELECT troot FROM msgs ORDER BY folder, uid").unwrap().query_map([], |r| r.get(0)).unwrap().flatten().collect();
        assert_eq!(roots, ["<a@x>", "<a@x>", "<a@x>", "<z@x>"]);
        assert_eq!(s.thread_mates(&["<a@x>"], &View::Folder { acct: "b".into(), folder: "INBOX".into() }).len(), 3);
        let inbox = View::Folder { acct: "a".into(), folder: "INBOX".into() };
        assert_eq!(s.thread_mates(&["<a@x>"], &inbox).len(), 1);
        assert_eq!(s.thread_mates(&["<a@x>"], &View::Unified).len(), 1);
        // a second open finds the column and leaves it alone
        drop(s);
        s = Store::open_at(&path).unwrap();
        let h = Header { msgid: "<d@x>".into(), refs: "<a@x> <c@x>".into(), ..header(3, "Re: Plan", 4) };
        s.put_headers("a", "INBOX", &[h]);
        assert_eq!(s.thread_mates(&["<a@x>", "\"odd\\id"], &View::Folder { acct: "b".into(), folder: "x".into() }).len(), 4);
        let _ = std::fs::remove_dir_all(dir);
    }
}
