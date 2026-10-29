//! `mailto:` URLs (RFC 6068) into a draft for the composer.

use crate::smtp::Draft;

/// True for anything that starts with the `mailto:` scheme, in any case.
pub fn is_mailto(s: &str) -> bool {
    s.get(..7).is_some_and(|p| p.eq_ignore_ascii_case("mailto:"))
}

/// Percent-decodes as UTF-8; broken sequences stay as they are. `+` is not a space here.
fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let hex = |i: usize| b.get(i).and_then(|c| (*c as char).to_digit(16));
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && let (Some(h), Some(l)) = (hex(i + 1), hex(i + 2))
        {
            out.push((h * 16 + l) as u8);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Appends comma-separated addresses to a recipient field.
fn add(field: &mut String, list: &str) {
    for a in list.split(',').map(str::trim).filter(|a| !a.is_empty()) {
        if !field.is_empty() {
            field.push_str(", ");
        }
        field.push_str(a);
    }
}

/// Parses a `mailto:` URL. Recipients from the path and from `to=` add up, as do `cc` and `bcc`;
/// the first `subject`, `body` and `in-reply-to` win. Unknown headers are dropped.
pub fn parse(url: &str) -> Option<Draft> {
    if !is_mailto(url) {
        return None;
    }
    let rest = url[7..].trim();
    let rest = rest.strip_prefix("//").unwrap_or(rest);
    let rest = rest.split_once('#').map_or(rest, |(r, _)| r);
    let (to, query) = rest.split_once('?').unwrap_or((rest, ""));
    let mut d = Draft::default();
    for a in to.split(',') {
        add(&mut d.to, &decode(a));
    }
    let mut subject = None;
    let mut body = None;
    let mut reply = None;
    for pair in query.split('&').filter(|p| !p.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        let v = decode(v);
        match decode(k).to_ascii_lowercase().as_str() {
            "to" => add(&mut d.to, &v),
            "cc" => add(&mut d.cc, &v),
            "bcc" => add(&mut d.bcc, &v),
            "subject" => {
                subject.get_or_insert(v);
            }
            "body" => {
                body.get_or_insert(v);
            }
            "in-reply-to" => {
                reply.get_or_insert(v);
            }
            _ => {}
        }
    }
    // one-line headers; the body keeps its breaks, as plain \n
    d.subject = subject.unwrap_or_default().replace(['\r', '\n'], " ").trim().to_owned();
    d.body = body.unwrap_or_default().replace("\r\n", "\n").replace('\r', "\n");
    d.in_reply_to = reply.unwrap_or_default().replace(['\r', '\n'], "").trim().to_owned();
    Some(d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_address() {
        let d = parse("mailto:lea@example.com").unwrap();
        assert_eq!(d.to, "lea@example.com");
        assert!(d.subject.is_empty() && d.body.is_empty() && d.cc.is_empty());
    }

    #[test]
    fn scheme_and_header_names_ignore_case() {
        let d = parse("MAILTO:a@x.ch?SUBJECT=Hi&Cc=b@x.ch&bCC=c@x.ch").unwrap();
        assert_eq!(d.to, "a@x.ch");
        assert_eq!(d.subject, "Hi");
        assert_eq!(d.cc, "b@x.ch");
        assert_eq!(d.bcc, "c@x.ch");
    }

    #[test]
    fn several_recipients_add_up() {
        let d = parse("mailto:a@x.ch,b@x.ch?to=c@x.ch&cc=d@x.ch,e@x.ch&cc=f@x.ch").unwrap();
        assert_eq!(d.to, "a@x.ch, b@x.ch, c@x.ch");
        assert_eq!(d.cc, "d@x.ch, e@x.ch, f@x.ch");
    }

    #[test]
    fn percent_decoding_is_utf8() {
        let d = parse("mailto:%22J%C3%BCrg%22%20%3Cj@x.ch%3E?subject=Gr%C3%BCezi%20%E2%9C%8C&body=a%0D%0Ab%0Ac+d").unwrap();
        assert_eq!(d.to, "\"Jürg\" <j@x.ch>");
        assert_eq!(d.subject, "Grüezi ✌");
        assert_eq!(d.body, "a\nb\nc+d");
    }

    #[test]
    fn encoded_separators_stay_in_values() {
        let d = parse("mailto:a@x.ch?subject=x%26y%3Dz%3F&body=50%25").unwrap();
        assert_eq!(d.subject, "x&y=z?");
        assert_eq!(d.body, "50%");
    }

    #[test]
    fn no_recipient_and_unknown_headers() {
        let d = parse("mailto:?subject=hello&x-foo=bar&keywords=k&body=").unwrap();
        assert!(d.to.is_empty());
        assert_eq!(d.subject, "hello");
        assert!(d.body.is_empty());
    }

    #[test]
    fn in_reply_to_and_first_subject_win() {
        let d = parse("mailto:list@x.ch?In-Reply-To=%3C3469A91.D10AF4C@example.com%3E&subject=one&subject=two").unwrap();
        assert_eq!(d.in_reply_to, "<3469A91.D10AF4C@example.com>");
        assert_eq!(d.subject, "one");
    }

    #[test]
    fn header_injection_is_flattened() {
        let d = parse("mailto:a@x.ch?subject=hi%0D%0ABcc:%20evil@x.ch").unwrap();
        assert_eq!(d.subject, "hi  Bcc: evil@x.ch");
        assert!(d.bcc.is_empty());
    }

    #[test]
    fn broken_escapes_and_other_schemes() {
        assert_eq!(parse("mailto:a@x.ch?subject=100%&body=%zz%4").unwrap().subject, "100%");
        assert_eq!(parse("mailto:a@x.ch?body=%zz%4").unwrap().body, "%zz%4");
        assert!(parse("https://x.ch").is_none());
        assert!(parse("mail").is_none());
    }
}
