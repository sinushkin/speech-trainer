//! speech-trainer client: listens to the microphone, cuts speech into
//! utterances with a voice-activity detector, sends them to the server and
//! plays back the tutor's audio. The mic is closed while the tutor speaks.
//!
//! Usage: see `cli::USAGE` (`client --help`). Environment: SPEECH_TRAINER_URL (default ws://127.0.0.1:8765/ws),
//!   SPEECH_TRAINER_ARECORD_DEVICE, SPEECH_TRAINER_APLAY_DEVICE.

mod audio;
mod cli;
mod vad;

use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use protocol::{ClientMsg, ServerMsg, wav};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio_tungstenite::tungstenite::Message;

fn env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}

fn show(msg: &ServerMsg) {
    match msg {
        ServerMsg::Ready { dialogs } => println!("== session started ({dialogs} dialogs available; say \"another dialog\" to switch)"),
        ServerMsg::Transcript { text } => println!("you:   {text}"),
        ServerMsg::Reply { text } => println!("tutor: {text}"),
        ServerMsg::Link { url, label } => println!("watch: {label}  {url}"),
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

    let (ws, _) = tokio_tungstenite::connect_async(&url).await.with_context(|| format!("cannot connect to {url}"))?;
    let (mut sink, mut stream) = ws.split();
    let hello = serde_json::to_string(&ClientMsg::Hello { dialog })?;
    sink.send(Message::Text(hello.into())).await?;

    // Frames from the microphone (or typed lines in --text mode) arrive here.
    let (mic_tx, mut mic_rx) = tokio::sync::mpsc::channel::<Vec<i16>>(256);
    let (line_tx, mut line_rx) = tokio::sync::mpsc::channel::<String>(8);
    if text_mode {
        tokio::spawn(async move {
            let mut lines = BufReader::new(tokio::io::stdin()).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if line_tx.send(line).await.is_err() {
                    break;
                }
            }
        });
    } else {
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

    let mut vad = vad::Vad::new(vad::VadConfig {
        trailing_silence_ms: args.pause_ms,
        max_utterance_ms: args.max_utterance_sec * 1000,
        ..vad::VadConfig::default()
    });
    let mut listening = false; // false until the tutor's first turn is done
    if !text_mode {
        println!(
            "calibrating the microphone, stay quiet for a moment... (an answer ends after {:.1} s of silence; --pause-ms changes that)",
            f64::from(args.pause_ms) / 1000.0
        );
    }

    loop {
        tokio::select! {
            incoming = stream.next() => {
                let Some(incoming) = incoming else { break };
                match incoming.context("connection error")? {
                    Message::Text(t) => match serde_json::from_str::<ServerMsg>(&t) {
                        Ok(msg) => {
                            show(&msg);
                            if matches!(msg, ServerMsg::SessionEnded { .. }) {
                                break;
                            }
                            if matches!(msg, ServerMsg::TurnDone) {
                                while mic_rx.try_recv().is_ok() {} // stale audio from while the tutor spoke
                                vad.reset();
                                listening = true;
                                if !text_mode { println!("(your turn, speak)"); }
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
            frame = mic_rx.recv(), if !text_mode => {
                let Some(frame) = frame else { break };
                if !listening {
                    // The noise floor is measured while the tutor is still thinking.
                    vad.calibrate(&frame);
                    continue;
                }
                if let Some(utterance) = vad.feed(&frame) {
                    listening = false;
                    sink.send(Message::Binary(wav::pcm16_to_bytes(&utterance).into())).await?;
                }
            }
            line = line_rx.recv(), if text_mode => {
                let Some(line) = line else { break };
                if !listening || line.trim().is_empty() { continue; }
                listening = false;
                let msg = serde_json::to_string(&ClientMsg::Text { text: line })?;
                sink.send(Message::Text(msg.into())).await?;
            }
        }
    }
    Ok(())
}
