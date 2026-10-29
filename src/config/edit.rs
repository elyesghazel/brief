//! Changes one key of config.toml in place: a top-level key, or a field of one `[[account]]`
//! block, also inside an inline table (`imap = { host = ... }`) or an `[account.imap]` table.
//!
//! Only the lines of that key change; comments, blank lines, order and everything else stay as
//! they were. Each result is parsed again and has to equal the old file with just this change,
//! otherwise the edit is refused and the file needs a hand edit.

/// Where a key lives.
#[derive(Clone, Copy, Debug)]
pub enum Scope<'a> {
    Top,
    /// The `[[account]]` block with this `name`.
    Account(&'a str),
}

#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
enum Ml {
    #[default]
    No,
    Basic,
    Literal,
}

/// Lexer state between lines: inside a multi-line string, or brackets still open.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
struct State {
    ml: Ml,
    depth: i32,
}

impl State {
    fn at_rest(self) -> bool {
        self.ml == Ml::No && self.depth <= 0
    }
}

/// Scans one line from `st`; returns the state after it and where a comment starts.
fn scan(line: &str, mut st: State) -> (State, Option<usize>) {
    let b = line.as_bytes();
    let at = |i: usize, s: &str| line[i..].starts_with(s);
    let mut i = 0;
    while i < b.len() {
        match st.ml {
            Ml::Basic => {
                if b[i] == b'\\' {
                    i += 2;
                    continue;
                }
                if at(i, "\"\"\"") {
                    st.ml = Ml::No;
                    i += 3;
                    continue;
                }
            }
            Ml::Literal => {
                if at(i, "'''") {
                    st.ml = Ml::No;
                    i += 3;
                    continue;
                }
            }
            Ml::No => match b[i] {
                b'#' => return (st, Some(i)),
                b'"' if at(i, "\"\"\"") => {
                    st.ml = Ml::Basic;
                    i += 3;
                    continue;
                }
                b'\'' if at(i, "'''") => {
                    st.ml = Ml::Literal;
                    i += 3;
                    continue;
                }
                b'"' => {
                    i += 1;
                    while i < b.len() && b[i] != b'"' {
                        i += if b[i] == b'\\' { 2 } else { 1 };
                    }
                }
                b'\'' => {
                    i += 1;
                    while i < b.len() && b[i] != b'\'' {
                        i += 1;
                    }
                }
                b'[' | b'{' => st.depth += 1,
                b']' | b'}' => st.depth -= 1,
                _ => {}
            },
        }
        i += 1;
    }
    (st, None)
}

#[derive(Debug, PartialEq)]
enum Kind {
    Blank,
    Comment,
    Header { array: bool, name: Vec<String> },
    KeyVal { key: Vec<String> },
}

/// A statement: lines `start..end` (a value may span several).
#[derive(Debug)]
struct Stmt {
    start: usize,
    end: usize,
    kind: Kind,
}

/// `a."b c".d` -> ["a", "b c", "d"]; None for something that is no plain key.
fn split_key(text: &str) -> Option<Vec<String>> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut chars = text.trim().chars().peekable();
    let mut quoted = false;
    while let Some(c) = chars.next() {
        match c {
            '"' | '\'' => {
                let mut done = false;
                for d in chars.by_ref() {
                    if d == c {
                        done = true;
                        break;
                    }
                    cur.push(d);
                }
                if !done {
                    return None;
                }
                quoted = true;
            }
            '.' => {
                out.push(std::mem::take(&mut cur).trim().to_owned());
                quoted = false;
            }
            c if c.is_ascii_alphanumeric() || c == '_' || c == '-' => cur.push(c),
            c if c.is_whitespace() => {}
            _ => return None,
        }
    }
    if cur.is_empty() && !quoted {
        return None;
    }
    out.push(cur);
    Some(out)
}

