//! Rich text for the composer: a small document model, its HTML and plain text forms, and an
//! importer for the HTML subset brief writes. No GTK here; `ui/rich.rs` maps it onto a buffer.

/// What a line is. Consecutive list and quote lines form one list or quote.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Block {
    #[default]
    Para,
    H1,
    H2,
    Quote,
    Bullet,
    Number,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Style {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
    pub link: Option<String>,
}

impl Style {
    pub fn is_plain(&self) -> bool {
        *self == Style::default()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Run {
    Text(String, Style),
    /// An inline image by content id.
    Image(String),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Line {
    pub block: Block,
    pub runs: Vec<Run>,
}

impl Line {
    pub fn new(block: Block) -> Line {
        Line { block, runs: Vec::new() }
    }

    /// Appends text, merged into the last run when the style matches.
    pub fn push(&mut self, text: &str, style: &Style) {
        if text.is_empty() {
            return;
        }
        if let Some(Run::Text(t, s)) = self.runs.last_mut()
            && s == style
        {
            t.push_str(text);
            return;
        }
        self.runs.push(Run::Text(text.to_owned(), style.clone()));
    }

    pub fn is_empty(&self) -> bool {
        self.runs.is_empty()
    }

    #[cfg(test)]
    pub fn text(&self) -> String {
        self.runs.iter().filter_map(|r| if let Run::Text(t, _) = r { Some(t.as_str()) } else { None }).collect()
    }
}

pub type Doc = Vec<Line>;

/// True when the plain text says it all: no inline formatting, links, images, lists or headings.
/// Quotes are fine, `> ` is how plain mail quotes anyway.
pub fn is_plain(doc: &Doc) -> bool {
    doc.iter().all(|l| {
        matches!(l.block, Block::Para | Block::Quote) && l.runs.iter().all(|r| matches!(r, Run::Text(_, s) if s.is_plain()))
    })
}

/// Formatting that a plain text editor cannot carry: everything but the line kinds.
pub fn has_inline(doc: &Doc) -> bool {
    doc.iter().flat_map(|l| &l.runs).any(|r| !matches!(r, Run::Text(_, s) if s.is_plain()))
}

/// The content ids of the images in use, in order.
pub fn images(doc: &Doc) -> Vec<&str> {
    doc.iter().flat_map(|l| &l.runs).filter_map(|r| if let Run::Image(c) = r { Some(c.as_str()) } else { None }).collect()
}

/// Drops empty paragraphs at the end.
fn trimmed(doc: &Doc) -> &[Line] {
    let n = doc.iter().rposition(|l| !(l.is_empty() && l.block == Block::Para)).map_or(0, |i| i + 1);
    &doc[..n]
}

/// A link target that is safe to send: http(s), mailto and tel. A bare domain gets https://, a bare
/// address mailto:. Anything else (javascript:, data:, file:...) is refused.
pub fn safe_url(url: &str) -> Option<String> {
    let url = url.trim();
    if url.is_empty() || url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return None;
    }
    let lower = url.to_ascii_lowercase();
    if let Some(colon) = url.find(':')
        && !url[..colon].contains(['/', '.', '@'])
    {
        let scheme = &lower[..colon];
        let ok = matches!(scheme, "http" | "https") && lower[colon..].starts_with("://") && url.len() > colon + 3;
        return (ok || (matches!(scheme, "mailto" | "tel") && url.len() > colon + 1)).then(|| url.to_owned());
    }
    if url.contains('@') && !url.contains('/') {
        return Some(format!("mailto:{url}"));
    }
    if url.contains('.') && !url.starts_with(['.', '/']) {
        return Some(format!("https://{url}"));
    }
    None
}

// ── plain text ───────────────────────────────────────────

/// The text form. `markdown` writes headings as `# ` (for the round trip through $EDITOR) and
/// leaves bold and italic unmarked; the mail form marks them `*bold*` and `_italic_`.
pub fn to_plain(doc: &Doc, markdown: bool) -> String {
    let mut out = String::new();
    let mut number = 0;
    for line in trimmed(doc) {
        number = if line.block == Block::Number { number + 1 } else { 0 };
        let mut text = String::new();
        for run in &line.runs {
            match run {
                Run::Image(_) => text += "[image]",
                Run::Text(t, s) => {
                    let mark = if markdown { "" } else if s.bold { "*" } else if s.italic { "_" } else { "" };
                    let shown = format!("{mark}{t}{mark}");
                    match &s.link {
                        Some(url) if url != t && url.strip_prefix("mailto:") != Some(t) => text += &format!("{shown} <{url}>"),
                        _ => text += &shown,
                    }
                }
            }
        }
        match line.block {
            Block::Para => {}
            Block::H1 if markdown => out += "# ",
            Block::H2 if markdown => out += "## ",
            Block::H1 | Block::H2 => {}
            Block::Quote => out += if text.is_empty() || text.starts_with('>') { ">" } else { "> " },
            Block::Bullet => out += "- ",
            Block::Number => out += &format!("{number}. "),
        }
        out += &text;
        out.push('\n');
    }
    out
}

/// Text into lines; `>` lines become quotes (one level taken off). With `markdown` also
/// `- `/`* ` bullets, `1. ` numbers and `# `/`## ` headings.
pub fn from_plain(text: &str, markdown: bool) -> Doc {
    let plain = Style::default();
    text.trim_end_matches('\n')
        .split('\n')
        .map(|l| {
            let l = l.strip_suffix('\r').unwrap_or(l);
            let (block, rest) = if let Some(r) = l.strip_prefix('>') {
                (Block::Quote, r.strip_prefix(' ').unwrap_or(r))
            } else if !markdown {
                (Block::Para, l)
            } else if let Some(r) = l.strip_prefix("- ").or_else(|| l.strip_prefix("* ")) {
                (Block::Bullet, r)
            } else if let Some(r) = l.strip_prefix("## ") {
                (Block::H2, r)
            } else if let Some(r) = l.strip_prefix("# ") {
                (Block::H1, r)
            } else if let Some((n, r)) = l.split_once(". ")
                && !n.is_empty()
                && n.len() < 4
                && n.chars().all(|c| c.is_ascii_digit())
            {
                (Block::Number, r)
            } else {
                (Block::Para, l)
            };
            let mut line = Line::new(block);
            line.push(rest, &plain);
            line
        })
        .collect()
}

// ── HTML ─────────────────────────────────────────────────

pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out += "&amp;",
            '<' => out += "&lt;",
            '>' => out += "&gt;",
            '"' => out += "&quot;",
            '\'' => out += "&#39;",
            _ => out.push(c),
        }
    }
    out
}

