//! The three pluggable parts of a turn: the tutor (an LLM session), the speech
//! recogniser and the speech synthesiser. Real implementations live in
//! `claude.rs` and `worker.rs`; tests use in-memory fakes.

use crate::markup::Lang;
use std::future::Future;

#[derive(Debug, thiserror::Error)]
pub enum BackendError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("timed out after {0:?}")]
    Timeout(std::time::Duration),
    #[error("process exited unexpectedly")]
    Exited,
    #[error("{0}")]
    Failed(String),
}

/// A conversation partner that keeps its own context between calls.
pub trait Tutor: Send {
    fn ask(&mut self, user_text: &str) -> impl Future<Output = Result<String, BackendError>> + Send;
}

pub trait Transcriber: Send + Sync {
    /// `wav` is a complete mono PCM16 WAV file; returns the recognised text.
    fn transcribe(&self, wav: Vec<u8>) -> impl Future<Output = Result<String, BackendError>> + Send;
}

pub trait Synthesizer: Send + Sync {
    /// Returns a complete WAV file with the spoken text.
    fn synthesize(&self, lang: Lang, text: &str) -> impl Future<Output = Result<Vec<u8>, BackendError>> + Send;
}
