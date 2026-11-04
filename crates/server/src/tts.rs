//! Speech synthesis with Piper voices. One resident `piper --json-input`
//! process per language (loading a voice takes a moment, so it happens once);
//! each request is one JSON line in, the written file's path as one line out.
//! A language whose voice cannot be started stays unavailable: the lesson goes
//! on with text only for that language.

use crate::backend::{BackendError, Synthesizer};
use crate::markup::Lang;
use serde_json::json;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;

#[derive(Debug, Clone)]
pub struct PiperConfig {
    /// The `piper` executable (it finds `espeak-ng-data` next to itself).
    pub bin: PathBuf,
    pub voices_dir: PathBuf,
    pub voice_en: String,
    pub voice_ru: String,
}

impl PiperConfig {
    pub fn from_env() -> Self {
        let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        let cache = PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".cache/speech-trainer");
        Self {
            bin: env("SPEECH_TRAINER_PIPER").map_or_else(|| cache.join("piper/piper"), PathBuf::from),
            voices_dir: env("SPEECH_TRAINER_VOICES").map_or_else(|| cache.join("voices"), PathBuf::from),
            voice_en: env("SPEECH_TRAINER_VOICE_EN").unwrap_or_else(|| "en_US-amy-medium".into()),
            voice_ru: env("SPEECH_TRAINER_VOICE_RU").unwrap_or_else(|| "ru_RU-irina-medium".into()),
        }
    }
}

struct Voice {
    // Held so the process is killed when the voice is dropped.
    _child: Child,
    stdin: ChildStdin,
    lines: Lines<BufReader<ChildStdout>>,
}

impl Voice {
    fn start(cfg: &PiperConfig, name: &str) -> Result<Self, BackendError> {
        let model = cfg.voices_dir.join(format!("{name}.onnx"));
        if !model.is_file() {
            return Err(BackendError::Failed(format!("voice file {} not found", model.display())));
        }
        let mut cmd = Command::new(&cfg.bin);
        cmd.arg("--model").arg(&model).arg("--json-input");
        cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).kill_on_drop(true);
        let mut child = cmd
            .spawn()
            .map_err(|e| BackendError::Failed(format!("cannot start {}: {e}", cfg.bin.display())))?;
        let stdin = child.stdin.take().ok_or_else(|| BackendError::Failed("piper stdin was not piped".into()))?;
        let stdout = child.stdout.take().ok_or_else(|| BackendError::Failed("piper stdout was not piped".into()))?;
        Ok(Self { _child: child, stdin, lines: BufReader::new(stdout).lines() })
    }

    async fn say(&mut self, text: &str, out: &std::path::Path) -> Result<(), BackendError> {
        let request = json!({ "text": text, "output_file": out }).to_string();
        self.stdin.write_all(request.as_bytes()).await?;
        self.stdin.write_all(b"\n").await?;
        self.stdin.flush().await?;
        // Piper prints the path of the finished file; any line means "done".
        self.lines.next_line().await?.ok_or(BackendError::Exited)?;
        Ok(())
    }
}

pub struct PiperSynthesizer {
    en: Option<Mutex<Voice>>,
    ru: Option<Mutex<Voice>>,
    timeout: Duration,
}

impl PiperSynthesizer {
    /// Starts both voices; a voice that fails is logged and left unavailable.
    pub fn start(cfg: &PiperConfig) -> Self {
        let voice = |lang: &str, name: &str| match Voice::start(cfg, name) {
            Ok(v) => Some(Mutex::new(v)),
            Err(e) => {
                tracing::warn!("{lang} voice unavailable: {e}");
                None
            }
        };
        Self { en: voice("en", &cfg.voice_en), ru: voice("ru", &cfg.voice_ru), timeout: Duration::from_secs(60) }
    }
}

fn temp_wav() -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("speech-trainer-tts-{}-{}.wav", std::process::id(), n))
}

impl Synthesizer for PiperSynthesizer {
    async fn synthesize(&self, lang: Lang, text: &str) -> Result<Vec<u8>, BackendError> {
        let voice = match lang {
            Lang::En => self.en.as_ref(),
            Lang::Ru => self.ru.as_ref(),
        }
        .ok_or_else(|| BackendError::Failed(format!("no {} voice is available", lang.code())))?;
        let path = temp_wav();
        let said = {
            let mut v = voice.lock().await;
            match tokio::time::timeout(self.timeout, v.say(text, &path)).await {
                Ok(r) => r,
                Err(_) => Err(BackendError::Timeout(self.timeout)),
            }
        };
        let bytes = match said {
            Ok(()) => tokio::fs::read(&path).await.map_err(BackendError::from),
            Err(e) => Err(e),
        };
        if let Err(e) = tokio::fs::remove_file(&path).await {
            tracing::debug!("could not remove {}: {e}", path.display());
        }
        bytes
    }
}
