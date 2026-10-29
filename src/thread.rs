//! Conversations: messages joined by Message-ID / References (In-Reply-To is folded into
//! `refs` on fetch), with a subject fallback for replies that carry neither.
//!
//! Everything works on rows already loaded for the list plus one indexed query for the same
//! conversations' messages in other folders (your replies in Sent), so a list build stays one pass.

use crate::store::{Row, Store, View, root_of};
use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
use std::hash::{BuildHasherDefault, Hasher};

/// FNV-1a: message ids are short and trusted enough, and SipHash showed up in list build times.
#[derive(Default)]
struct Fnv(u64);

impl Hasher for Fnv {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        let mut h = if self.0 == 0 { 0xcbf29ce484222325 } else { self.0 };
        for b in bytes {
            h = (h ^ *b as u64).wrapping_mul(0x100000001b3);
        }
        self.0 = h;
    }
}

type Map<K, V> = HashMap<K, V, BuildHasherDefault<Fnv>>;
type Set<K> = HashSet<K, BuildHasherDefault<Fnv>>;

/// A reply without references joins a same-subject conversation only this close in time.
const SUBJECT_WINDOW: i64 = 30 * 86400;

/// Reply and forward markers: English, German, Dutch, Nordic, French, Italian, Polish.
const PREFIXES: &[&str] = &["re", "aw", "antw", "antwort", "fw", "fwd", "wg", "wtr", "sv", "vs", "tr", "rv", "ref", "enc", "rif", "odp"];

/// One line of the list in thread mode.
#[derive(Clone, Debug)]
pub struct Conv {
    /// The view's own messages, newest first: what actions on the row apply to.
    pub members: Vec<Row>,
    /// The same conversation's messages in other folders (Sent, Archive, Inbox).
    pub mates: Vec<Row>,
}

impl Conv {
    /// Every message, oldest first, as the reader shows them.
    pub fn messages(&self) -> Vec<Row> {
        let mut all: Vec<Row> = self.members.iter().chain(&self.mates).cloned().collect();
        all.sort_by_key(|r| r.date);
        all
    }

    /// The list row: the newest own message, with the conversation's subject, people, count and
    /// flags (unread if any own message is).
    pub fn row(&self, me: &[String]) -> Row {
        let mut rep = self.members[0].clone();
        rep.count = (self.members.len() + self.mates.len()) as u32;
        if rep.count > 1 {
            let mut msgs: Vec<&Row> = self.members.iter().chain(&self.mates).collect();
            msgs.sort_by_key(|r| r.date);
            rep.subject = msgs[0].subject.clone();
            rep.people = people(&msgs, me);
            rep.seen = self.members.iter().all(|r| r.seen);
            rep.flagged = self.members.iter().any(|r| r.flagged);
            rep.answered = self.members.iter().any(|r| r.answered);
            rep.attach = msgs.iter().any(|r| r.attach);
        }
        rep
    }

    pub fn contains(&self, k: &(String, String, u32)) -> bool {
        self.members.iter().chain(&self.mates).any(|r| r.acct == k.0 && r.folder == k.1 && r.uid == k.2)
    }
}

/// The subject without reply/forward prefixes ("Re: AW[2]: x" -> "x"), lowercased with single
/// spaces, and whether there were any.
pub fn base_subject(subject: &str) -> (String, bool) {
    let mut rest = subject.trim();
    let mut had = false;
    while let Some(r) = strip_prefix(rest) {
        rest = r;
        had = true;
    }
    (rest.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase(), had)
}

/// The subject after one leading "Re:" / "AW[2]:" / ..., if it starts with one.
fn strip_prefix(s: &str) -> Option<&str> {
    let (colon, c) = s.char_indices().take(12).find(|(_, c)| matches!(c, ':' | '：'))?;
    let head = s[..colon].trim_end();
    let head = head.trim_end_matches(|c: char| c.is_ascii_digit() || matches!(c, '[' | ']' | '(' | ')')).trim_end();
    PREFIXES.iter().any(|p| head.eq_ignore_ascii_case(p)).then(|| s[colon + c.len_utf8()..].trim_start())
}