fn parse(lines: &[&str]) -> Vec<Stmt> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let t = line.trim_start();
        let start = i;
        let (mut st, comment) = scan(line, State::default());
        let code = &line[..comment.unwrap_or(line.len())];
        let kind = if t.trim().is_empty() {
            Kind::Blank
        } else if t.starts_with('#') {
            Kind::Comment
        } else if t.starts_with('[') {
            let c = code.trim();
            let array = c.starts_with("[[") && c.ends_with("]]");
            let inner = if array { &c[2..c.len().saturating_sub(2)] } else { c.trim_start_matches('[').trim_end_matches(']') };
            Kind::Header { array, name: split_key(inner).unwrap_or_default() }
        } else {
            // the key is everything before the first `=` outside quotes
            let mut eq = None;
            let mut q: Option<char> = None;
            for (j, c) in code.char_indices() {
                match (q, c) {
                    (None, '"' | '\'') => q = Some(c),
                    (Some(o), c) if c == o => q = None,
                    (None, '=') => {
                        eq = Some(j);
                        break;
                    }
                    _ => {}
                }
            }
            match eq.and_then(|e| split_key(&code[..e])) {
                Some(key) => Kind::KeyVal { key },
                None => Kind::Comment,
            }
        };
        i += 1;
        while !st.at_rest() && i < lines.len() {
            st = scan(lines[i], st).0;
            i += 1;
        }
        out.push(Stmt { start, end: i, kind });
    }
    out
}

/// A TOML value on one line: strings as basic strings with escapes, so a signature with line
/// breaks stays one line.
pub fn literal(v: &toml::Value) -> String {
    match v {
        toml::Value::String(s) => {
            let mut out = String::from("\"");
            for c in s.chars() {
                match c {
                    '"' => out += "\\\"",
                    '\\' => out += "\\\\",
                    '\n' => out += "\\n",
                    '\t' => out += "\\t",
                    '\r' => out += "\\r",
                    c if (c as u32) < 0x20 || c == '\u{7f}' => out += &format!("\\u{:04X}", c as u32),
                    c => out.push(c),
                }
            }
            out + "\""
        }
        toml::Value::Table(t) => {
            let parts: Vec<String> = t.iter().map(|(k, v)| format!("{} = {}", bare_key(k), literal(v))).collect();
            if parts.is_empty() { "{}".into() } else { format!("{{ {} }}", parts.join(", ")) }
        }
        toml::Value::Array(a) => format!("[{}]", a.iter().map(literal).collect::<Vec<_>>().join(", ")),
        other => other.to_string(),
    }
}

fn bare_key(k: &str) -> String {
    if !k.is_empty() && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        k.to_owned()
    } else {
        literal(&toml::Value::String(k.to_owned()))
    }
}

/// The line ending of a line ("" for the last line without one).
fn ending(line: &str) -> &str {
    if line.ends_with("\r\n") {
        "\r\n"
    } else if line.ends_with('\n') {
        "\n"
    } else {
        ""
    }
}

/// `code` in place of the statement `s`, keeping its indentation and the comment of its last line.
fn replacement(lines: &[&str], s: &Stmt, code: &str) -> String {
    let first = lines[s.start];
    let indent = &first[..first.len() - first.trim_start().len()];
    let last = lines[s.end - 1];
    // the state the last line starts in
    let mut st = State::default();
    for l in &lines[s.start..s.end - 1] {
        st = scan(l, st).0;
    }
    let body = last.trim_end_matches(['\n', '\r']);
    let mut out = format!("{indent}{code}");
    if let (_, Some(c)) = scan(body, st) {
        // the comment stays in its column while the value fits
        let col = if s.end - 1 == s.start { c } else { 0 };
        let gap = col.saturating_sub(out.len()).max(2);
        out += &" ".repeat(gap);
        out += &body[c..];
    }
    out + if ending(last).is_empty() { "\n" } else { ending(last) }
}

/// The text with one key set (`Some`) or removed (`None`). Err says why it was left alone.
pub fn edit(text: &str, scope: Scope, path: &[&str], value: Option<&toml::Value>) -> Result<String, String> {
    if path.is_empty() || path.len() > 2 || (matches!(scope, Scope::Top) && path.len() != 1) {
        return Err("unsupported key".into());
    }
    let want = expected(text, scope, path, value)?;
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let stmts = parse(&lines);
    let out = match scope {
        Scope::Top => edit_top(&lines, &stmts, path[0], value),
        Scope::Account(name) => edit_account(&lines, &stmts, name, path, value)?,
    };
    let got: toml::Table = out.parse().map_err(|e| format!("the edit would break the file ({e})"))?;
    if got != want {
        return Err("the edit would change more than this setting".into());
    }
    Ok(out)
}

