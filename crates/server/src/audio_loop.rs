//! Unit test of the whole audio loop, without Claude and without a real
//! microphone or speakers:
//!
//! text (Russian and English mixed) -> Piper speech -> mp3 file -> decode ->
//! Whisper recognition -> a server turn (fake tutor) -> spoken reply.
//!
//! It uses the real Whisper model, the Piper binary with both voices and
//! `ffmpeg` (libmp3lame). If any of them is missing the test is skipped with a
//! message; set `SPEECH_TRAINER_REQUIRE_MODELS=1` to make that a failure.
//! Nothing is played: no sound device is touched.

use crate::backend::{BackendError, Synthesizer, Tutor};
use crate::markup::Lang;
use crate::session::{Out, Session};
use crate::stt::WhisperTranscriber;
use crate::tts::{PiperConfig, PiperSynthesizer};
use protocol::{ServerMsg, wav};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Instant;
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

/// Returns why the test cannot run here, or `None` when everything is present.
fn missing_prerequisite(cfg: &PiperConfig) -> Option<String> {
    let model = model_path();
    let voice = |name: &str| cfg.voices_dir.join(format!("{name}.onnx"));
    let checks = [
        (model.is_file(), format!("Whisper model {}", model.display())),
        (cfg.bin.is_file(), format!("Piper binary {}", cfg.bin.display())),
        (voice(&cfg.voice_en).is_file(), format!("voice {}", voice(&cfg.voice_en).display())),
        (voice(&cfg.voice_ru).is_file(), format!("voice {}", voice(&cfg.voice_ru).display())),
        (std::process::Command::new("ffmpeg").arg("-version").stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success()), "ffmpeg in PATH".to_string()),
    ];
    let missing: Vec<String> = checks.into_iter().filter(|(ok, _)| !ok).map(|(_, what)| what).collect();
    (!missing.is_empty()).then(|| missing.join(", "))
}

#[tokio::test]
async fn mixed_russian_english_phrase_goes_through_the_whole_audio_loop() -> TestResult {
    let cfg = PiperConfig::from_env();
    if let Some(missing) = missing_prerequisite(&cfg) {
        assert!(std::env::var_os("SPEECH_TRAINER_REQUIRE_MODELS").is_none(), "missing: {missing}");
        eprintln!("SKIPPED audio loop test, missing: {missing}");
        return Ok(());
    }
    let t0 = Instant::now();
    let mut mark = Instant::now();
    let mut lap = |what: &str, audio_seconds: Option<f32>| {
        let took = mark.elapsed().as_secs_f32();
        match audio_seconds {
            Some(a) if took > 0.0 => println!("[timing] {what}: {took:.2} s for {a:.1} s of audio ({:.1}x real time)", a / took),
            _ => println!("[timing] {what}: {took:.2} s"),
        }
        mark = Instant::now();
    };
    let tts = PiperSynthesizer::start(&cfg);
    lap("start both Piper voices", None);

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
    lap("synthesise 4 pieces", Some(seconds));

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
    lap("encode mp3", Some(seconds));

    // 3. Decode the mp3 back to what the recogniser expects: 16 kHz mono.
    let pcm = run_tool(
        "ffmpeg",
        &["-hide_banner", "-loglevel", "error", "-i", path_str(&mp3_path)?, "-f", "s16le", "-ar", "16000", "-ac", "1", "pipe:1"],
        None,
    )
    .await?;
    let decoded = wav::pcm16_from_bytes(&pcm);
    assert!((decoded.len() as f32 / 16_000.0 - seconds).abs() < 0.5, "decoded length differs from the spoken one");
    lap("decode mp3 to 16 kHz", Some(seconds));

        // 4. Recognise it.
    let stt = Arc::new(WhisperTranscriber::load(&model_path(), "en")?);
    lap("load Whisper model", None);
    let heard = crate::backend::Transcriber::transcribe(stt.as_ref(), wav::encode_pcm16_mono(&decoded, 16_000)).await?;
    println!("whisper heard: {heard:?}");
    lap("recognise", Some(seconds));
    assert!(normalise(&heard).contains("around the clock"), "the English part was not recognised: {heard:?}");

    // 5. The same audio as a learner's utterance through a whole server turn.
    let tutor = FixedTutor("<say lang=\"en\">Ты сказал это по-английски. Отлично! Well done. That is how a native speaker says it.</say>");
    let dialog = crate::dialog::parse(include_str!("../tests/fixtures/16_the_job_interview.md"))?;
    let (tx, mut rx) = mpsc::channel(64);
    let urls = dialog.cards.iter().map(|c| c.url.clone()).collect();
    let mut session = Session::new(Arc::new(urls), tutor, stt, Arc::new(tts), tx, std::time::Duration::from_secs(3), Arc::new(crate::progress::Progress::in_memory()));
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
            }
            Out::Msg(_) => {}
        }
    }
    assert!(normalise(&transcript).contains("around the clock"), "turn transcript: {transcript:?}");
    lap("server turn (recognise again + fake tutor + 2 spoken parts)", Some(seconds));
    println!("[timing] whole test: {:.2} s", t0.elapsed().as_secs_f32());
    assert_eq!(audio_pieces, 2, "one tag with Russian and English text must become a Russian and an English audio part");
    assert!(finished, "the turn never finished");
    Ok(())
}

fn path_str(p: &Path) -> Result<&str, Box<dyn std::error::Error>> {
    p.to_str().ok_or_else(|| "path is not valid UTF-8".into())
}