fn find(parent: &mut [usize], mut i: usize) -> usize {
    while parent[i] != i {
        parent[i] = parent[parent[i]];
        i = parent[i];
    }
    i
}

fn union(parent: &mut [usize], a: usize, b: usize) {
    let (a, b) = (find(parent, a), find(parent, b));
    if a != b {
        parent[a.max(b)] = a.min(b);
    }
}

/// Whether two messages share a person: same sender, or one's sender among the other's recipients.
fn related(a: &Row, b: &Row) -> bool {
    let has = |hay: &str, needle: &str| {
        !needle.is_empty() && hay.as_bytes().windows(needle.len()).any(|w| w.eq_ignore_ascii_case(needle.as_bytes()))
    };
    !a.from_addr.is_empty() && a.from_addr.eq_ignore_ascii_case(&b.from_addr) || has(&b.to, &a.from_addr) || has(&a.to, &b.from_addr)
}

/// Splits rows into conversations: groups of row indices, newest first, in the order of each
/// group's newest message. Rows that mention a common message id are joined; a reply with no
/// references joins the closest same-subject message with a shared person within the window,
/// preferring earlier ones. Also returns every message id seen, mapped to a row that mentions it.
fn group(rows: &[Row]) -> (Vec<Vec<usize>>, Map<&str, usize>) {
    let n = rows.len();
    let mut parent: Vec<usize> = (0..n).collect();
    let mut by_id: Map<&str, usize> = Map::with_capacity_and_hasher(n * 2, Default::default());
    for (i, r) in rows.iter().enumerate() {
        for id in std::iter::once(r.msgid.as_str()).chain(r.refs.split_whitespace()).filter(|id| !id.is_empty()) {
            match by_id.entry(id) {
                Entry::Occupied(e) => union(&mut parent, i, *e.get()),
                Entry::Vacant(e) => {
                    e.insert(i);
                }
            }
        }
    }
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(rows[i].date));
    // replies without references: usually none, so subjects are only normalized when there are
    let lone: Vec<usize> = (0..n).filter(|&i| rows[i].refs.is_empty() && strip_prefix(rows[i].subject.trim()).is_some()).collect();
    if !lone.is_empty() {
        let bases: Vec<String> = rows.iter().map(|r| base_subject(&r.subject).0).collect();
        let mut by_subject: Map<(&str, &str), Vec<usize>> = Map::default();
        for (i, r) in rows.iter().enumerate().filter(|(i, _)| !bases[*i].is_empty()) {
            by_subject.entry((r.acct.as_str(), bases[i].as_str())).or_default().push(i);
        }
        for i in lone {
            let r = &rows[i];
            let Some(mates) = by_subject.get(&(r.acct.as_str(), bases[i].as_str())) else { continue };
            let best = mates
                .iter()
                .copied()
                .filter(|&j| j != i && (rows[j].date - r.date).abs() <= SUBJECT_WINDOW && related(r, &rows[j]))
                .min_by_key(|&j| (rows[j].date > r.date, (rows[j].date - r.date).abs()));
            if let Some(j) = best {
                union(&mut parent, i, j);
            }
        }
    }
    let mut slot: Map<usize, usize> = Map::default();
    let mut out: Vec<Vec<usize>> = Vec::new();
    for i in order {
        let root = find(&mut parent, i);
        let g = *slot.entry(root).or_insert_with(|| {
            out.push(Vec::new());
            out.len() - 1
        });
        out[g].push(i);
    }
    (out, by_id)
}

