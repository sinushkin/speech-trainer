//! Microphone check at start: stay quiet (the noise floor is measured), then say
//! something after the beep. What was heard is played back, so that both
//! directions are confirmed: the learner hears the speakers and the program
//! hears the microphone. The check repeats until the learner is satisfied.

use crate::audio;
use crate::vad::{Vad, VadConfig, rms};
use anyhow::{Context, Result, anyhow};
use std::time::Duration;
use tokio::sync::mpsc::Receiver;

/// One second of 30 ms frames to measure the background noise.
const QUIET_FRAMES: usize = 33;
/// How long to wait for the learner to say something.
const SPEAK_TIMEOUT: Duration = Duration::from_secs(10);

/// Loudest 30 ms of the phrase.
pub fn peak_level(utterance: &[i16]) -> f32 {
    utterance.chunks(audio::FRAME_SAMPLES).map(rms).fold(0.0, f32::max)
}

/// The speech should clearly exceed the threshold, not scrape past it.
pub fn is_too_quiet(peak: f32, threshold: f32) -> bool {
    peak < threshold * 1.5
}

fn drain<T>(rx: &mut Receiver<T>) {
    while rx.try_recv().is_ok() {}
}

/// Runs the check and returns the detector, calibrated to the room.
pub async fn run(
    cfg: VadConfig,
    frames: &mut Receiver<Vec<i16>>,
    lines: &mut Receiver<String>,
    play_device: Option<&str>,
    beep: &[u8],
) -> Result<Vad> {
    loop {
        let mut vad = Vad::new(VadConfig { calibration_frames: QUIET_FRAMES, ..cfg.clone() });
        println!("microphone check 1/2: stay quiet for a second...");
        drain(frames);
        let threshold = loop {
            if let Some(t) = vad.threshold() {
                break t;
            }
            let frame = frames.recv().await.ok_or_else(|| anyhow!("the microphone stopped"))?;
            vad.calibrate(&frame);
        };
        println!("  background noise measured (speech threshold {threshold:.0})");

        audio::play_wav(beep, play_device).await.context("the beep did not play: check the speakers (SPEECH_TRAINER_APLAY_DEVICE)")?;
        drain(frames);
        println!("microphone check 2/2: say something, for example \"Hello, can you hear me?\"");
        let heard = tokio::time::timeout(SPEAK_TIMEOUT, async {
            loop {
                let frame = frames.recv().await.ok_or_else(|| anyhow!("the microphone stopped"))?;
                if let Some(utterance) = vad.feed(&frame) {
                    return Ok::<_, anyhow::Error>(utterance);
                }
            }
        })
        .await;

        let (heard_ok, accept_hint) = match heard {
            Ok(utterance) => {
                let utterance = utterance?;
                let peak = peak_level(&utterance);
                if is_too_quiet(peak, threshold) {
                    println!("  I heard you, but quietly (level {peak:.0}, threshold {threshold:.0}): speak louder or move closer");
                } else {
                    println!("  I hear you well (level {peak:.0}, threshold {threshold:.0}). Playing it back:");
                }
                audio::play_wav(&protocol::wav::encode_pcm16_mono(&utterance, protocol::SAMPLE_RATE), play_device).await?;
                (true, "Enter - all good, r + Enter - check again")
            }
            Err(_) => {
                println!("  I heard nothing within {} s: check the microphone (SPEECH_TRAINER_ARECORD_DEVICE)", SPEAK_TIMEOUT.as_secs());
                (false, "Enter - try again, s + Enter - go on without a check")
            }
        };
        println!("{accept_hint}");
        drain(frames);
        drain(lines);
        let answer = lines.recv().await.unwrap_or_default();
        let answer = answer.trim().to_lowercase();
        match (heard_ok, answer.as_str()) {
            (true, "r") | (false, "") => {}
            _ => {
                vad.reset();
                return Ok(vad);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn constant(amplitude: i16, n: usize) -> Vec<i16> {
        (0..n).map(|i| if i % 2 == 0 { amplitude } else { -amplitude }).collect()
    }

    #[test]
    fn the_peak_is_the_loudest_frame_not_the_average() {
        let mut phrase = constant(100, 480);
        phrase.extend(constant(4_000, 480));
        phrase.extend(constant(100, 480));
        let peak = peak_level(&phrase);
        assert!((3_990.0..4_010.0).contains(&peak), "{peak}");
        assert_eq!(peak_level(&[]), 0.0);
    }

    #[test]
    fn speech_barely_above_the_threshold_counts_as_too_quiet() {
        assert!(is_too_quiet(130.0, 120.0));
        assert!(!is_too_quiet(1_500.0, 120.0));
    }
}
