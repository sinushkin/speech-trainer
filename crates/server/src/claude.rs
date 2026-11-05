//! A resident `claude -p` process in stream-json mode, one per dialog session.
//! Conversation context lives inside the process, so each turn sends only the
//! learner's new words. Approach taken from LangTeacher's claude-llama-proxy.

use crate::backend::{BackendError, Tutor};
use serde_json::{Value, json};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

/// The tutor may only read: these are the only tools it gets, and with
/// `dontAsk` anything that would need a permission (for example a path outside
/// its working directory, the dialogs folder) is refused instead of prompting.
const TOOLS: &str = "Read,Glob,Grep";

#[derive(Debug, Clone)]
pub struct ClaudeConfig {
    pub bin: String,
    pub model: String,
    /// Optional HTTP(S) proxy for the claude process, e.g. `http://127.0.0.1:8118`.
    pub proxy: Option<String>,
    pub turn_timeout: Duration,
}

impl ClaudeConfig {
    /// `default_model` (from the instruction file) is used unless
    /// `SPEECH_TRAINER_CLAUDE_MODEL` is set; without either it is Haiku.
    pub fn from_env(default_model: Option<&str>) -> Self {
        let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        Self {
            bin: env("SPEECH_TRAINER_CLAUDE_BIN").unwrap_or_else(|| "claude".into()),
            model: env("SPEECH_TRAINER_CLAUDE_MODEL")
                .or_else(|| default_model.map(str::to_string))
                .unwrap_or_else(|| "haiku".into()),
            proxy: env("SPEECH_TRAINER_CLAUDE_PROXY"),
            turn_timeout: Duration::from_secs(120),
        }
    }
}

/// Command line arguments of the resident process.
/// `extra_dirs` are folders besides the working directory that the read-only
/// tools may look into (the video transcripts).
pub fn build_args(cfg: &ClaudeConfig, system_prompt: &str, extra_dirs: &[std::path::PathBuf]) -> Vec<String> {
    let mut args: Vec<String> = [
        "-p",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--verbose",
        "--no-session-persistence",
        "--tools",
        TOOLS,
        "--permission-mode",
        "dontAsk",
        "--model",
        &cfg.model,
        "--system-prompt",
        system_prompt,
    ]
    .iter()
    .map(|a| a.to_string())
    .collect();
    for dir in extra_dirs {
        args.push("--add-dir".to_string());
        args.push(dir.display().to_string());
    }
    args
}

pub struct ClaudeSession {
    // Held so the process is killed when the session is dropped.
    _child: Child,
    stdin: ChildStdin,
    lines: Lines<BufReader<ChildStdout>>,
    timeout: Duration,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Event {
    Result { text: String, is_error: bool },
    Other,
}

/// Classifies one line of claude's stream-json output.
pub fn parse_event(line: &str) -> Event {
    let Ok(v) = serde_json::from_str::<Value>(line) else {
        return Event::Other;
    };
    if v.get("type").and_then(Value::as_str) != Some("result") {
        return Event::Other;
    }
    Event::Result {
        text: v.get("result").and_then(Value::as_str).unwrap_or_default().to_string(),
        is_error: v.get("is_error").and_then(Value::as_bool).unwrap_or(false),
    }
}

pub fn user_line(text: &str) -> String {
    json!({ "type": "user", "message": { "role": "user", "content": text } }).to_string()
}

impl ClaudeSession {
    /// Starts a Claude process whose working directory is `cwd` (the dialogs folder).
    pub fn spawn(
        cfg: &ClaudeConfig,
        system_prompt: &str,
        cwd: &std::path::Path,
        extra_dirs: &[std::path::PathBuf],
    ) -> Result<Self, BackendError> {
        let mut cmd = Command::new(&cfg.bin);
        cmd.args(build_args(cfg, system_prompt, extra_dirs)).current_dir(cwd);
        if let Some(proxy) = &cfg.proxy {
            cmd.env("HTTP_PROXY", proxy).env("HTTPS_PROXY", proxy);
        }
        cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).kill_on_drop(true);
        let mut child = cmd.spawn()?;
        let stdin = child.stdin.take().ok_or_else(|| BackendError::Failed("claude stdin was not piped".into()))?;
        let stdout = child.stdout.take().ok_or_else(|| BackendError::Failed("claude stdout was not piped".into()))?;
        Ok(Self { _child: child, stdin, lines: BufReader::new(stdout).lines(), timeout: cfg.turn_timeout })
    }

