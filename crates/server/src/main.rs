mod app;
mod backend;
mod claude;
mod dialog;
mod markup;
mod prompt;
mod session;
mod stt;
mod tts;

use anyhow::{Context, Result, bail};
use std::path::PathBuf;
use std::sync::Arc;

fn env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let model = PathBuf::from(env("SPEECH_TRAINER_MODEL").unwrap_or_else(|| {
        let home = std::env::var("HOME").unwrap_or_default();
        format!("{home}/.cache/speech-trainer/ggml-small.en.bin")
    }));
    if !model.is_file() {
        bail!("whisper model {} not found; download a ggml model (ggml-small.en.bin from ggerganov/whisper.cpp on Hugging Face) or set SPEECH_TRAINER_MODEL", model.display());
    }
    let bind = env("SPEECH_TRAINER_BIND").unwrap_or_else(|| "127.0.0.1:8765".into());
    let dialogs_dir = PathBuf::from(env("SPEECH_TRAINER_DIALOGS").unwrap_or_else(|| "dialogs".into()));
    if !dialogs_dir.is_dir() {
        bail!("dialogs directory {} does not exist (set SPEECH_TRAINER_DIALOGS)", dialogs_dir.display());
    }

    let state = Arc::new(app::AppState {
        dialogs_dir,
        claude: claude::ClaudeConfig::from_env(),
        stt: Arc::new(stt::WhisperTranscriber::load(&model, "en").context("cannot load the speech recognition model")?),
        tts: Arc::new(tts::NoSpeech),
    });

    let listener = tokio::net::TcpListener::bind(&bind).await.with_context(|| format!("cannot bind {bind}"))?;
    tracing::info!("speech-trainer server listening on {bind}");
    axum::serve(listener, app::router(state)).await.context("server error")?;
    Ok(())
}
