//! Loads prepared dialogs written in the `english-transcripts/dialogs` Markdown
//! format: a dialogue plus quiz cards (question, hint, model answer, video link).

use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Card {
    pub phrase: String,
    pub ask: String,
    pub hint: String,
    pub model_answer: String,
    pub url: String,
    pub link_label: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dialog {
    pub title: String,
    pub setting: String,
    pub lines: Vec<(String, String)>,
    pub cards: Vec<Card>,
}

#[derive(Debug, thiserror::Error)]
pub enum DialogError {
    #[error("invalid dialog name {0:?}: only letters, digits, '_' and '-' are allowed")]
    BadName(String),
    #[error("cannot read dialog {name}: {source}")]
    Io { name: String, source: std::io::Error },
    #[error("dialog has no title line (`# Dialog NN: Title`)")]
    NoTitle,
    #[error("dialog has no cards")]
    NoCards,
}

/// Loads `<dir>/<name>.md`. `name` comes from the network, so it is restricted
/// to a safe character set before it touches the file system.
pub fn load(dir: &Path, name: &str) -> Result<Dialog, DialogError> {
    let valid = !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if !valid {
        return Err(DialogError::BadName(name.to_string()));
    }
    let text = std::fs::read_to_string(dir.join(format!("{name}.md")))
        .map_err(|source| DialogError::Io { name: name.to_string(), source })?;
    parse(&text)
}

/// Names of the dialogs available in `dir`, sorted.
pub fn list(dir: &Path) -> std::io::Result<Vec<String>> {
    let mut names = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().and_then(|e| e.to_str()) == Some("md")
            && let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                names.push(stem.to_string());
            }
    }
    names.sort();
    Ok(names)
}

fn field<'a>(line: &'a str, label: &str) -> Option<&'a str> {
    let rest = line.strip_prefix("- **")?.strip_prefix(label)?.strip_prefix(":** ")?;
    Some(rest.trim())
}

pub fn parse(markdown: &str) -> Result<Dialog, DialogError> {
    let mut title = None;
    let mut setting = String::new();
    let mut lines = Vec::new();
    let mut cards: Vec<Card> = Vec::new();
    let mut in_cards = false;

    for raw in markdown.lines() {
        let line = raw.trim_end();
        if let Some(rest) = line.strip_prefix("# Dialog ") {
            title = Some(rest.split_once(": ").map_or(rest, |(_, t)| t).trim().to_string());
        } else if line.starts_with("**Level:**") {
            if let Some((_, s)) = line.split_once("**Setting:**") {
                setting = s.trim().to_string();
            }
        } else if line.starts_with("## Cards") {
            in_cards = true;
        } else if !in_cards {
            if let Some(rest) = line.strip_prefix("**")
                && let Some((speaker, text)) = rest.split_once(":** ") {
                    lines.push((speaker.to_string(), text.trim().to_string()));
                }
        } else if let Some(heading) = line.strip_prefix("### ") {
            let phrase = heading.split_once(". ").map_or(heading, |(_, p)| p);
            cards.push(Card {
                phrase: phrase.trim().to_string(),
                ask: String::new(),
                hint: String::new(),
                model_answer: String::new(),
                url: String::new(),
                link_label: String::new(),
            });
        } else if let Some(card) = cards.last_mut() {
            if let Some(v) = field(line, "Ask (EN)") {
                card.ask = v.to_string();
            } else if let Some(v) = field(line, "Hint (RU)") {
                card.hint = v.to_string();
            } else if let Some(v) = field(line, "Model answer") {
                card.model_answer = v.to_string();
            } else if let Some(v) = field(line, "In the video") {
                // «label» — [id @ m:ss](https://youtu.be/id?t=N)
                if let (Some(open), Some(close)) = (v.rfind("]("), v.rfind(')'))
                    && open + 2 <= close {
                        card.url = v[open + 2..close].to_string();
                    }
                if let (Some(s), Some(e)) = (v.rfind('['), v.rfind("]("))
                    && s < e {
                        card.link_label = v[s + 1..e].to_string();
                    }
            }
        }
    }

    let title = title.ok_or(DialogError::NoTitle)?;
    if cards.is_empty() {
        return Err(DialogError::NoCards);
    }
    Ok(Dialog { title, setting, lines, cards })
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("../tests/fixtures/16_the_job_interview.md");

    #[test]
    fn parses_a_real_dialog() -> Result<(), DialogError> {
        let d = parse(FIXTURE)?;
        assert_eq!(d.title, "The Job Interview");
        assert!(!d.setting.is_empty());
        assert!(d.lines.len() >= 20, "dialogue lines: {}", d.lines.len());
        assert_eq!(d.cards.len(), 12);
        for c in &d.cards {
            assert!(!c.phrase.is_empty() && !c.ask.is_empty() && !c.hint.is_empty());
            assert!(!c.model_answer.is_empty());
            assert!(c.url.starts_with("https://youtu.be/") && c.url.contains("?t="), "{}", c.url);
            assert!(c.link_label.contains(" @ "), "{}", c.link_label);
        }
        Ok(())
    }

    #[test]
    fn rejects_unsafe_names_before_touching_the_disk() {
        for bad in ["", "../etc/passwd", "a/b", "x.md", "a b"] {
            assert!(matches!(load(Path::new("."), bad), Err(DialogError::BadName(_))), "{bad}");
        }
    }

    #[test]
    fn missing_pieces_are_reported() {
        assert!(matches!(parse("no title here"), Err(DialogError::NoTitle)));
        assert!(matches!(parse("# Dialog 01: T\n"), Err(DialogError::NoCards)));
    }
}
