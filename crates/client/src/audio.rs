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