/// "Lea, Tom, me": senders oldest first, first names, "me" for `me`; long lists keep the first
/// and the last two.
fn people(msgs: &[&Row], me: &[String]) -> String {
    let mut seen: Vec<String> = Vec::new();
    let mut names: Vec<String> = Vec::new();
    for r in msgs {
        let addr = r.from_addr.to_lowercase();
        if seen.contains(&addr) {
            continue;
        }
        seen.push(addr.clone());
        let name = if me.contains(&addr) {
            "me".to_owned()
        } else {
            let full = r.from_name.trim().trim_matches('"');
            let first = full.split_whitespace().next().unwrap_or("");
            // "Meier, Lea" style
            let first = if first.ends_with(',') { full.split_whitespace().nth(1).unwrap_or(first) } else { first };
            if first.is_empty() { addr.split('@').next().unwrap_or(&addr).to_owned() } else { first.to_owned() }
        };
        names.push(name);
    }
    // "me" goes last unless it is all there is
    if let Some(i) = names.iter().position(|n| n == "me").filter(|_| names.len() > 1) {
        let m = names.remove(i);
        names.push(m);
    }
    if names.len() > 3 {
        format!("{} .. {}", names[0], names[names.len() - 2..].join(", "))
    } else {
        names.join(", ")
    }
}

/// Ids that find a view's messages in other folders (`Store::thread_mates`): each row's thread
/// root and own id.
fn ids(rows: &[Row]) -> Vec<&str> {
    let mut ids: Set<&str> = Set::with_capacity_and_hasher(rows.len() * 2, Default::default());
    for r in rows {
        ids.insert(root_of(&r.msgid, &r.refs));
        ids.insert(&r.msgid);
    }
    ids.remove("");
    ids.into_iter().collect()
}

/// Messages from other folders already looked up for one view, so a rebuild (a flag change, a
/// search keystroke, new mail) only queries ids it has not asked about. Whoever sees another
/// folder change clears it.
#[derive(Default)]
pub struct Mates {
    view: Option<View>,
    asked: Set<String>,
    pub rows: Vec<Row>,
}

impl Mates {
    /// Brings the cache up to date for the view's rows: one query for new ids at most.
    pub fn fill(&mut self, store: &Store, view: &View, rows: &[Row]) {
        if self.view.as_ref() != Some(view) {
            *self = Mates { view: Some(view.clone()), ..Default::default() };
        }
        let new: Vec<&str> = ids(rows).into_iter().filter(|id| !self.asked.contains(*id)).collect();
        if new.is_empty() {
            return;
        }
        self.rows.extend(store.thread_mates(&new, view));
        self.asked.extend(new.into_iter().map(str::to_owned));
    }

    pub fn clear(&mut self) {
        *self = Mates::default();
    }
}

