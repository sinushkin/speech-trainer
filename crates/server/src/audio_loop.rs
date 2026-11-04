//! One test of the whole audio loop, without Claude:
//!
//! text (Russian and English mixed) -> Piper speech -> mp3 file -> playback ->
//! decode -> Whisper recognition -> a server turn (fake tutor) -> spoken reply -> playback.
//!
//! It needs real models and tools, so it is ignored by default. Run it with
//! `cargo test -p server --release -- --ignored --nocapture`. Requirements:
//! the Whisper model (`SPEECH_TRAINER_MODEL`), the Piper binary and the two
//! voices (see `PiperConfig`), `ffmpeg` with libmp3lame, and `aplay` with a
//! sound device. Set `SPEECH_TRAINER_NO_PLAYBACK=1` to skip playing sound.

use crate::backend::{BackendError, Synthesizer, Tutor};
use crate::markup::Lang;
use crate::session::{Out, Session};
use crate::stt::WhisperTranscriber;
use crate::tts::{PiperConfig, PiperSynthesizer};
use protocol::{ServerMsg, wav};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::sync::mpsc;

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// Alternating languages, as a tutor would say it.
const PHRASE: [(Lang, &str); 4] = [
    (Lang::Ru, "Сегодня мы учим выражение"),
    (Lang::En, "around the clock"),
    (Lang::Ru, "то есть круглые сутки. Повтори, пожалуйста"),
    (Lang::En, "I work around the clock"),
];

async fn run_tool(program: &str, args: &[&str], stdin_data: Option<Vec<u8>>) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let mut cmd = Command::new(program);
    cmd.args(args).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    cmd.stdin(if stdin_data.is_some() { Stdio::piped() } else { Stdio::null() });
    let mut child = cmd.spawn().map_err(|e| format!("cannot start {program}: {e}"))?;
    if let (Some(data), Some(mut stdin)) = (stdin_data, child.stdin.take()) {
        stdin.write_all(&data).await?;
    }
    let out = child.wait_with_output().await?;
    if !out.status.success() {
        return Err(format!("{program} failed: {}", String::from_utf8_lossy(&out.stderr)).into());
    }
    Ok(out.stdout)
}

/// Plays a WAV file through ALSA and waits until it has finished.
async fn play(wav_bytes: Vec<u8>) -> TestResult {
    if std::env::var_os("SPEECH_TRAINER_NO_PLAYBACK").is_some() {
        return Ok(());
    }
    run_tool("aplay", &["-q", "-"], Some(wav_bytes)).await?;
    Ok(())
}

fn normalise(text: &str) -> String {
    text.to_lowercase().chars().map(|c| if c.is_alphanumeric() { c } else { ' ' }).collect::<String>().split_whitespace().collect::<Vec<_>>().join(" ")
}

struct FixedTutor(&'static str);

impl Tutor for FixedTutor {
    async fn ask(&mut self, _user_text: &str) -> Result<String, BackendError> {
        Ok(self.0.to_string())
    }
}

fn model_path() -> PathBuf {
    std::env::var("SPEECH_TRAINER_MODEL").map(PathBuf::from).unwrap_or_else(|_| {
        PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".cache/speech-trainer/ggml-small.en.bin")
    })
}