/// Escaped text with runs of spaces and tabs kept.
fn escape_text(s: &str, line_start: bool) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_space = line_start;
    for c in s.chars() {
        match c {
            ' ' if prev_space => out += "&nbsp;",
            '\t' => out += "&nbsp;&nbsp;&nbsp;&nbsp;",
            _ => out += &escape(c.encode_utf8(&mut [0; 4])),
        }
        prev_space = c == ' ' || c == '\t';
    }
    out
}

const QUOTE_STYLE: &str = "margin:0 0 0 .8ex;border-left:2px solid #ccc;padding-left:1ex;color:#555";
const LIST_STYLE: &str = "margin:0 0 0 1.6em;padding:0";
const LINK_STYLE: &str = "color:#1a73e8;text-decoration:underline";

fn inline_html(line: &Line) -> String {
    let mut out = String::new();
    for run in &line.runs {
        match run {
            Run::Image(cid) => out += &format!("<img src=\"cid:{}\" alt=\"\" style=\"max-width:100%;height:auto\">", escape(cid)),
            Run::Text(t, s) => {
                let mut open = String::new();
                let mut close = String::new();
                if let Some(url) = s.link.as_deref().and_then(safe_url) {
                    open += &format!("<a href=\"{}\" style=\"{LINK_STYLE}\">", escape(&url));
                    close.insert_str(0, "</a>");
                }
                for (on, tag) in [(s.bold, "b"), (s.italic, "i"), (s.underline, "u"), (s.strike, "s")] {
                    if on {
                        open += &format!("<{tag}>");
                        close.insert_str(0, &format!("</{tag}>"));
                    }
                }
                out += &open;
                out += &escape_text(t, out.is_empty());
                out += &close;
            }
        }
    }
    if out.is_empty() { "<br>".into() } else { out }
}

