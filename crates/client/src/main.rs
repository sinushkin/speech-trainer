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
        ServerMsg::Ready { .. } if video => println!("== session started in video mode (say \"command another video\" or \"command next dialog\" to switch)"),
        ServerMsg::Ready { dialogs } => println!("== session started ({dialogs} dialogs available; say \"command next dialog\" or \"command video mode\" to switch)"),
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
        ServerMsg::Paused => println!("(paused: say \"command resume\" or press Enter to go on)"),
        ServerMsg::TurnDone => {}
        ServerMsg::Error { message } => eprintln!("error: {message}"),
        ServerMsg::SessionEnded { reason } => println!("== session ended: {reason}"),
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    protocol::logging::init("client", "warn");

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
    let hello = serde_json::to_string(&ClientMsg::Hello { dialog, video: args.video, review: args.review })?;
    sink.send(Message::Text(hello.into())).await?;

    let mut listening = false; // false until the tutor's first turn is done
    // A video was assigned: the microphone stays closed (the video's sound would
    // be recorded) until the learner presses Enter.
    let mut awaiting_ready = false;
    // "command pause" was said: the microphone stays on, the server answers nothing.
    let mut paused = false;
    let mut stdin_open = true;
    // After a correction the tutor waits a moment for the learner (who may ask
    // "in Russian"); if nobody speaks it goes on by itself.
    let mut silence_window: Option<u32> = None; // announced by the server, armed at turn_done
    let mut continue_deadline: Option<tokio::time::Instant> = None;
    // Once the socket held unread data while the loop sat idle (2026-11-10); a regular
    // tick makes sure the connection is looked at even if a wake-up is ever lost.
    let mut tick = tokio::time::interval(std::time::Duration::from_millis(250));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = tick.tick() => {}
            incoming = stream.next() => {
                let Some(incoming) = incoming else { break };
                match incoming.context("connection error")? {
                    Message::Text(t) => match serde_json::from_str::<ServerMsg>(&t) {
                        Ok(msg) => {
                            tracing::info!("server says: {msg:?}");
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
                            if matches!(msg, ServerMsg::Paused) {
                                // No beep: listen again for "command resume".
                                paused = true;
                                continue_deadline = None;
                                silence_window = None;
                                while mic_rx.try_recv().is_ok() {}
                                vad.reset();
                                listening = true;
                            } else if matches!(msg, ServerMsg::TurnDone | ServerMsg::Error { .. }) {
                                paused = false;
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
                        let started = std::time::Instant::now();
                        tracing::info!("audio part of {} bytes, playing", wav_bytes.len());
                        if let Err(e) = audio::play_wav(&wav_bytes, play_device.as_deref()).await {
                            tracing::warn!("playback failed: {e}");
                        }
                        tracing::info!("audio part played in {:.1} s", started.elapsed().as_secs_f32());
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
                    tracing::info!("silence window over, telling the tutor to go on");
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
                    tracing::info!("sending an utterance of {} ms", utterance.len() * 1000 / protocol::SAMPLE_RATE as usize);
                    sink.send(Message::Binary(wav::pcm16_to_bytes(&utterance).into())).await?;
                }
            }
            line = line_rx.recv(), if stdin_open => {
                let Some(line) = line else {
                    if text_mode { break; }
                    stdin_open = false; // by voice the keyboard is optional
                    continue;
                };
                if paused {
                    // Enter resumes; in text mode anything typed goes to the server as it is.
                    paused = false;
                    listening = false;
                    let text = if line.trim().is_empty() { "command resume".to_string() } else { line };
                    sink.send(Message::Text(serde_json::to_string(&ClientMsg::Text { text })?.into())).await?;
                    continue;
                }
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
