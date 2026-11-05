mod app;
#[cfg(test)]
mod audio_loop;
mod backend;
mod claude;
mod dialog;
mod instruction;
mod library;
mod markup;
mod prompt;
mod session;
mod stt;
mod tts;
mod videos;

use anyhow::{Context, Result, bail};
use std::path::PathBuf;
use std::sync::Arc;

const USAGE: &str = "usage: server --instruction <Instruction.md>\n\
    The instruction file says where the dialogs are, what is learned and how long sessions live;\n\
    see Instruction.md in the repository root. SPEECH_TRAINER_INSTRUCTION can replace the argument.\n\
    Other settings (environment): SPEECH_TRAINER_BIND, SPEECH_TRAINER_MODEL, SPEECH_TRAINER_PIPER,\n\
    SPEECH_TRAINER_VOICES, SPEECH_TRAINER_CLAUDE_BIN, SPEECH_TRAINER_CLAUDE_MODEL, SPEECH_TRAINER_CLAUDE_PROXY";

fn env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}

fn instruction_path() -> Result<PathBuf> {
    let mut args = std::env::args().skip(1);
    let mut path = env("SPEECH_TRAINER_INSTRUCTION");
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--instruction" => path = Some(args.next().context("--instruction needs a file path")?),
            "-h" | "--help" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            other => match other.strip_prefix("--instruction=") {
                Some(p) => path = Some(p.to_string()),
                None => bail!("unknown argument {other:?}\n{USAGE}"),
            },
        }
    }
    path.map(PathBuf::from).with_context(|| format!("no instruction file given\n{USAGE}"))
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let path = instruction_path()?;
    let instruction = instruction::load(&path).with_context(|| format!("bad instruction file {}", path.display()))?;
    if !instruction.dialogs_dir.is_dir() {
        bail!("dialogs_dir {} (from {}) does not exist", instruction.dialogs_dir.display(), path.display());
    }

    let library = library::Library::scan(&instruction.dialogs_dir).context("cannot read the dialogs folder")?;
    if library.entries.is_empty() {
        bail!("no dialogs found in {}", instruction.dialogs_dir.display());
    }

    let videos = match &instruction.videos_dir {
        Some(dir) => {
            let lib = videos::VideoLibrary::scan(dir).with_context(|| format!("cannot read videos_dir {}", dir.display()))?;
            if lib.videos.is_empty() {
                tracing::warn!("no usable video transcripts in {}: video mode is off", dir.display());
                None
            } else {
                tracing::info!("video mode: {} videos from {}", lib.videos.len(), dir.display());
                Some(lib)
            }
        }
        None => None,
    };

    let model = PathBuf::from(env("SPEECH_TRAINER_MODEL").unwrap_or_else(|| {
        let home = std::env::var("HOME").unwrap_or_default();
        format!("{home}/.cache/speech-trainer/ggml-small.en.bin")
    }));
    if !model.is_file() {
        bail!("whisper model {} not found; download a ggml model (ggml-small.en.bin from ggerganov/whisper.cpp on Hugging Face) or set SPEECH_TRAINER_MODEL", model.display());
    }
    let bind = env("SPEECH_TRAINER_BIND").unwrap_or_else(|| "127.0.0.1:8765".into());

    let claude = claude::ClaudeConfig::from_env(instruction.model.as_deref());
    tracing::info!(
        "dialogs: {} ({} files), claude model: {}, session timeout: {} min, max sessions: {}",
        instruction.dialogs_dir.display(),
        library.entries.len(),
        claude.model,
        instruction.session_timeout.as_secs() / 60,
        instruction.max_sessions
    );
    let state = Arc::new(app::AppState::new(
        instruction,
        library,
        videos,
        claude,
        Arc::new(stt::WhisperTranscriber::load(&model, "en").context("cannot load the speech recognition model")?),
        Arc::new(tts::PiperSynthesizer::start(&tts::PiperConfig::from_env())),
    ));

    let listener = tokio::net::TcpListener::bind(&bind).await.with_context(|| format!("cannot bind {bind}"))?;
    tracing::info!("speech-trainer server listening on {bind}");
    axum::serve(listener, app::router(state)).await.context("server error")?;
    Ok(())
}