/// Small inline-styled HTML that mail clients agree on: a div per line, `ul`/`ol`/`blockquote`
/// around runs of list and quote lines, `cid:` images. No classes, no external CSS.
pub fn to_html(doc: &Doc) -> String {
    let mut body = String::new();
    let mut open: Option<Block> = None;
    for line in trimmed(doc) {
        let group = matches!(line.block, Block::Quote | Block::Bullet | Block::Number).then_some(line.block);
        if open != group {
            match open {
                Some(Block::Quote) => body += "</blockquote>",
                Some(Block::Bullet) => body += "</ul>",
                Some(Block::Number) => body += "</ol>",
                _ => {}
            }
            match group {
                Some(Block::Quote) => body += &format!("<blockquote style=\"{QUOTE_STYLE}\">"),
                Some(Block::Bullet) => body += &format!("<ul style=\"{LIST_STYLE}\">"),
                Some(Block::Number) => body += &format!("<ol style=\"{LIST_STYLE}\">"),
                _ => {}
            }
            open = group;
        }
        let inner = inline_html(line);
        body += &match line.block {
            Block::Para | Block::Quote => format!("<div>{inner}</div>"),
            Block::H1 => format!("<h1 style=\"font-size:1.5em;margin:.4em 0 .2em\">{inner}</h1>"),
            Block::H2 => format!("<h2 style=\"font-size:1.25em;margin:.4em 0 .2em\">{inner}</h2>"),
            Block::Bullet | Block::Number => format!("<li>{inner}</li>"),
        };
    }
    match open {
        Some(Block::Quote) => body += "</blockquote>",
        Some(Block::Bullet) => body += "</ul>",
        Some(Block::Number) => body += "</ol>",
        _ => {}
    }
    format!(
        "<!DOCTYPE html><html><head><meta charset=\"utf-8\"><meta name=\"generator\" content=\"brief\"></head>\
         <body><div style=\"font-family:sans-serif;font-size:14px;line-height:1.45\">{body}</div></body></html>\n"
    )
}

/// Whether brief wrote this HTML, so `from_html` can read it back without losing anything.
pub fn is_ours(html: &str) -> bool {
    html.contains("<meta name=\"generator\" content=\"brief\">")
}

