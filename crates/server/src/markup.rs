//! Parses the tutor's tagged reply into things to speak and links to show.
//!
//! The tutor is told to answer only with
//! `<say lang="en">…</say>`, `<say lang="ru">…</say>` and `<link url="…">label</link>`.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    En,
    Ru,
}

impl Lang {
    pub fn code(self) -> &'static str {
        match self {
            Lang::En => "en",
            Lang::Ru => "ru",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Segment {
    Say { lang: Lang, text: String },
    Link { url: String, label: String },
}

/// Splits a reply into segments. Text without any tag is treated as English speech,
/// so a tutor that forgets the markup still produces something audible.
pub fn parse_reply(raw: &str) -> Vec<Segment> {
    let mut out = Vec::new();
    let mut rest = raw;
    let mut saw_tag = false;
    while let Some(start) = rest.find('<') {
        let after = &rest[start..];
        let parsed = if after.starts_with("<say") {
            parse_say(after)
        } else if after.starts_with("<link") {
            parse_link(after)
        } else {
            None
        };
        match parsed {
            Some((segment, consumed)) => {
                saw_tag = true;
                if let Some(seg) = segment {
                    out.push(seg);
                }
                rest = &after[consumed..];
            }
            None => rest = &after[1..],
        }
    }
    if !saw_tag {
        let text = raw.trim();
        if !text.is_empty() {
            out.push(Segment::Say { lang: Lang::En, text: text.to_string() });
        }
    }
    out
}

fn attr<'a>(tag_open: &'a str, name: &str) -> Option<&'a str> {
    let key = format!("{name}=\"");
    let from = tag_open.find(&key)? + key.len();
    let len = tag_open[from..].find('"')?;
    Some(&tag_open[from..from + len])
}

/// Returns the parsed segment (None when empty) and the number of bytes consumed.
fn parse_say(s: &str) -> Option<(Option<Segment>, usize)> {
    let open_end = s.find('>')?;
    let lang = match attr(&s[..open_end], "lang") {
        Some("ru") => Lang::Ru,
        _ => Lang::En,
    };
    let body_start = open_end + 1;
    // A reply cut off before `</say>` still speaks what was said.
    let (text, consumed) = match s[body_start..].find("</say>") {
        Some(close) => (s[body_start..body_start + close].trim(), body_start + close + "</say>".len()),
        None => (s[body_start..].trim(), s.len()),
    };
    let seg = (!text.is_empty()).then(|| Segment::Say { lang, text: text.to_string() });
    Some((seg, consumed))
}

fn parse_link(s: &str) -> Option<(Option<Segment>, usize)> {
    let open_end = s.find('>')?;
    let url = attr(&s[..open_end], "url")?.to_string();
    let body_start = open_end + 1;
    let close = s[body_start..].find("</link>")?;
    let label = s[body_start..body_start + close].trim().to_string();
    let consumed = body_start + close + "</link>".len();
    Some((Some(Segment::Link { url, label }), consumed))
}

/// Plain text of the spoken parts, for display.
pub fn display_text(segments: &[Segment]) -> String {
    segments
        .iter()
        .filter_map(|s| match s {
            Segment::Say { text, .. } => Some(text.as_str()),
            Segment::Link { .. } => None,
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn say(lang: Lang, text: &str) -> Segment {
        Segment::Say { lang, text: text.into() }
    }

    #[test]
    fn parses_mixed_languages_and_link() {
        let raw = "<say lang=\"en\">Tell me about your work.</say>\n<say lang=\"ru\">Скажи, что работаешь круглосуточно.</say>\n<link url=\"https://youtu.be/x?t=5\">x @ 0:05</link>";
        assert_eq!(
            parse_reply(raw),
            vec![
                say(Lang::En, "Tell me about your work."),
                say(Lang::Ru, "Скажи, что работаешь круглосуточно."),
                Segment::Link { url: "https://youtu.be/x?t=5".into(), label: "x @ 0:05".into() },
            ]
        );
    }

    #[test]
    fn untagged_text_is_spoken_as_english() {
        assert_eq!(parse_reply("  Hello there!  "), vec![say(Lang::En, "Hello there!")]);
        assert!(parse_reply("   ").is_empty());
    }

    #[test]
    fn stray_angle_brackets_and_unclosed_tags_do_not_panic() {
        assert_eq!(parse_reply("1 < 2 and 3 > 2"), vec![say(Lang::En, "1 < 2 and 3 > 2")]);
        assert_eq!(parse_reply("<say lang=\"en\">cut off mid"), vec![say(Lang::En, "cut off mid")]);
        assert!(parse_reply("<say lang=\"en\"></say>").is_empty());
    }

    #[test]
    fn display_text_skips_links() {
        let segs = parse_reply("<say lang=\"en\">One.</say><link url=\"u\">l</link><say lang=\"ru\">Два.</say>");
        assert_eq!(display_text(&segs), "One. Два.");
    }
}