#[tokio::test]
#[ignore = "needs the Whisper model, Piper with both voices, ffmpeg and a sound device"]
async fn mixed_russian_english_phrase_goes_through_the_whole_audio_loop() -> TestResult {
    let tts = PiperSynthesizer::start(&PiperConfig::from_env());

    // 1. Speak the phrase and join the pieces with short pauses.
    let mut samples: Vec<i16> = Vec::new();
    let mut rate = 0;
    for (lang, text) in PHRASE {
        let (r, piece) = wav::decode_pcm16_mono(&tts.synthesize(lang, text).await?)?;
        assert!(rate == 0 || rate == r, "voices must share a sample rate ({rate} vs {r})");
        rate = r;
        assert!(piece.iter().any(|s| s.abs() > 500), "{lang:?} piece {text:?} is silent");
        samples.extend(piece);
        samples.extend(std::iter::repeat_n(0i16, rate as usize * 35 / 100));
    }
    let seconds = samples.len() as f32 / rate as f32;
    println!("spoken phrase: {seconds:.1} s at {rate} Hz");

    // 2. Save it as mp3.
    let mp3 = run_tool(
        "ffmpeg",
        &["-hide_banner", "-loglevel", "error", "-f", "wav", "-i", "pipe:0", "-codec:a", "libmp3lame", "-q:a", "4", "-f", "mp3", "pipe:1"],
        Some(wav::encode_pcm16_mono(&samples, rate)),
    )
    .await?;
    assert!(mp3.len() > 10_000, "mp3 is only {} bytes", mp3.len());
    let mp3_path = std::env::temp_dir().join("speech-trainer-mixed-ru-en.mp3");
    std::fs::write(&mp3_path, &mp3)?;
    println!("saved {} ({} bytes)", mp3_path.display(), mp3.len());

    // 3. Decode the mp3 back to what the recogniser expects: 16 kHz mono.
    let pcm = run_tool(
        "ffmpeg",
        &["-hide_banner", "-loglevel", "error", "-i", path_str(&mp3_path)?, "-f", "s16le", "-ar", "16000", "-ac", "1", "pipe:1"],
        None,
    )
    .await?;
    let decoded = wav::pcm16_from_bytes(&pcm);
    assert!((decoded.len() as f32 / 16_000.0 - seconds).abs() < 0.5, "decoded length differs from the spoken one");

    // 4. Play the mp3 audio.
    println!("playing the mp3...");
    play(wav::encode_pcm16_mono(&decoded, 16_000)).await?;

    // 5. Recognise it.
    let model = model_path();
    assert!(model.is_file(), "Whisper model not found at {}", model.display());
    let stt = Arc::new(WhisperTranscriber::load(&model, "en")?);
    let heard = crate::backend::Transcriber::transcribe(stt.as_ref(), wav::encode_pcm16_mono(&decoded, 16_000)).await?;
    println!("whisper heard: {heard:?}");
    assert!(normalise(&heard).contains("around the clock"), "the English part was not recognised: {heard:?}");

    // 6. The same audio as a learner's utterance through a whole server turn.
    let tutor = FixedTutor("<say lang=\"ru\">Ты сказал это по-английски. Отлично!</say><say lang=\"en\">Well done. That is how a native speaker says it.</say>");
    let dialog = crate::dialog::parse(include_str!("../tests/fixtures/16_the_job_interview.md"))?;
    let (tx, mut rx) = mpsc::channel(64);
    let mut session = Session::new(dialog, tutor, stt, Arc::new(tts), tx);
    session.on_utterance(&pcm).await.map_err(|_| "session closed")?;
    drop(session);

    let mut audio_pieces = 0;
    let mut transcript = String::new();
    let mut finished = false;
    while let Some(out) = rx.recv().await {
        match out {
            Out::Msg(ServerMsg::Transcript { text }) => transcript = text,
            Out::Msg(ServerMsg::TurnDone) => finished = true,
            Out::Msg(ServerMsg::Error { message }) => return Err(message.into()),
            Out::Audio(bytes) => {
                let (_, piece) = wav::decode_pcm16_mono(&bytes)?;
                assert!(piece.iter().any(|s| s.abs() > 500), "the tutor's spoken reply is silent");
                audio_pieces += 1;
                println!("playing the tutor's reply, part {audio_pieces}...");
                play(bytes).await?;
            }
            Out::Msg(_) => {}
        }
    }
    assert!(normalise(&transcript).contains("around the clock"), "turn transcript: {transcript:?}");
    assert_eq!(audio_pieces, 2, "expected one Russian and one English audio part");
    assert!(finished, "the turn never finished");
    Ok(())
}

fn path_str(p: &Path) -> Result<&str, Box<dyn std::error::Error>> {
    p.to_str().ok_or_else(|| "path is not valid UTF-8".into())
}