fn decode_entities(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out += &rest[..i];
        rest = &rest[i..];
        let end = rest.find(';').filter(|&e| e < 12);
        let ent = end.map(|e| &rest[1..e]);
        let c = match ent {
            Some("amp") => Some('&'),
            Some("lt") => Some('<'),
            Some("gt") => Some('>'),
            Some("quot") => Some('"'),
            Some("apos") => Some('\''),
            Some("nbsp") => Some(' '),
            Some(n) if n.starts_with("#x") || n.starts_with("#X") => u32::from_str_radix(&n[2..], 16).ok().and_then(char::from_u32),
            Some(n) if n.starts_with('#') => n[1..].parse().ok().and_then(char::from_u32),
            _ => None,
        };
        match (c, end) {
            (Some(c), Some(e)) => {
                out.push(if c == '\u{a0}' { ' ' } else { c });
                rest = &rest[e + 1..];
            }
            _ => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out + rest
}

/// The value of `name="..."` in a tag's attribute text.
fn attr(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let mut from = 0;
    while let Some(i) = lower[from..].find(name).map(|i| i + from) {
        from = i + name.len();
        let before_ok = i == 0 || lower.as_bytes()[i - 1].is_ascii_whitespace();
        let after = lower[from..].trim_start();
        if !before_ok || !after.starts_with('=') {
            continue;
        }
        let v = tag[tag.len() - after.len() + 1..].trim_start();
        let (q, v) = match v.chars().next() {
            Some(q @ ('"' | '\'')) => (Some(q), &v[1..]),
            _ => (None, v),
        };
        let end = match q {
            Some(q) => v.find(q).unwrap_or(v.len()),
            None => v.find(|c: char| c.is_whitespace() || c == '>').unwrap_or(v.len()),
        };
        return Some(decode_entities(&v[..end]));
    }
    None
}

/// Reads back the HTML that `to_html` writes (and simple HTML like it). Unknown tags are dropped,
/// their text kept; head, style and script are skipped.
pub fn from_html(html: &str) -> Doc {
    #[derive(Default)]
    struct State {
        doc: Doc,
        cur: Option<Line>,
        style: Style,
        counts: [u32; 4],
        links: Vec<Option<String>>,
        quote: u32,
        lists: Vec<Block>,
        heading: Option<Block>,
    }
    impl State {
        fn block(&self) -> Block {
            if let Some(h) = self.heading {
                h
            } else if let Some(l) = self.lists.last() {
                *l
            } else if self.quote > 0 {
                Block::Quote
            } else {
                Block::Para
            }
        }
        fn line(&mut self) -> &mut Line {
            let b = self.block();
            self.cur.get_or_insert_with(|| Line::new(b))
        }
        /// Ends the open line; with `force` an empty line counts too (`<br>`).
        fn flush(&mut self, force: bool) {
            match self.cur.take() {
                Some(l) if force || !l.is_empty() => self.doc.push(l),
                None if force => self.doc.push(Line::new(self.block())),
                _ => {}
            }
        }
        fn restyle(&mut self) {
            let [b, i, u, s] = self.counts.map(|n| n > 0);
            self.style = Style { bold: b, italic: i, underline: u, strike: s, link: self.links.iter().rev().flatten().next().cloned() };
        }
    }
    let mut st = State::default();
    let mut rest = html;
    let mut skip: Option<String> = None;
    while !rest.is_empty() {
        let (text, tag) = match rest.find('<') {
            Some(0) => {
                if rest.starts_with("<!--") {
                    rest = rest.find("-->").map_or("", |e| &rest[e + 3..]);
                    continue;
                }
                let Some(end) = rest.find('>') else { break };
                let tag = &rest[1..end];
                rest = &rest[end + 1..];
                ("", Some(tag))
            }
            Some(i) => {
                let t = &rest[..i];
                rest = &rest[i..];
                (t, None)
            }
            None => {
                let t = rest;
                rest = "";
                (t, None)
            }
        };
        if let Some(tag) = tag {
            let tag = tag.trim_end_matches('/').trim();
            let closing = tag.starts_with('/');
            let name: String = tag.trim_start_matches('/').chars().take_while(|c| c.is_ascii_alphanumeric()).collect::<String>().to_ascii_lowercase();
            if let Some(s) = &skip {
                if closing && &name == s {
                    skip = None;
                }
                continue;
            }
            match (name.as_str(), closing) {
                ("head" | "style" | "script" | "title", false) => skip = Some(name.clone()),
                ("br", _) => st.flush(true),
                ("div" | "p" | "li" | "h1" | "h2" | "h3" | "h4" | "blockquote" | "ul" | "ol" | "table" | "tr", _) => {
                    st.flush(false);
                    match (name.as_str(), closing) {
                        ("blockquote", false) => st.quote += 1,
                        ("blockquote", true) => st.quote = st.quote.saturating_sub(1),
                        ("ul", false) => st.lists.push(Block::Bullet),
                        ("ol", false) => st.lists.push(Block::Number),
                        ("ul" | "ol", true) => {
                            st.lists.pop();
                        }
                        ("h1", false) => st.heading = Some(Block::H1),
                        ("h2" | "h3" | "h4", false) => st.heading = Some(Block::H2),
                        ("h1" | "h2" | "h3" | "h4", true) => st.heading = None,
                        _ => {}
                    }
                }
                ("b" | "strong" | "i" | "em" | "u" | "s" | "strike" | "del", _) => {
                    let k = match name.as_str() {
                        "b" | "strong" => 0,
                        "i" | "em" => 1,
                        "u" => 2,
                        _ => 3,
                    };
                    st.counts[k] = if closing { st.counts[k].saturating_sub(1) } else { st.counts[k] + 1 };
                    st.restyle();
                }
                ("a", false) => {
                    st.links.push(attr(tag, "href").and_then(|u| safe_url(&u)));
                    st.restyle();
                }
                ("a", true) => {
                    st.links.pop();
                    st.restyle();
                }
                ("img", false) => {
                    if let Some(cid) = attr(tag, "src").and_then(|s| s.strip_prefix("cid:").map(str::to_owned)) {
                        st.line().runs.push(Run::Image(cid));
                    }
                }
                _ => {}
            }
            continue;
        }
        if skip.is_some() {
            continue;
        }
        let mut t = String::with_capacity(text.len());
        for c in text.chars() {
            if !c.is_ascii_whitespace() {
                t.push(c);
            } else if !t.ends_with(' ') {
                t.push(' ');
            }
        }
        let t = if st.cur.as_ref().is_none_or(Line::is_empty) { t.trim_start() } else { &t };
        if t.is_empty() {
            continue;
        }
        let t = decode_entities(t);
        let style = st.style.clone();
        st.line().push(&t, &style);
    }
    st.flush(false);
    st.doc
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s() -> Style {
        Style::default()
    }

    fn line(block: Block, runs: &[(&str, Style)]) -> Line {
        let mut l = Line::new(block);
        for (t, st) in runs {
            l.push(t, st);
        }
        l
    }

    fn sample() -> Doc {
        let bold = Style { bold: true, ..s() };
        let bi = Style { bold: true, italic: true, ..s() };
        let link = Style { link: Some("https://x.ch/?a=1&b=\"2\"".into()), ..s() };
        let mut img = Line::new(Block::Para);
        img.runs.push(Run::Image("img1.abc@brief".into()));
        vec![
            line(Block::H1, &[("Title", s())]),
            line(Block::Para, &[("Hi ", s()), ("bold", bold), (" and ", s()), ("both", bi), (" <tag> & co", s())]),
            Line::new(Block::Para),
            line(Block::Bullet, &[("one", s())]),
            line(Block::Bullet, &[("two  spaced", Style { strike: true, underline: true, ..s() })]),
            line(Block::Number, &[("first", s())]),
            line(Block::Number, &[("see ", s()), ("the site", link)]),
            line(Block::Quote, &[("quoted", s())]),
            line(Block::Quote, &[]),
            line(Block::Quote, &[("> older", s())]),
            line(Block::H2, &[("Sub", s())]),
            img,
            line(Block::Para, &[("Grüezi 👋", s())]),
        ]
    }

    #[test]
    fn html_round_trip() {
        let doc = sample();
        let html = to_html(&doc);
        assert!(is_ours(&html));
        assert_eq!(from_html(&html), doc);
    }

    #[test]
    fn html_escapes_and_structure() {
        let html = to_html(&sample());
        assert!(html.contains("&lt;tag&gt; &amp; co"));
        assert!(html.contains("href=\"https://x.ch/?a=1&amp;b=&quot;2&quot;\""));
        assert!(html.contains("<ul style=\"margin:0 0 0 1.6em;padding:0\"><li>one</li>"));
        assert!(html.contains("<li><u><s>two &nbsp;spaced</s></u></li></ul><ol"));
        assert!(html.contains("<b><i>both</i></b>"));
        assert!(html.contains("<blockquote style=\""));
        assert!(html.contains("<div><br></div>"));
        assert!(html.contains("<img src=\"cid:img1.abc@brief\""));
        assert!(!html.contains("class="));
        assert_eq!(images(&sample()), ["img1.abc@brief"]);
    }

    #[test]
    fn plain_form() {
        let text = to_plain(&sample(), false);
        assert_eq!(
            text,
            "Title\nHi *bold* and *both* <tag> & co\n\n- one\n- two  spaced\n1. first\n2. see the site <https://x.ch/?a=1&b=\"2\">\n\
             > quoted\n>\n>> older\nSub\n[image]\nGrüezi 👋\n"
        );
        let md = to_plain(&sample(), true);
        assert!(md.starts_with("# Title\nHi bold and both"));
        assert!(md.contains("\n## Sub\n"));
    }

    #[test]
    fn plain_round_trip_of_blocks() {
        let doc: Doc = sample()
            .into_iter()
            .map(|l| {
                let t = l.text();
                line(l.block, &[(&t, s())])
            })
            .filter(|l| !l.is_empty() || l.block != Block::Para)
            .collect();
        assert_eq!(from_plain(&to_plain(&doc, true), true), doc);
    }

    #[test]
    fn plain_reply_quotes() {
        let doc = from_plain("Hi\n\nOn x, Lea wrote:\n> Hallo\n>> old\n- not a list\n", false);
        assert_eq!(doc[3], line(Block::Quote, &[("Hallo", s())]));
        assert_eq!(doc[4], line(Block::Quote, &[("> old", s())]));
        assert_eq!(doc[5].block, Block::Para);
        assert!(is_plain(&doc));
        assert_eq!(to_plain(&doc, false), "Hi\n\nOn x, Lea wrote:\n> Hallo\n>> old\n- not a list\n");
    }

    #[test]
    fn plain_detection() {
        assert!(!is_plain(&sample()));
        assert!(has_inline(&sample()));
        let lists = vec![line(Block::Bullet, &[("x", s())])];
        assert!(!is_plain(&lists) && !has_inline(&lists));
    }

    #[test]
    fn urls() {
        assert_eq!(safe_url("https://a.ch/x").as_deref(), Some("https://a.ch/x"));
        assert_eq!(safe_url("a.ch").as_deref(), Some("https://a.ch"));
        assert_eq!(safe_url("me@a.ch").as_deref(), Some("mailto:me@a.ch"));
        assert_eq!(safe_url("mailto:me@a.ch").as_deref(), Some("mailto:me@a.ch"));
        assert_eq!(safe_url("localhost:8080/x").as_deref(), None);
        for bad in ["javascript:alert(1)", "JavaScript:alert(1)", " javascript:x", "java\tscript:x", "data:text/html,x", "file:///etc/passwd", "vbscript:x", "http:/x", "", "a b.ch"] {
            assert_eq!(safe_url(bad), None, "{bad}");
        }
    }

    #[test]
    fn import_drops_unsafe_and_unknown() {
        let doc = from_html("<html><head><style>p{}</style></head><body><p>a <a href=\"javascript:alert(1)\">x</a> <span>b</span></p><script>evil()</script><p>c&nbsp;&#246;&#x1F600;</p></body></html>");
        assert_eq!(doc.len(), 2);
        assert_eq!(doc[0], line(Block::Para, &[("a x b", s())]));
        assert_eq!(doc[1].text(), "c ö😀");
    }

    #[test]
    fn import_gmail_style() {
        let doc = from_html("<div dir=\"ltr\">Hi<div><br></div><div>line <b>two</b></div><br><ul><li>a</li></ul></div>");
        assert_eq!(doc[0].text(), "Hi");
        assert!(doc[1].is_empty());
        assert_eq!(doc[2].runs[1], Run::Text("two".into(), Style { bold: true, ..s() }));
        assert_eq!(doc.last().unwrap().block, Block::Bullet);
    }
}
