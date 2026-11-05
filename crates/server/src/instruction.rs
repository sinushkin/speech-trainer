//! The instruction file the server is started with. It says where the dialogs
//! are and what is being learned, and sets the session limits:
//!
//! ```text
//! ---
//! dialogs_dir: ~/repos/english-transcripts/dialogs
//! model: haiku
//! session_timeout_minutes: 30
//! correction_pause_seconds: 3
//! max_sessions: 4
//! ---
//! Free text: what we learn and how the tutor should teach. It goes into the
//! tutor's system prompt as it is.
//! ```
//!
//! `dialogs_dir` is required; a relative path is resolved against the folder of
//! the instruction file, and `~/` means the home directory.

use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Instruction {
    pub dialogs_dir: PathBuf,
    /// Claude model for every session; `None` means the server default (Haiku).
    pub model: Option<String>,
    /// A session with no activity for this long is stopped.
    pub session_timeout: Duration,
    /// After a correction the client waits this long for the learner (who may ask
    /// "in Russian"); if the learner stays silent the lesson goes on by itself.
    pub correction_pause: Duration,
    /// How many sessions (Claude processes) may run at once.
    pub max_sessions: usize,
    /// What we learn and how: the text below the header.
    pub body: String,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum InstructionError {
    #[error("cannot read {path}: {message}")]
    Io { path: String, message: String },
    #[error("the file must start with a `---` header holding settings such as dialogs_dir")]
    NoHeader,
    #[error("the `---` header is never closed")]
    UnclosedHeader,
    #[error("header line {0:?} is not `key: value`")]
    BadLine(String),
    #[error("unknown setting {0:?} (known: dialogs_dir, model, session_timeout_minutes, correction_pause_seconds, max_sessions)")]
    UnknownKey(String),
    #[error("the setting `dialogs_dir` is required")]
    MissingDialogsDir,
    #[error("setting {key} must be a positive whole number, got {value:?}")]
    BadNumber { key: &'static str, value: String },
    #[error("the instruction text below the header is empty")]
    EmptyBody,
}

fn expand(path: &str, base: &Path, home: Option<&Path>) -> PathBuf {
    if let (Some(rest), Some(home)) = (path.strip_prefix("~/"), home) {
        return home.join(rest);
    }
    let p = Path::new(path);
    if p.is_absolute() { p.to_path_buf() } else { base.join(p) }
}

fn positive(key: &'static str, value: &str) -> Result<u64, InstructionError> {
    value
        .parse::<u64>()
        .ok()
        .filter(|n| *n > 0)
        .ok_or_else(|| InstructionError::BadNumber { key, value: value.to_string() })
}

pub fn parse(text: &str, base_dir: &Path, home: Option<&Path>) -> Result<Instruction, InstructionError> {
    let mut lines = text.lines();
    if lines.next().map(str::trim) != Some("---") {
        return Err(InstructionError::NoHeader);
    }
    let mut dialogs_dir = None;
    let mut model = None;
    let mut timeout_minutes = 30;
    let mut correction_pause_seconds = 3;
    let mut max_sessions = 4;
    let mut closed = false;
    for line in lines.by_ref() {
        let line = line.trim();
        if line == "---" {
            closed = true;
            break;
        }
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, value) = line.split_once(':').ok_or_else(|| InstructionError::BadLine(line.to_string()))?;
        let value = value.trim();
        match key.trim() {
            "dialogs_dir" => dialogs_dir = Some(expand(value, base_dir, home)),
            "model" => model = Some(value.to_string()).filter(|m| !m.is_empty()),
            "session_timeout_minutes" => timeout_minutes = positive("session_timeout_minutes", value)?,
            "correction_pause_seconds" => correction_pause_seconds = positive("correction_pause_seconds", value)?,
            "max_sessions" => max_sessions = positive("max_sessions", value)? as usize,
            other => return Err(InstructionError::UnknownKey(other.to_string())),
        }
    }
    if !closed {
        return Err(InstructionError::UnclosedHeader);
    }
    let body = lines.collect::<Vec<_>>().join("\n").trim().to_string();
    if body.is_empty() {
        return Err(InstructionError::EmptyBody);
    }
    Ok(Instruction {
        dialogs_dir: dialogs_dir.ok_or(InstructionError::MissingDialogsDir)?,
        model,
        session_timeout: Duration::from_secs(timeout_minutes * 60),
        correction_pause: Duration::from_secs(correction_pause_seconds),
        max_sessions,
        body,
    })
}

pub fn load(path: &Path) -> Result<Instruction, InstructionError> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| InstructionError::Io { path: path.display().to_string(), message: e.to_string() })?;
    let base = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
    let home = std::env::var_os("HOME").map(PathBuf::from);
    parse(&text, base, home.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(text: &str) -> Result<Instruction, InstructionError> {
        parse(text, Path::new("/srv/trainer"), Some(Path::new("/home/me")))
    }

    #[test]
    fn parses_a_complete_file() -> Result<(), InstructionError> {
        let i = p("---\n# comment\ndialogs_dir: ~/dialogs\nmodel: haiku\nsession_timeout_minutes: 45\ncorrection_pause_seconds: 5\nmax_sessions: 2\n---\nLearn idioms.\n\nBe brief.\n")?;
        assert_eq!(i.dialogs_dir, Path::new("/home/me/dialogs"));
        assert_eq!(i.model.as_deref(), Some("haiku"));
        assert_eq!(i.session_timeout, Duration::from_secs(45 * 60));
        assert_eq!(i.correction_pause, Duration::from_secs(5));
        assert_eq!(i.max_sessions, 2);
        assert_eq!(i.body, "Learn idioms.\n\nBe brief.");
        Ok(())
    }

    #[test]
    fn defaults_are_thirty_minutes_and_four_sessions() -> Result<(), InstructionError> {
        let i = p("---\ndialogs_dir: /abs/dialogs\n---\ntext")?;
        assert_eq!(i.session_timeout, Duration::from_secs(30 * 60));
        assert_eq!(i.max_sessions, 4);
        assert_eq!(i.correction_pause, Duration::from_secs(3));
        assert_eq!(i.model, None);
        assert_eq!(i.dialogs_dir, Path::new("/abs/dialogs"));
        Ok(())
    }

    #[test]
    fn a_relative_dialogs_dir_is_relative_to_the_instruction_file() -> Result<(), InstructionError> {
        assert_eq!(p("---\ndialogs_dir: dialogs\n---\ntext")?.dialogs_dir, Path::new("/srv/trainer/dialogs"));
        Ok(())
    }

    #[test]
    fn mistakes_are_reported_precisely() {
        assert_eq!(p("no header"), Err(InstructionError::NoHeader));
        assert_eq!(p("---\ndialogs_dir: x\n"), Err(InstructionError::UnclosedHeader));
        assert_eq!(p("---\ndialogs_dir x\n---\nt"), Err(InstructionError::BadLine("dialogs_dir x".into())));
        assert_eq!(p("---\ndialogs_dir: x\ntimeout: 5\n---\nt"), Err(InstructionError::UnknownKey("timeout".into())));
        assert_eq!(p("---\nmodel: haiku\n---\nt"), Err(InstructionError::MissingDialogsDir));
        assert_eq!(
            p("---\ndialogs_dir: x\nmax_sessions: 0\n---\nt"),
            Err(InstructionError::BadNumber { key: "max_sessions", value: "0".into() })
        );
        assert_eq!(p("---\ndialogs_dir: x\n---\n  \n"), Err(InstructionError::EmptyBody));
    }

    #[test]
    fn the_repository_instruction_file_is_valid() -> Result<(), InstructionError> {
        let i = load(Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../Instruction.md")))?;
        assert!(i.body.len() > 200);
        for command in ["next dialog", "previous dialog", "another dialog", "repeat", "in Russian"] {
            assert!(i.body.contains(command), "the instruction file must list the command {command:?}");
        }
        assert_eq!(i.session_timeout, Duration::from_secs(30 * 60));
        Ok(())
    }
}
