//! Parses the tutor's tagged reply into things to speak and links to show.
//!
//! The tutor is told to answer only with
//! `<say lang="en">…</say>`, `<say lang="ru">…</say>`, `<link url="…">label</link>`,
//! `<watch url="…">title</watch>` (send the learner to watch a whole video), `<done dialog="name"/>` (a dialog was finished) and the bare `<continue/>`, which ends a feedback turn: the lesson goes on by
//! itself unless the learner speaks within a short window.

use crate::phrasebook::Missed;

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
    /// "Watch this video now, the microphone stays closed until you are ready."
    Watch { url: String, label: String },
    /// "The learner finished this dialog" (remembered between sessions).
    Done { dialog: String },
    /// "The learner did not manage this phrase": goes into the phrasebook.
    Missed(Missed),
    /// "The learner managed this phrase of the phrasebook."
    Passed { phrase: String },
    /// "Go on by yourself if the learner stays silent."
    Continue,
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
        } else if after.starts_with("<watch") {
            parse_watch(after)
        } else if after.starts_with("<missed") {
            parse_missed(after)
        } else if after.starts_with("<passed") {
            parse_passed(after)
        } else if after.starts_with("<done") {
            parse_done(after)
        } else if after.starts_with("<continue") {
            parse_continue(after)
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

/// `<tag url="…">label</tag>`: returns the url, the label and the bytes consumed.
fn parse_url_tag(s: &str, close_tag: &str) -> Option<(String, String, usize)> {
    let open_end = s.find('>')?;
    let url = attr(&s[..open_end], "url")?.to_string();
    let body_start = open_end + 1;
    let close = s[body_start..].find(close_tag)?;
    let label = s[body_start..body_start + close].trim().to_string();
    Some((url, label, body_start + close + close_tag.len()))
}

fn parse_link(s: &str) -> Option<(Option<Segment>, usize)> {
    let (url, label, consumed) = parse_url_tag(s, "</link>")?;
    Some((Some(Segment::Link { url, label }), consumed))
}

fn parse_watch(s: &str) -> Option<(Option<Segment>, usize)> {
    let (url, label, consumed) = parse_url_tag(s, "</watch>")?;
    Some((Some(Segment::Watch { url, label }), consumed))
}

/// End of a self-closing tag: `/>`, or a plain `>` from a sloppy writer.
fn tag_end(s: &str) -> Option<(usize, usize)> {
    match (s.find("/>"), s.find('>')) {
        (Some(slash), Some(gt)) if slash + 1 == gt => Some((slash, gt + 1)),
        (_, Some(gt)) => Some((gt, gt + 1)),
        _ => None,
    }
}

fn parse_missed(s: &str) -> Option<(Option<Segment>, usize)> {
    let (inner_end, consumed) = tag_end(s)?;
    let open = &s[..inner_end];
    let get = |name: &str| attr(open, name).unwrap_or("").trim().to_string();
    let phrase = get("phrase");
    let seg = (!phrase.is_empty())
        .then(|| Segment::Missed(Missed { phrase, hint_ru: get("hint"), model_answer: get("answer"), source: get("source"), url: get("url") }));
    Some((seg, consumed))
}

fn parse_passed(s: &str) -> Option<(Option<Segment>, usize)> {
    let (inner_end, consumed) = tag_end(s)?;
    let phrase = attr(&s[..inner_end], "phrase").unwrap_or("").trim().to_string();
    Some(((!phrase.is_empty()).then_some(Segment::Passed { phrase }), consumed))
}

fn parse_done(s: &str) -> Option<(Option<Segment>, usize)> {
    let end = s.find('>')?;
    let dialog = attr(&s[..end], "dialog").map(|d| d.trim().trim_end_matches(".md").to_string()).filter(|d| !d.is_empty());
    Some((dialog.map(|dialog| Segment::Done { dialog }), end + 1))
}

fn parse_continue(s: &str) -> Option<(Option<Segment>, usize)> {
    let end = s.find('>')?;
    Some((Some(Segment::Continue), end + 1))
}

fn script_of(c: char) -> Option<Lang> {
    if ('\u{400}'..='\u{4FF}').contains(&c) {
        Some(Lang::Ru)
    } else if c.is_alphabetic() {
        Some(Lang::En)
    } else {
        None
    }
}

/// Splits text into runs that are each read by the right voice, going by the
/// alphabet of the text itself and ignoring what the tutor wrote in the `lang`
/// attribute: Cyrillic letters are read by the Russian voice, Latin letters by
/// the English one. A model sometimes puts Russian words into an English tag (or
/// the other way round) and the English voice cannot read Cyrillic.
///
/// Digits, spaces and punctuation stay with the run they follow. Text with no
/// letters at all (for example "25") is read by the `fallback` voice, and text
/// with nothing readable is dropped.
pub fn split_by_script(fallback: Lang, text: &str) -> Vec<(Lang, String)> {
    let mut runs: Vec<(Lang, String)> = Vec::new();
    let mut leading = String::new();
    for c in text.chars() {
        match (script_of(c), runs.last_mut()) {
            (Some(lang), Some((last, run))) if *last == lang => run.push(c),
            (Some(lang), _) => {
                let mut run = std::mem::take(&mut leading);
                run.push(c);
                runs.push((lang, run));
            }
            (None, Some((_, run))) => run.push(c),
            (None, None) => leading.push(c),
        }
    }
    let mut runs: Vec<(Lang, String)> =
        runs.into_iter().map(|(lang, run)| (lang, run.trim().to_string())).filter(|(_, run)| !run.is_empty()).collect();
    if runs.is_empty() {
        let rest = leading.trim();
        if rest.chars().any(char::is_alphanumeric) {
            runs.push((fallback, rest.to_string()));
        }
    }
    runs
}

/// Plain text of the spoken parts, for display.
pub fn display_text(segments: &[Segment]) -> String {
    segments
        .iter()
        .filter_map(|s| match s {
            Segment::Say { text, .. } => Some(text.as_str()),
            Segment::Link { .. } | Segment::Watch { .. } | Segment::Done { .. } | Segment::Missed(_) | Segment::Passed { .. } | Segment::Continue => None,
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missed_and_passed_tags_carry_the_phrase_and_are_not_spoken() {
        let raw = "<say lang=\"en\">Not quite.</say><missed phrase=\"around the clock\" hint=\"работать круглосуточно\" answer=\"We work around the clock.\" source=\"16_the_job_interview\" url=\"https://youtu.be/x?t=5\"/><passed phrase=\"get on my nerves\"/><continue/>";
        let segs = parse_reply(raw);
        assert_eq!(
            segs,
            vec![
                say(Lang::En, "Not quite."),
                Segment::Missed(Missed {
                    phrase: "around the clock".into(),
                    hint_ru: "работать круглосуточно".into(),
                    model_answer: "We work around the clock.".into(),
                    source: "16_the_job_interview".into(),
                    url: "https://youtu.be/x?t=5".into(),
                }),
                Segment::Passed { phrase: "get on my nerves".into() },
                Segment::Continue,
            ]
        );
        assert_eq!(display_text(&segs), "Not quite.");
        assert!(parse_reply("<missed hint=\"x\"/>").is_empty(), "no phrase, nothing to keep");
    }

    #[test]
    fn a_done_tag_names_the_finished_dialog_with_or_without_the_extension() {
        assert_eq!(
            parse_reply("<say lang=\"en\">Well done.</say><done dialog=\"16_the_job_interview.md\"/>"),
            vec![say(Lang::En, "Well done."), Segment::Done { dialog: "16_the_job_interview".into() }]
        );
        assert_eq!(parse_reply("<done dialog=\"x\"/>"), vec![Segment::Done { dialog: "x".into() }]);
        assert!(parse_reply("<done/>").is_empty(), "no name, nothing to remember");
    }

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
    fn the_watch_tag_carries_a_url_and_a_title() {
        let raw = "<say lang=\"en\">Watch this.</say><watch url=\"https://www.youtube.com/watch?v=abc\">Фразы с GET</watch>";
        assert_eq!(
            parse_reply(raw),
            vec![
                say(Lang::En, "Watch this."),
                Segment::Watch { url: "https://www.youtube.com/watch?v=abc".into(), label: "Фразы с GET".into() },
            ]
        );
        // A tag without a url is ignored rather than guessed.
        assert!(parse_reply("<watch>no url</watch>").iter().all(|s| !matches!(s, Segment::Watch { .. })));
    }

    #[test]
    fn the_continue_tag_is_recognised_in_both_spellings() {
        for tag in ["<continue/>", "<continue />", "<continue></continue>"] {
            let raw = format!("<say lang=\"en\">Good.</say>{tag}");
            let segs = parse_reply(&raw);
            assert_eq!(segs.first(), Some(&say(Lang::En, "Good.")), "{tag}");
            assert_eq!(segs.get(1), Some(&Segment::Continue), "{tag}");
        }
        // The tag alone is not spoken text, so nothing falls back to "untagged English".
        assert_eq!(parse_reply("<continue/>"), vec![Segment::Continue]);
    }

    fn runs(fallback: Lang, text: &str) -> Vec<(Lang, String)> {
        split_by_script(fallback, text)
    }

    fn run(lang: Lang, text: &str) -> (Lang, String) {
        (lang, text.to_string())
    }

    #[test]
    fn russian_words_in_an_english_tag_go_to_the_russian_voice_and_back() {
        assert_eq!(runs(Lang::En, "Привет, это урок."), vec![run(Lang::Ru, "Привет, это урок.")]);
        assert_eq!(runs(Lang::Ru, "Take your time."), vec![run(Lang::En, "Take your time.")]);
        assert_eq!(runs(Lang::En, "Hello."), vec![run(Lang::En, "Hello.")]);
    }

    #[test]
    fn a_mixed_sentence_is_split_where_the_alphabet_changes() {
        assert_eq!(
            runs(Lang::Ru, "Native-версия: My manager gave us two days. Фраза put together значит собрать."),
            vec![
                run(Lang::En, "Native-"),
                run(Lang::Ru, "версия:"),
                run(Lang::En, "My manager gave us two days."),
                run(Lang::Ru, "Фраза"),
                run(Lang::En, "put together"),
                run(Lang::Ru, "значит собрать."),
            ]
        );
    }

    #[test]
    fn digits_and_punctuation_follow_the_run_and_letterless_text_uses_the_tag() {
        assert_eq!(runs(Lang::Ru, "Карточка 25, вопрос 3!"), vec![run(Lang::Ru, "Карточка 25, вопрос 3!")]);
        assert_eq!(runs(Lang::Ru, "— Хорошо."), vec![run(Lang::Ru, "— Хорошо.")]);
        assert_eq!(runs(Lang::En, "25"), vec![run(Lang::En, "25")]);
        assert_eq!(runs(Lang::Ru, "42"), vec![run(Lang::Ru, "42")]);
        assert!(runs(Lang::En, "...").is_empty());
        assert!(runs(Lang::En, "   ").is_empty());
        // Quotes around an English phrase inside Russian text.
        assert_eq!(
            runs(Lang::Ru, "Скажи «take your time» спокойно."),
            vec![run(Lang::Ru, "Скажи «"), run(Lang::En, "take your time»"), run(Lang::Ru, "спокойно.")]
        );
    }

    #[test]
    fn display_text_skips_links() {
        let segs = parse_reply("<say lang=\"en\">One.</say><link url=\"u\">l</link><say lang=\"ru\">Два.</say>");
        assert_eq!(display_text(&segs), "One. Два.");
    }
}