/// What the file has to parse to after the edit.
fn expected(text: &str, scope: Scope, path: &[&str], value: Option<&toml::Value>) -> Result<toml::Table, String> {
    let mut table: toml::Table = text.parse().map_err(|e| format!("config.toml does not parse: {e}"))?;
    let target = match scope {
        Scope::Top => &mut table,
        Scope::Account(name) => {
            let list = table.get_mut("account").and_then(|a| a.as_array_mut()).ok_or("no accounts in config.toml")?;
            let mut hits = list.iter_mut().filter(|a| a.get("name").and_then(|n| n.as_str()) == Some(name));
            let acc = hits.next().ok_or_else(|| format!("no account called {name} in config.toml"))?;
            if hits.next().is_some() {
                return Err(format!("two accounts are called {name}"));
            }
            acc.as_table_mut().ok_or("odd account block")?
        }
    };
    let (leaf, parent) = match path {
        [k] => (*k, target),
        [t, k] => {
            let sub = target.entry(t.to_string()).or_insert_with(|| toml::Value::Table(toml::Table::new()));
            (*k, sub.as_table_mut().ok_or_else(|| format!("{t} is not a table"))?)
        }
        _ => unreachable!(),
    };
    match value {
        Some(v) => {
            parent.insert(leaf.to_owned(), v.clone());
        }
        None => {
            parent.remove(leaf);
        }
    }
    Ok(table)
}

fn join(lines: &[&str], at: usize, until: usize, insert: &str) -> String {
    let mut out = lines[..at].concat();
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out += insert;
    out += &lines[until..].concat();
    out
}

fn line_for(key: &str, value: &toml::Value) -> String {
    format!("{} = {}", bare_key(key), literal(value))
}

fn edit_top(lines: &[&str], stmts: &[Stmt], key: &str, value: Option<&toml::Value>) -> String {
    let region: Vec<&Stmt> = stmts.iter().take_while(|s| !matches!(s.kind, Kind::Header { .. })).collect();
    let found = region.iter().find(|s| matches!(&s.kind, Kind::KeyVal { key: k } if k.len() == 1 && k[0] == key));
    match (found, value) {
        (Some(s), Some(v)) => join(lines, s.start, s.end, &replacement(lines, s, &line_for(key, v))),
        (Some(s), None) => join(lines, s.start, s.end, ""),
        (None, None) => lines.concat(),
        (None, Some(v)) => {
            // the template's commented example: `# key = old   # what it does` comes alive
            let example = region.iter().find(|s| {
                s.kind == Kind::Comment
                    && lines[s.start].trim_start().trim_start_matches('#').trim_start().strip_prefix(key).is_some_and(|r| r.trim_start().starts_with('='))
            });
            if let Some(s) = example {
                let line = lines[s.start];
                let body = line.trim_end_matches(['\n', '\r']);
                let rest = body.trim_start().trim_start_matches('#');
                let mut new = line_for(key, v);
                if let Some(c) = rest.find('#') {
                    let col = body.len() - rest.len() + c;
                    new += &" ".repeat(col.saturating_sub(new.len()).max(2));
                    new += &rest[c..];
                }
                return join(lines, s.start, s.end, &(new + if ending(line).is_empty() { "\n" } else { ending(line) }));
            }
            let at = match region.iter().rev().find(|s| matches!(s.kind, Kind::KeyVal { .. })) {
                Some(s) => s.end,
                // after the opening comment, else first
                None => region.iter().take_while(|s| s.kind == Kind::Comment).last().map_or(0, |s| s.end),
            };
            let next_is_header = stmts.iter().find(|s| s.start == at).is_some_and(|s| matches!(s.kind, Kind::Header { .. }));
            let mut new = line_for(key, v) + "\n";
            if next_is_header {
                new += "\n";
            }
            join(lines, at, at, &new)
        }
    }
}

/// The account block named `name`: its statements, up to the next header that is not one of its
/// sub-tables.
fn account_block<'s>(lines: &[&str], stmts: &'s [Stmt], name: &str) -> Result<&'s [Stmt], String> {
    let mut found = None;
    for (i, s) in stmts.iter().enumerate() {
        if !matches!(&s.kind, Kind::Header { array: true, name } if name == &["account"]) {
            continue;
        }
        let end = stmts[i + 1..]
            .iter()
            .position(|t| matches!(&t.kind, Kind::Header { name, .. } if name.first().map(String::as_str) != Some("account") || name.len() == 1))
            .map_or(stmts.len(), |p| i + 1 + p);
        let block = &stmts[i..end];
        let this = block.iter().take_while(|t| t.start == s.start || !matches!(t.kind, Kind::Header { .. })).find_map(|t| match &t.kind {
            Kind::KeyVal { key } if key == &["name"] => {
                let text = lines[t.start..t.end].concat();
                text.parse::<toml::Table>().ok()?.get("name")?.as_str().map(str::to_owned)
            }
            _ => None,
        });
        if this.as_deref() == Some(name) {
            if found.is_some() {
                return Err(format!("two accounts are called {name}"));
            }
            found = Some(block);
        }
    }
    found.ok_or_else(|| format!("no account called {name} in config.toml"))
}