/// Turns the view's rows (newest first) into one row per conversation plus its messages. `mates`
/// are candidates from other folders (see `Mates`); they only attach to conversations the view
/// already has, never start one. `me` holds the accounts' own addresses, lowercased.
pub fn conversations(rows: Vec<Row>, mates: &[Row], me: &[String]) -> (Vec<Row>, Vec<Conv>) {
    let (groups, by_id) = group(&rows);
    let mut conv_of = vec![0; rows.len()];
    for (c, g) in groups.iter().enumerate() {
        for &i in g {
            conv_of[i] = c;
        }
    }
    let mut attached: Vec<Vec<Row>> = vec![Vec::new(); groups.len()];
    // one copy of each message: Gmail-like servers keep the same mail in several folders
    let mut msgids: Set<&str> = Set::default();
    for m in mates {
        if !m.msgid.is_empty() && (by_id.get(m.msgid.as_str()).is_some_and(|&i| rows[i].msgid == m.msgid) || !msgids.insert(&m.msgid)) {
            continue;
        }
        let found = std::iter::once(m.msgid.as_str()).chain(m.refs.split_whitespace()).find_map(|id| by_id.get(id));
        if let Some(&i) = found {
            attached[conv_of[i]].push(m.clone());
        }
    }
    drop(by_id);
    let mut slots: Vec<Option<Row>> = rows.into_iter().map(Some).collect();
    let convs: Vec<Conv> = groups
        .into_iter()
        .zip(attached)
        .map(|(g, mates)| Conv { members: g.into_iter().map(|i| slots[i].take().unwrap()).collect(), mates })
        .collect();
    let reps = convs.iter().map(|c| c.row(me)).collect();
    (reps, convs)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(uid: u32, msgid: &str, refs: &str, subject: &str, from: &str, date: i64) -> Row {
        Row {
            acct: "a".into(),
            folder: "INBOX".into(),
            uid,
            subject: subject.into(),
            from_name: String::new(),
            from_addr: from.into(),
            to: "me@x.ch".into(),
            date,
            seen: true,
            flagged: false,
            answered: false,
            attach: false,
            preview: String::new(),
            msgid: msgid.into(),
            refs: refs.into(),
            count: 0,
            people: String::new(),
        }
    }

    fn uids(rows: &[Row]) -> Vec<Vec<u32>> {
        group(rows).0.into_iter().map(|g| g.into_iter().map(|i| rows[i].uid).collect()).collect()
    }

    #[test]
    fn strips_prefixes_in_several_languages() {
        assert_eq!(base_subject("Re: AW: Offerte  Küche"), ("offerte küche".into(), true));
        assert_eq!(base_subject("WG: Fwd: RE[2]: Plan"), ("plan".into(), true));
        assert_eq!(base_subject("Antw: SV:Meeting"), ("meeting".into(), true));
        assert_eq!(base_subject("Re(3): x"), ("x".into(), true));
        assert_eq!(base_subject("Invoice: October"), ("invoice: october".into(), false));
        assert_eq!(base_subject("Rechnung"), ("rechnung".into(), false));
        assert_eq!(base_subject("  "), (String::new(), false));
    }

    #[test]
    fn follows_reference_chains() {
        let rows = [
            row(1, "<a>", "", "Plan", "lea@x.ch", 100),
            row(2, "<b>", "<a>", "Re: Plan", "tom@x.ch", 200),
            row(3, "<c>", "<a> <b>", "Re: Plan", "lea@x.ch", 300),
            row(4, "<z>", "", "Other", "lea@x.ch", 250),
        ];
        assert_eq!(uids(&rows), [vec![3, 2, 1], vec![4]]);
    }

    #[test]
    fn joins_through_missing_parents() {
        // the root <a> is not cached; both replies name it
        let rows = [row(2, "<b>", "<a>", "Re: Plan", "tom@x.ch", 200), row(3, "<c>", "<a>", "Re: Plan", "lea@x.ch", 300)];
        assert_eq!(uids(&rows), [vec![3, 2]]);
        // only In-Reply-To (folded into refs), pointing at a reply
        let rows = [row(1, "<a>", "", "Plan", "lea@x.ch", 100), row(2, "<b>", "<a>", "Re: Plan", "me@x.ch", 200), row(3, "<c>", "<b>", "AW: Plan", "lea@x.ch", 300)];
        assert_eq!(uids(&rows), [vec![3, 2, 1]]);
    }

    #[test]
    fn subject_fallback_needs_prefix_window_and_person() {
        let day = 86400;
        let rows = [
            row(1, "<a>", "", "Offerte", "lea@x.ch", 0),
            row(2, "<b>", "", "AW: Offerte", "lea@x.ch", 2 * day),
            row(3, "<c>", "", "Re: offerte", "lea@x.ch", 90 * day),
            row(4, "<d>", "", "Re: Offerte", "stranger@y.ch", 3 * day),
        ];
        assert_eq!(uids(&rows), [vec![3], vec![4], vec![2, 1]]);
        // no prefix: two newsletters with one subject stay apart
        let rows = [row(1, "<a>", "", "Weekly digest", "news@x.ch", 0), row(2, "<b>", "", "Weekly digest", "news@x.ch", day)];
        assert_eq!(uids(&rows).len(), 2);
    }

    #[test]
    fn people_puts_me_last_and_shortens() {
        let mut a = row(1, "", "", "", "lea@x.ch", 1);
        a.from_name = "Lea Meier".into();
        let b = row(2, "", "", "", "me@x.ch", 2);
        let mut c = row(3, "", "", "", "tom@x.ch", 3);
        c.from_name = "Keller, Tom".into();
        let d = row(4, "", "", "", "anna@x.ch", 4);
        let me = vec!["me@x.ch".to_owned()];
        assert_eq!(people(&[&a, &b, &c], &me), "Lea, Tom, me");
        assert_eq!(people(&[&a, &b, &c, &d], &me), "Lea .. anna, me");
        assert_eq!(people(&[&b], &me), "me");
    }
}
