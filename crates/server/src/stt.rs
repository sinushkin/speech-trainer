//! Speech recognition with whisper.cpp through the `whisper-rs` bindings.
//! The model is loaded once; every utterance gets its own decoding state and
//! runs on a blocking thread so the async runtime stays responsive.

use crate::backend::{BackendError, Transcriber};
use protocol::{SAMPLE_RATE, wav};
use std::path::Path;
use std::sync::Arc;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

pub struct WhisperTranscriber {
    ctx: Arc<WhisperContext>,
    language: String,
    threads: i32,
}

fn failed(what: &str, e: impl std::fmt::Display) -> BackendError {
    BackendError::Failed(format!("{what}: {e}"))
}

impl WhisperTranscriber {
    /// `model` is a ggml model file, e.g. `ggml-small.en.bin`.
    pub fn load(model: &Path, language: &str) -> Result<Self, BackendError> {
        let path = model
            .to_str()
            .ok_or_else(|| BackendError::Failed(format!("model path is not valid UTF-8: {}", model.display())))?;
        let ctx = WhisperContext::new_with_params(path, WhisperContextParameters::default())
            .map_err(|e| failed(&format!("cannot load whisper model {path}"), e))?;
        let threads = std::thread::available_parallelism().map_or(4, |n| n.get().min(8)) as i32;
        Ok(Self { ctx: Arc::new(ctx), language: language.to_string(), threads })
    }
}

/// 16 kHz mono PCM16 WAV bytes to the float samples whisper expects.
pub fn wav_to_f32(wav_bytes: &[u8]) -> Result<Vec<f32>, BackendError> {
    let (rate, samples) = wav::decode_pcm16_mono(wav_bytes).map_err(|e| failed("bad audio", e))?;
    if rate != SAMPLE_RATE {
        return Err(BackendError::Failed(format!("audio must be {SAMPLE_RATE} Hz, got {rate} Hz")));
    }
    Ok(samples.iter().map(|&s| f32::from(s) / 32768.0).collect())
}

fn recognise(ctx: &WhisperContext, language: &str, threads: i32, audio: &[f32]) -> Result<String, BackendError> {
    let mut state = ctx.create_state().map_err(|e| failed("cannot create whisper state", e))?;
    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    params.set_language(Some(language));
    params.set_n_threads(threads);
    params.set_translate(false);
    params.set_no_context(true);
    params.set_print_special(false);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);
    state.full(params, audio).map_err(|e| failed("whisper failed", e))?;

    let mut text = String::new();
    for segment in state.as_iter() {
        let piece = segment.to_str_lossy().map_err(|e| failed("cannot read a segment", e))?;
        text.push_str(&piece);
    }
    Ok(text.trim().to_string())
}

impl Transcriber for WhisperTranscriber {
    async fn transcribe(&self, wav_bytes: Vec<u8>) -> Result<String, BackendError> {
        let audio = wav_to_f32(&wav_bytes)?;
        let (ctx, language, threads) = (self.ctx.clone(), self.language.clone(), self.threads);
        tokio::task::spawn_blocking(move || recognise(&ctx, &language, threads, &audio))
            .await
            .map_err(|e| failed("recognition task failed", e))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_pcm16_to_unit_range_floats() -> Result<(), BackendError> {
        let wav_bytes = wav::encode_pcm16_mono(&[0, 16384, -16384, i16::MIN], SAMPLE_RATE);
        let f = wav_to_f32(&wav_bytes)?;
        assert_eq!(f, vec![0.0, 0.5, -0.5, -1.0]);
        Ok(())
    }

    #[test]
    fn wrong_sample_rate_is_rejected() {
        let wav_bytes = wav::encode_pcm16_mono(&[1, 2, 3], 22_050);
        assert!(matches!(wav_to_f32(&wav_bytes), Err(BackendError::Failed(m)) if m.contains("22050")));
    }

    /// Needs the real model: `SPEECH_TRAINER_MODEL=~/.cache/speech-trainer/ggml-small.en.bin cargo test -- --ignored`
    #[tokio::test]
    #[ignore = "needs a whisper ggml model (set SPEECH_TRAINER_MODEL)"]
    async fn recognises_a_synthesised_english_sentence() -> Result<(), Box<dyn std::error::Error>> {
        let model = std::env::var("SPEECH_TRAINER_MODEL")?;
        let stt = WhisperTranscriber::load(Path::new(&model), "en")?;
        let bytes = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/clock_16k.wav"))?;
        let text = stt.transcribe(bytes).await?;
        let lower = text.to_lowercase();
        assert!(lower.contains("around the clock"), "heard: {text:?}");
        Ok(())
    }
}
