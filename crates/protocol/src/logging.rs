//! Logging for both programs: to the terminal as before, and also to a file
//! (`~/.local/state/speech-trainer/<name>.log`, or `$SPEECH_TRAINER_LOG_DIR/<name>.log`)
//! so a stall can be investigated afterwards without the terminal.

use std::path::PathBuf;
use std::sync::Mutex;
use tracing_subscriber::Layer;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, fmt};

/// Directory of the log files.
pub fn log_dir(env: impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
    if let Some(d) = env("SPEECH_TRAINER_LOG_DIR") {
        return Some(PathBuf::from(d));
    }
    let base = env("XDG_STATE_HOME").map(PathBuf::from).or_else(|| env("HOME").map(|h| PathBuf::from(h).join(".local/state")))?;
    Some(base.join("speech-trainer"))
}

/// `terminal_default` is the terminal's level when RUST_LOG is not set; the file always gets `info`
/// (more with RUST_LOG).
pub fn init(name: &str, terminal_default: &str) {
    let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    let terminal_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(terminal_default));
    let file_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let terminal = fmt::layer().with_writer(std::io::stderr).with_filter(terminal_filter);

    let opened = log_dir(env).ok_or_else(|| "no HOME".to_string()).and_then(|dir| {
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let path = dir.join(format!("{name}.log"));
        let file = std::fs::OpenOptions::new().create(true).append(true).open(&path).map_err(|e| e.to_string())?;
        Ok((path, file))
    });
    match opened {
        Ok((path, file)) => {
            let to_file = fmt::layer().with_ansi(false).with_writer(Mutex::new(file)).with_filter(file_filter);
            tracing_subscriber::registry().with(terminal).with(to_file).init();
            tracing::info!("log file: {}", path.display());
        }
        Err(why) => {
            tracing_subscriber::registry().with(terminal).init();
            tracing::warn!("no log file: {why}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_log_directory_follows_the_environment() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |k: &str| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| v.to_string())
        };
        assert_eq!(log_dir(env(&[("SPEECH_TRAINER_LOG_DIR", "/l"), ("HOME", "/h")])), Some(PathBuf::from("/l")));
        assert_eq!(log_dir(env(&[("HOME", "/h")])), Some(PathBuf::from("/h/.local/state/speech-trainer")));
        assert_eq!(log_dir(env(&[("XDG_STATE_HOME", "/s"), ("HOME", "/h")])), Some(PathBuf::from("/s/speech-trainer")));
        assert_eq!(log_dir(env(&[])), None);
    }
}
