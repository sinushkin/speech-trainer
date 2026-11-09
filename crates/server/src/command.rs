//! Commands the server handles itself, without asking the tutor: pausing the
//! lesson. They are recognised in the learner's words, and like every command
//! they start with the word "command" ("command pause"), so that an ordinary
//! answer is never taken for one. All other commands are left to the tutor
//! (see `Instruction.md`).

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Pause,
    Resume,
}

fn words(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !(c.is_alphanumeric() || c == '\''))
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .collect()
}

/// `paused` is the current state: while the lesson is paused the bare word
/// "resume" works too, because nothing else can be meant.
pub fn parse(text: &str, paused: bool) -> Option<Command> {
    let words = words(text);
    let words: Vec<&str> = words.iter().map(String::as_str).collect();
    match words.as_slice() {
        ["command" | "commands", "pause"] => Some(Command::Pause),
        ["command" | "commands", "resume" | "continue"] => Some(Command::Resume),
        ["resume"] if paused => Some(Command::Resume),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pause_and_resume_need_the_word_command() {
        for text in ["Command pause", "command, pause.", " Command: PAUSE! ", "Commands pause"] {
            assert_eq!(parse(text, false), Some(Command::Pause), "{text:?}");
        }
        for text in ["command resume", "Command, continue."] {
            assert_eq!(parse(text, true), Some(Command::Resume), "{text:?}");
        }
    }

    #[test]
    fn ordinary_speech_is_not_a_command() {
        for text in ["pause", "Let's pause for a moment", "I will take a pause", "command", "command pause the lesson", "continue"] {
            assert_eq!(parse(text, false), None, "{text:?}");
        }
    }

    #[test]
    fn a_bare_resume_counts_only_while_paused() {
        assert_eq!(parse("Resume.", true), Some(Command::Resume));
        assert_eq!(parse("Resume.", false), None);
    }
}
