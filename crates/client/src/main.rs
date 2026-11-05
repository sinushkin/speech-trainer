//! speech-trainer client: listens to the microphone, cuts speech into
//! utterances with a voice-activity detector, sends them to the server and
//! plays back the tutor's audio. The mic is closed while the tutor speaks.
//!
//! Usage: see `cli::USAGE` (`client --help`). Environment: SPEECH_TRAINER_URL (default ws://127.0.0.1:8765/ws),
//!   SPEECH_TRAINER_ARECORD_DEVICE, SPEECH_TRAINER_APLAY_DEVICE.

mod audio;
mod calibrate;
mod cli;
mod vad;

use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use protocol::{ClientMsg, ServerMsg, wav};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio_tungstenite::tungstenite::Message;

/// What the tutor is told when you press Enter after watching a video.
const READY_MESSAGE: &str = "I have watched the video. I am ready.";

fn env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}

/// Resolves at `deadline`; never resolves when there is none.
async fn wait_until(deadline: Option<tokio::time::Instant>) {
    match deadline {
        Some(d) => tokio::time::sleep_until(d).await,
        None => std::future::pending::<()>().await,
    }
}

fn show(msg: &ServerMsg, video: bool) {
    match msg {
        ServerMsg::Ready { .. } if video => println!("== session started in video mode (say \"another video\" or \"next dialog\" to switch)"),
        ServerMsg::Ready { dialogs } => println!("== session started ({dialogs} dialogs available; say \"next dialog\" or \"video mode\" to switch)"),
        ServerMsg::Transcript { text } => println!("you:   {text}"),
        ServerMsg::Reply { text } => println!("tutor: {text}"),
        ServerMsg::Link { url, label } => println!("watch: {label}  {url}"),
        ServerMsg::Watch { url, label } => {
            println!("video: {label}\n       {url}");
            println!("(watch it on YouTube, then press Enter here to start the training)");
        }
        ServerMsg::ContinueIfSilent { after_ms } => {
            println!("(say \"in Russian\" within {:.0} s for the Russian version, or stay silent to go on)", f64::from(*after_ms) / 1000.0);
        }
        ServerMsg::TurnDone => {}
        ServerMsg::Error { message } => eprintln!("error: {message}"),
        ServerMsg::SessionEnded { reason } => println!("== session ended: {reason}"),
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into()))
        .init();

    let raw: Vec<String> = std::env::args().skip(1).collect();
    let args = match cli::parse(&raw, env).map_err(|e| anyhow::anyhow!("{e}\n{}", cli::USAGE))? {
        cli::Parsed::Help => {
            println!("{}", cli::USAGE);
            return Ok(());
        }
        cli::Parsed::Run(args) => args,
    };
    let text_mode = args.text_mode;
    let dialog = args.dialog;
    let url = env("SPEECH_TRAINER_URL").unwrap_or_else(|| "ws://127.0.0.1:8765/ws".into());
    let play_device = env("SPEECH_TRAINER_APLAY_DEVICE");

    // Frames from the microphone (or typed lines in --text mode) arrive here.
    let (mic_tx, mut mic_rx) = tokio::sync::mpsc::channel::<Vec<i16>>(256);
    let (line_tx, mut line_rx) = tokio::sync::mpsc::channel::<String>(8);
    // The keyboard: answers in text mode, and the Enter key after watching a video.
    tokio::spawn(async move {
        let mut lines = BufReader::new(tokio::io::stdin()).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if line_tx.send(line).await.is_err() {
                break;
            }
        }
    });
    if !text_mode {
        let mut mic = audio::Mic::open(env("SPEECH_TRAINER_ARECORD_DEVICE").as_deref())?;
        tokio::spawn(async move {
            loop {
                match mic.read_frame().await {
                    Ok(Some(frame)) => {
                        // When the main loop is busy (tutor speaking) old frames are dropped.
                        if mic_tx.try_send(frame).is_err() && mic_tx.is_closed() {
                            break;
                        }
                    }
                    Ok(None) => break,
                    Err(e) => {
                        tracing::warn!("microphone error: {e}");
                        break;
                    }
                }
            }
        });
    }

    let beep = audio::beep_wav();
    let vad_cfg = vad::VadConfig {
        trailing_silence_ms: args.pause_ms,
        max_utterance_ms: args.max_utterance_sec * 1000,
        ..vad::VadConfig::default()
    };
    // The microphone check comes before the connection, so the session clock
    // (and a tutor process) does not start while the learner tunes the setup.
    let mut vad = if text_mode || !args.calibrate {
        vad::Vad::new(vad_cfg)
    } else {
        calibrate::run(vad_cfg, &mut mic_rx, &mut line_rx, play_device.as_deref(), &beep).await?
    };

    let (ws, _) = tokio_tungstenite::connect_async(&url).await.with_context(|| format!("cannot connect to {url}"))?;
    let (mut sink, mut stream) = ws.split();
    let hello = serde_json::to_string(&ClientMsg::Hello { dialog, video: args.video })?;
    sink.send(Message::Text(hello.into())).await?;

    let mut listening = false; // false until the tutor's first turn is done
    // A video was assigned: the microphone stays closed (the video's sound would
    // be recorded) until the learner presses Enter.
    let mut awaiting_ready = false;
    let mut stdin_open = true;
    // After a correction the tutor waits a moment for the learner (who may ask
    // "in Russian"); if nobody speaks it goes on by itself.
    let mut silence_window: Option<u32> = None; // announced by the server, armed at turn_done
    let mut continue_deadline: Option<tokio::time::Instant> = None;
    loop {
        tokio::select! {
            incoming = stream.next() => {
                let Some(incoming) = incoming else { break };
                match incoming.context("connection error")? {
                    Message::Text(t) => match serde_json::from_str::<ServerMsg>(&t) {
                        Ok(msg) => {
                            show(&msg, args.video);
                            if matches!(msg, ServerMsg::SessionEnded { .. }) {
                                break;
                            }
                            if let ServerMsg::ContinueIfSilent { after_ms } = msg {
                                silence_window = Some(after_ms);
                            }
                            if matches!(msg, ServerMsg::Watch { .. }) {
                                awaiting_ready = true;
                            }
                            if matches!(msg, ServerMsg::TurnDone) && awaiting_ready {
                                // No beep and no microphone until Enter.
                                continue_deadline = None;
                                silence_window = None;
                            } else if matches!(msg, ServerMsg::TurnDone) {
                                // The beep says "you can speak now". It comes first, so that
                                // the microphone does not pick it up: older audio is dropped after it.
                                if !text_mode
                                    && let Err(e) = audio::play_wav(&beep, play_device.as_deref()).await {
                                        tracing::warn!("beep failed: {e}");
                                    }
                                while mic_rx.try_recv().is_ok() {} // stale audio from while the tutor spoke
                                vad.reset();
                                listening = true;
                                // Typing takes longer than a speech window, so text mode never auto-continues.
                                continue_deadline = silence_window
                                    .take()
                                    .filter(|_| !text_mode)
                                    .map(|ms| tokio::time::Instant::now() + std::time::Duration::from_millis(u64::from(ms)));
                                println!("({})", if text_mode { "your turn, type your answer" } else { "your turn, speak" });
                            }
                        }
                        Err(e) => tracing::warn!("unreadable server message: {e}"),
                    },
                    Message::Binary(wav_bytes) => {
                        if let Err(e) = audio::play_wav(&wav_bytes, play_device.as_deref()).await {
                            tracing::warn!("playback failed: {e}");
                        }
                    }
                    Message::Close(_) => break,
                    _ => {}
                }
            }
            () = wait_until(continue_deadline) => {
                continue_deadline = None;
                // Someone already started speaking: that answer is coming, do not interrupt it.
                if listening && !vad.in_speech() {
                    listening = false;
                    sink.send(Message::Text(serde_json::to_string(&ClientMsg::Continue)?.into())).await?;
                }
            }
            frame = mic_rx.recv(), if !text_mode => {
                let Some(frame) = frame else { break };
                if !listening {
                    // The noise floor is measured while the tutor is still thinking.
                    vad.calibrate(&frame);
                    continue;
                }
                if let Some(utterance) = vad.feed(&frame) {
                    listening = false;
                    continue_deadline = None;
                    sink.send(Message::Binary(wav::pcm16_to_bytes(&utterance).into())).await?;
                }
            }
            line = line_rx.recv(), if stdin_open => {
                let Some(line) = line else {
                    if text_mode { break; }
                    stdin_open = false; // by voice the keyboard is optional
                    continue;
                };
                if awaiting_ready {
                    awaiting_ready = false;
                    listening = false;
                    let text = if line.trim().is_empty() { READY_MESSAGE.to_string() } else { line };
                    sink.send(Message::Text(serde_json::to_string(&ClientMsg::Text { text })?.into())).await?;
                    continue;
                }
                if !text_mode || !listening || line.trim().is_empty() { continue; }
                listening = false;
                // `/continue` plays the part of the silence window in text mode.
                let msg = if line.trim() == "/continue" {
                    serde_json::to_string(&ClientMsg::Continue)?
                } else {
                    serde_json::to_string(&ClientMsg::Text { text: line })?
                };
                sink.send(Message::Text(msg.into())).await?;
            }
        }
    }
    Ok(())
}