fn edit_account(lines: &[&str], stmts: &[Stmt], name: &str, path: &[&str], value: Option<&toml::Value>) -> Result<String, String> {
    let block = account_block(lines, stmts, name)?;
    // the block's own keys come before its first sub-table header
    let main_len = block.iter().skip(1).position(|s| matches!(s.kind, Kind::Header { .. })).map_or(block.len(), |p| p + 1);
    let main = &block[..main_len];
    let is_key = |s: &Stmt, k: &[&str]| matches!(&s.kind, Kind::KeyVal { key } if key.iter().map(String::as_str).eq(k.iter().copied()));
    let last_key_end = |part: &[Stmt]| part.iter().rev().find(|s| matches!(s.kind, Kind::KeyVal { .. })).map_or(part[0].end, |s| s.end);
    let set_or_remove = |s: &Stmt, key: &str| match value {
        Some(v) => join(lines, s.start, s.end, &replacement(lines, s, &line_for(key, v))),
        None => join(lines, s.start, s.end, ""),
    };

    if let [key] = path {
        return Ok(match (main.iter().find(|s| is_key(s, &[key])), value) {
            (Some(s), _) => set_or_remove(s, key),
            (None, None) => lines.concat(),
            (None, Some(v)) => {
                let at = last_key_end(main);
                join(lines, at, at, &(line_for(key, v) + "\n"))
            }
        });
    }
    let (table, key) = (path[0], path[1]);
    // `imap.host = ...`
    if let Some(s) = main.iter().find(|s| is_key(s, &[table, key])) {
        let text = lines[s.start..s.end].concat();
        return Ok(match value {
            Some(v) => join(lines, s.start, s.end, &replacement(lines, s, &format!("{} = {}", text.split('=').next().unwrap_or("").trim(), literal(v)))),
            None => join(lines, s.start, s.end, ""),
        });
    }
    // `imap = { host = ..., port = ... }`
    if let Some(s) = main.iter().find(|s| is_key(s, &[table])) {
        let text = lines[s.start..s.end].concat();
        let parsed: toml::Table = text.parse().map_err(|e| format!("{table}: {e}"))?;
        let mut inline = parsed.get(table).and_then(|t| t.as_table()).cloned().ok_or_else(|| format!("{table} is not a table"))?;
        // keep the order the keys were written in, new ones last
        let mut order: Vec<(usize, String)> = inline.keys().map(|k| (text.find(&format!("{k} ")).or_else(|| text.find(&format!("{k}="))).unwrap_or(usize::MAX), k.clone())).collect();
        match value {
            Some(v) => {
                if !inline.contains_key(key) {
                    order.push((usize::MAX, key.to_owned()));
                }
                inline.insert(key.to_owned(), v.clone());
            }
            None => {
                inline.remove(key);
            }
        }
        order.sort_by_key(|(p, _)| *p);
        let parts: Vec<String> = order.iter().filter_map(|(_, k)| inline.get(k).map(|v| format!("{} = {}", bare_key(k), literal(v)))).collect();
        let code = if parts.is_empty() { format!("{table} = {{}}") } else { format!("{table} = {{ {} }}", parts.join(", ")) };
        return Ok(join(lines, s.start, s.end, &replacement(lines, s, &code)));
    }
    // `[account.imap]`
    if let Some(h) = block.iter().position(|s| matches!(&s.kind, Kind::Header { array: false, name } if name.len() == 2 && name[1] == table)) {
        let end = block[h + 1..].iter().position(|s| matches!(s.kind, Kind::Header { .. })).map_or(block.len(), |p| h + 1 + p);
        let sub = &block[h..end];
        return Ok(match (sub.iter().find(|s| is_key(s, &[key])), value) {
            (Some(s), _) => set_or_remove(s, key),
            (None, None) => lines.concat(),
            (None, Some(v)) => {
                let at = last_key_end(sub);
                join(lines, at, at, &(line_for(key, v) + "\n"))
            }
        });
    }
    Ok(match value {
        None => lines.concat(),
        Some(v) => {
            let at = last_key_end(main);
            join(lines, at, at, &format!("{table} = {{ {} }}\n", line_for(key, v)))
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use toml::Value;

    const FILE: &str = r#"# my mail, edited by hand
theme = "eg"        # dark one
# font = "Inter"    # the UI font
font_size = 11  # bigger
notify = false
editor = "kitty -e nvim # not a comment"   # really a comment

# Personal, the main one
[[account]]
name = "Personal"
email = "me@x.ch"   # the address
imap = { host = "imap.x.ch", port = 993 }   # servers
smtp = { host = "smtp.x.ch", port = 465, security = "tls" }
password_cmd = "pass x"
signature = """
--
Me # still the signature
"""
# trailing note for Personal

[[account]] # work
name = "Work [old]"
email = "me@work.ch"
[account.imap]
host = "imap.work.ch"   # internal
port = 993
[account.smtp]
host = "smtp.work.ch"
port = 587
security = "starttls"

[oauth.google]
client_id = "x"
"#;

    fn s(v: &str) -> Value {
        Value::String(v.into())
    }

    fn cfg(text: &str) -> crate::config::Config {
        toml::from_str(text).unwrap()
    }

    #[test]
    fn top_level_keys_keep_their_comments() {
        let out = edit(FILE, Scope::Top, &["theme"], Some(&s("tokyo-night"))).unwrap();
        assert!(out.contains("theme = \"tokyo-night\"  # dark one\n"));
        let out = edit(FILE, Scope::Top, &["font_size"], Some(&Value::Float(12.5))).unwrap();
        assert!(out.contains("font_size = 12.5  # bigger\n"));
        let out = edit(FILE, Scope::Top, &["editor"], Some(&s("foot nvim"))).unwrap();
        assert!(out.contains("editor = \"foot nvim\"                       # really a comment\n"));
        // only that line changed
        let diff: Vec<_> = FILE.lines().zip(out.lines()).filter(|(a, b)| a != b).collect();
        assert_eq!(diff.len(), 1);
        assert_eq!(out.lines().count(), FILE.lines().count());
    }

    #[test]
    fn commented_example_comes_alive() {
        let out = edit(FILE, Scope::Top, &["font"], Some(&s("JetBrains Mono"))).unwrap();
        assert!(out.contains("\nfont = \"JetBrains Mono\"  # the UI font\n"));
        assert_eq!(cfg(&out).font, "JetBrains Mono");
        assert_eq!(out.lines().count(), FILE.lines().count());
    }

    #[test]
    fn new_top_level_key_goes_after_the_others() {
        let out = edit(FILE, Scope::Top, &["threads"], Some(&Value::Boolean(false))).unwrap();
        assert!(out.contains("   # really a comment\nthreads = false\n\n# Personal"));
        assert!(!cfg(&out).threads);
        let out = edit("[[account]]\nname = \"A\"\n", Scope::Top, &["html"], Some(&Value::Boolean(false))).unwrap();
        assert!(out.starts_with("html = false\n\n[[account]]"));
        let out = edit("", Scope::Top, &["theme"], Some(&s("eg"))).unwrap();
        assert_eq!(out, "theme = \"eg\"\n");
    }

    #[test]
    fn top_level_removal() {
        let out = edit(FILE, Scope::Top, &["notify"], None).unwrap();
        assert!(!out.contains("notify"));
        assert!(out.contains("# bigger\neditor"));
    }

    #[test]
    fn account_fields_in_place() {
        let out = edit(FILE, Scope::Account("Personal"), &["password_cmd"], Some(&s("pass y"))).unwrap();
        assert!(out.contains("password_cmd = \"pass y\"\nsignature"));
        let out = edit(FILE, Scope::Account("Work [old]"), &["email"], Some(&s("new@work.ch"))).unwrap();
        assert_eq!(cfg(&out).accounts[1].email, "new@work.ch");
        assert_eq!(cfg(&out).accounts[0].email, "me@x.ch");
    }

    #[test]
    fn multi_line_values_are_replaced_whole() {
        let out = edit(FILE, Scope::Account("Personal"), &["signature"], Some(&s("-- \nMe\nPhone 123"))).unwrap();
        assert!(out.contains("signature = \"-- \\nMe\\nPhone 123\"\n# trailing note for Personal\n"));
        assert_eq!(cfg(&out).accounts[0].signature, "-- \nMe\nPhone 123");
        let out = edit(&out, Scope::Account("Personal"), &["signature"], None).unwrap();
        assert!(out.contains("password_cmd = \"pass x\"\n# trailing note"));
    }

    #[test]
    fn new_account_field_goes_into_the_block() {
        let out = edit(FILE, Scope::Account("Personal"), &["color"], Some(&s("#f9e2af"))).unwrap();
        assert!(out.contains("\"\"\"\ncolor = \"#f9e2af\"\n# trailing note"));
        let out = edit(FILE, Scope::Account("Work [old]"), &["display_name"], Some(&s("Me \"Work\""))).unwrap();
        assert!(out.contains("email = \"me@work.ch\"\ndisplay_name = \"Me \\\"Work\\\"\"\n[account.imap]"));
        assert_eq!(cfg(&out).accounts[1].display_name, "Me \"Work\"");
    }

    #[test]
    fn inline_server_tables() {
        let out = edit(FILE, Scope::Account("Personal"), &["imap", "host"], Some(&s("mail.x.ch"))).unwrap();
        assert!(out.contains("imap = { host = \"mail.x.ch\", port = 993 }   # servers\n"));
        let out = edit(&out, Scope::Account("Personal"), &["imap", "security"], Some(&s("starttls"))).unwrap();
        assert!(out.contains("imap = { host = \"mail.x.ch\", port = 993, security = \"starttls\" }  # servers\n"));
        let out = edit(&out, Scope::Account("Personal"), &["smtp", "port"], Some(&Value::Integer(587))).unwrap();
        assert!(out.contains("smtp = { host = \"smtp.x.ch\", port = 587, security = \"tls\" }\n"));
        let c = cfg(&out);
        assert_eq!((c.accounts[0].imap.host.as_str(), c.accounts[0].smtp.port), ("mail.x.ch", 587));
    }

    #[test]
    fn server_sub_tables() {
        let out = edit(FILE, Scope::Account("Work [old]"), &["imap", "host"], Some(&s("imap2.work.ch"))).unwrap();
        assert!(out.contains("[account.imap]\nhost = \"imap2.work.ch\"  # internal\nport = 993\n"));
        let out = edit(&out, Scope::Account("Work [old]"), &["imap", "security"], Some(&s("starttls"))).unwrap();
        assert!(out.contains("port = 993\nsecurity = \"starttls\"\n[account.smtp]"));
        assert_eq!(cfg(&out).accounts[1].imap.security, crate::config::Security::Starttls);
    }

    #[test]
    fn renaming_keeps_the_block() {
        let out = edit(FILE, Scope::Account("Work [old]"), &["name"], Some(&s("Work"))).unwrap();
        assert!(out.contains("[[account]] # work\nname = \"Work\"\n"));
        assert_eq!(cfg(&out).accounts.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(), ["Personal", "Work"]);
    }

    #[test]
    fn unsafe_or_unknown_edits_are_refused() {
        assert!(edit(FILE, Scope::Account("Nobody"), &["color"], Some(&s("#fff"))).is_err());
        let twice = "[[account]]\nname = \"A\"\n[[account]]\nname = \"A\"\n";
        assert!(edit(twice, Scope::Account("A"), &["color"], Some(&s("#fff"))).is_err());
        assert!(edit("theme = \n", Scope::Top, &["theme"], Some(&s("eg"))).is_err());
        // dotted keys work too
        let dotted = "theme = \"eg\"\n[[account]]\nname = \"A\"\nimap.host = \"a\"\nimap.port = 1\n";
        let out = edit(dotted, Scope::Account("A"), &["imap", "host"], Some(&s("b"))).unwrap();
        assert!(out.contains("imap.host = \"b\"\nimap.port = 1\n"));
    }

    #[test]
    fn values_stay_on_one_line() {
        assert_eq!(literal(&s("a\"b\\c\nd\te")), r#""a\"b\\c\nd\te""#);
        assert_eq!(literal(&Value::Float(11.0)), "11.0");
        assert_eq!(literal(&Value::Integer(-1)), "-1");
        let mut t = toml::Table::new();
        t.insert("host".into(), s("x"));
        assert_eq!(literal(&Value::Table(t)), "{ host = \"x\" }");
    }

    #[test]
    fn crlf_files_stay_crlf() {
        let text = "theme = \"eg\"  # x\r\nnotify = true\r\n";
        let out = edit(text, Scope::Top, &["notify"], Some(&Value::Boolean(false))).unwrap();
        assert_eq!(out, "theme = \"eg\"  # x\r\nnotify = false\r\n");
    }
}
