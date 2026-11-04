//! Parses prepared dialogs written in the `english-transcripts/dialogs` Markdown
//! format: a dialogue plus quiz cards (question, hint, model answer, video link).

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
    #[error("dialog has no title line (`# Dialog NN: Title`)")]
    NoTitle,
    #[error("dialog has no cards")]
    NoCards,
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
    fn missing_pieces_are_reported() {
        assert!(matches!(parse("no title here"), Err(DialogError::NoTitle)));
        assert!(matches!(parse("# Dialog 01: T\n"), Err(DialogError::NoCards)));
    }
}