    async fn ask_inner(&mut self, user_text: &str) -> Result<String, BackendError> {
        self.stdin.write_all(user_line(user_text).as_bytes()).await?;
        self.stdin.write_all(b"\n").await?;
        self.stdin.flush().await?;
        loop {
            let line = self.lines.next_line().await?.ok_or(BackendError::Exited)?;
            match parse_event(&line) {
                Event::Result { text, is_error: false } => return Ok(text),
                Event::Result { text, is_error: true } => return Err(BackendError::Failed(text)),
                Event::Other => {}
            }
        }
    }
}

impl Tutor for ClaudeSession {
    async fn ask(&mut self, user_text: &str) -> Result<String, BackendError> {
        let timeout = self.timeout;
        match tokio::time::timeout(timeout, self.ask_inner(user_text)).await {
            Ok(result) => result,
            Err(_) => Err(BackendError::Timeout(timeout)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_the_result_event_and_ignores_the_rest() {
        assert_eq!(
            parse_event(r#"{"type":"result","subtype":"success","is_error":false,"result":"<say>Hi</say>"}"#),
            Event::Result { text: "<say>Hi</say>".into(), is_error: false }
        );
        assert_eq!(
            parse_event(r#"{"type":"result","is_error":true,"result":"rate limit"}"#),
            Event::Result { text: "rate limit".into(), is_error: true }
        );
        assert_eq!(parse_event(r#"{"type":"assistant","message":{}}"#), Event::Other);
        assert_eq!(parse_event("not json"), Event::Other);
    }

    #[test]
    fn the_tutor_gets_read_only_tools_and_no_permission_bypass() {
        let cfg = ClaudeConfig { bin: "claude".into(), model: "haiku".into(), proxy: None, turn_timeout: Duration::from_secs(1) };
        let args = build_args(&cfg, "PROMPT", &[]);
        let after = |flag: &str| args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).cloned();
        assert_eq!(after("--tools").as_deref(), Some("Read,Glob,Grep"));
        assert_eq!(after("--permission-mode").as_deref(), Some("dontAsk"));
        assert_eq!(after("--model").as_deref(), Some("haiku"));
        assert_eq!(after("--system-prompt").as_deref(), Some("PROMPT"));
        assert!(!args.iter().any(|a| a.contains("dangerously")), "permissions must not be bypassed");
        assert!(!args.contains(&"--add-dir".to_string()), "no extra folder unless asked");

        let with_dir = build_args(&cfg, "PROMPT", &[std::path::PathBuf::from("/data/transcripts")]);
        let i = with_dir.iter().position(|a| a == "--add-dir");
        assert_eq!(i.and_then(|i| with_dir.get(i + 1)).map(String::as_str), Some("/data/transcripts"));
    }

    #[test]
    fn user_line_is_one_line_of_valid_json() -> Result<(), serde_json::Error> {
        let line = user_line("hello\n\"quoted\"");
        assert!(!line.contains('\n'));
        let v: Value = serde_json::from_str(&line)?;
        assert_eq!(v["message"]["content"], "hello\n\"quoted\"");
        Ok(())
    }

    /// Drives the real session code against a stand-in script that speaks the
    /// stream-json protocol, so no network or subscription is needed.
    #[tokio::test]
    async fn talks_to_a_stream_json_child_process() -> Result<(), BackendError> {
        let script = std::env::temp_dir().join(format!("fake-claude-{}.sh", std::process::id()));
        std::fs::write(
            &script,
            "#!/bin/sh\nn=0\nwhile read -r line; do\n  n=$((n+1))\n  echo '{\"type\":\"system\"}'\n  echo \"{\\\"type\\\":\\\"result\\\",\\\"is_error\\\":false,\\\"result\\\":\\\"turn $n\\\"}\"\ndone\n",
        )?;
        let cfg = ClaudeConfig {
            bin: "sh".into(),
            model: "m".into(),
            proxy: None,
            turn_timeout: Duration::from_secs(5),
        };
        let mut session = spawn_with_script(&cfg, &script)?;
        assert_eq!(session.ask("first").await?, "turn 1");
        assert_eq!(session.ask("second").await?, "turn 2");
        std::fs::remove_file(&script)?;
        Ok(())
    }

    fn spawn_with_script(cfg: &ClaudeConfig, script: &std::path::Path) -> Result<ClaudeSession, BackendError> {
        let mut cmd = Command::new(&cfg.bin);
        cmd.arg(script);
        cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).kill_on_drop(true);
        let mut child = cmd.spawn()?;
        let stdin = child.stdin.take().ok_or(BackendError::Exited)?;
        let stdout = child.stdout.take().ok_or(BackendError::Exited)?;
        Ok(ClaudeSession { _child: child, stdin, lines: BufReader::new(stdout).lines(), timeout: cfg.turn_timeout })
    }
}
