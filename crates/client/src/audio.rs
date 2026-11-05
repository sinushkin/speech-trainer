//! Microphone capture and playback through the ALSA command line tools
//! (`arecord`/`aplay`). They work on PulseAudio/PipeWire setups through the
//! default device, resample for us, and need no native audio library.

use anyhow::{Context, Result, anyhow};
use std::process::Stdio;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, ChildStdout, Command};

/// 30 ms of 16 kHz mono audio.
pub const FRAME_SAMPLES: usize = 480;

pub struct Mic {
    // Held so the recorder is killed when the mic is dropped.
    _child: Child,
    stdout: ChildStdout,
}

impl Mic {
    pub fn open(device: Option<&str>) -> Result<Self> {
        let mut cmd = Command::new("arecord");
        cmd.args(["-q", "-f", "S16_LE", "-r", "16000", "-c", "1", "-t", "raw"]);
        if let Some(d) = device {
            cmd.args(["-D", d]);
        }
        cmd.stdout(Stdio::piped()).stderr(Stdio::inherit()).kill_on_drop(true);
        let mut child = cmd.spawn().context("cannot start arecord (is alsa-utils installed?)")?;
        let stdout = child.stdout.take().ok_or_else(|| anyhow!("arecord stdout was not piped"))?;
        Ok(Self { _child: child, stdout })
    }

    /// Reads exactly one frame; `None` when the recorder stopped.
    pub async fn read_frame(&mut self) -> Result<Option<Vec<i16>>> {
        let mut bytes = vec![0u8; FRAME_SAMPLES * 2];
        match self.stdout.read_exact(&mut bytes).await {
            Ok(_) => Ok(Some(protocol::wav::pcm16_from_bytes(&bytes))),
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
}

/// A short "pik": the signal that the microphone is open and you may speak.
/// 880 Hz, 140 ms, with 10 ms fades at both ends so it does not click.
pub fn beep_wav() -> Vec<u8> {
    const RATE: u32 = 16_000;
    const FREQ: f32 = 880.0;
    let n = RATE as usize * 140 / 1000;
    let fade = RATE as usize * 10 / 1000;
    let samples: Vec<i16> = (0..n)
        .map(|i| {
            let gain = (i.min(n - 1 - i) as f32 / fade as f32).min(1.0);
            let wave = (2.0 * std::f32::consts::PI * FREQ * i as f32 / RATE as f32).sin();
            (wave * gain * 0.35 * f32::from(i16::MAX)) as i16
        })
        .collect();
    protocol::wav::encode_pcm16_mono(&samples, RATE)
}

/// Plays one complete WAV file and waits until it finished.
pub async fn play_wav(wav: &[u8], device: Option<&str>) -> Result<()> {
    let mut cmd = Command::new("aplay");
    cmd.arg("-q");
    if let Some(d) = device {
        cmd.args(["-D", d]);
    }
    cmd.arg("-").stdin(Stdio::piped()).stderr(Stdio::inherit()).kill_on_drop(true);
    let mut child = cmd.spawn().context("cannot start aplay (is alsa-utils installed?)")?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(wav).await.context("cannot write audio to aplay")?;
        // Dropping stdin closes the pipe so aplay sees the end of the file.
    }
    let status = child.wait().await?;
    if !status.success() {
        return Err(anyhow!("aplay exited with {status}"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_beep_is_a_short_audible_880_hz_tone_without_clicks() -> Result<(), protocol::wav::WavError> {
        let (rate, samples) = protocol::wav::decode_pcm16_mono(&beep_wav())?;
        assert_eq!(rate, 16_000);
        assert_eq!(samples.len(), 2_240, "140 ms");
        let peak = samples.iter().map(|s| s.unsigned_abs()).max().unwrap_or(0);
        assert!((8_000..16_000).contains(&peak), "audible but not loud, peak {peak}");
        assert!(samples[0].abs() < 300 && samples[samples.len() - 1].abs() < 300, "fades in and out");
        let crossings = samples.windows(2).filter(|w| (w[0] < 0) != (w[1] < 0)).count();
        // 880 Hz for 0.14 s is 123 periods, so about 246 sign changes.
        assert!((235..=255).contains(&crossings), "pitch off, {crossings} zero crossings");
        Ok(())
    }
}
