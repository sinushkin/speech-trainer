//! Minimal WAV (RIFF, PCM16, mono) encoding and decoding.

use std::fmt;

#[derive(Debug, PartialEq, Eq)]
pub enum WavError {
    TooShort,
    NotRiffWave,
    Unsupported(&'static str),
    NoData,
}

impl fmt::Display for WavError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WavError::TooShort => write!(f, "WAV data is too short"),
            WavError::NotRiffWave => write!(f, "not a RIFF/WAVE file"),
            WavError::Unsupported(what) => write!(f, "unsupported WAV: {what}"),
            WavError::NoData => write!(f, "WAV has no data chunk"),
        }
    }
}

impl std::error::Error for WavError {}

pub fn encode_pcm16_mono(samples: &[i16], sample_rate: u32) -> Vec<u8> {
    let data_len = (samples.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + samples.len() * 2);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&(sample_rate * 2).to_le_bytes()); // byte rate
    out.extend_from_slice(&2u16.to_le_bytes()); // block align
    out.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

/// Parses PCM16 mono WAV, returning `(sample_rate, samples)`.
pub fn decode_pcm16_mono(bytes: &[u8]) -> Result<(u32, Vec<i16>), WavError> {
    if bytes.len() < 12 {
        return Err(WavError::TooShort);
    }
    if &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(WavError::NotRiffWave);
    }
    let mut pos = 12;
    let mut sample_rate = None;
    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let size = u32::from_le_bytes([bytes[pos + 4], bytes[pos + 5], bytes[pos + 6], bytes[pos + 7]]) as usize;
        let body = pos + 8;
        let end = body.saturating_add(size).min(bytes.len());
        if id == b"fmt " {
            if end - body < 16 {
                return Err(WavError::TooShort);
            }
            let fmt = &bytes[body..end];
            let format = u16::from_le_bytes([fmt[0], fmt[1]]);
            let channels = u16::from_le_bytes([fmt[2], fmt[3]]);
            let bits = u16::from_le_bytes([fmt[14], fmt[15]]);
            if format != 1 {
                return Err(WavError::Unsupported("not PCM"));
            }
            if channels != 1 {
                return Err(WavError::Unsupported("not mono"));
            }
            if bits != 16 {
                return Err(WavError::Unsupported("not 16-bit"));
            }
            sample_rate = Some(u32::from_le_bytes([fmt[4], fmt[5], fmt[6], fmt[7]]));
        } else if id == b"data" {
            let rate = sample_rate.ok_or(WavError::Unsupported("data before fmt"))?;
            let samples = bytes[body..end]
                .chunks_exact(2)
                .map(|c| i16::from_le_bytes([c[0], c[1]]))
                .collect();
            return Ok((rate, samples));
        }
        pos = body.saturating_add(size + (size & 1));
    }
    Err(WavError::NoData)
}

/// Reinterprets raw little-endian PCM16 bytes as samples (a trailing odd byte is dropped).
pub fn pcm16_from_bytes(bytes: &[u8]) -> Vec<i16> {
    bytes.chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]])).collect()
}

pub fn pcm16_to_bytes(samples: &[i16]) -> Vec<u8> {
    samples.iter().flat_map(|s| s.to_le_bytes()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_then_decode_returns_the_same_samples() -> Result<(), WavError> {
        let samples: Vec<i16> = vec![0, 1, -1, 1000, -1000, i16::MAX, i16::MIN];
        let wav = encode_pcm16_mono(&samples, 22_050);
        assert_eq!(&wav[0..4], b"RIFF");
        let (rate, decoded) = decode_pcm16_mono(&wav)?;
        assert_eq!(rate, 22_050);
        assert_eq!(decoded, samples);
        Ok(())
    }

    #[test]
    fn decode_rejects_garbage_and_stereo() {
        assert_eq!(decode_pcm16_mono(b"short"), Err(WavError::TooShort));
        assert_eq!(decode_pcm16_mono(b"RIFFxxxxNOPE0000"), Err(WavError::NotRiffWave));
        let mut stereo = encode_pcm16_mono(&[1, 2], 16_000);
        stereo[22] = 2; // channels = 2
        assert_eq!(decode_pcm16_mono(&stereo), Err(WavError::Unsupported("not mono")));
    }

    #[test]
    fn raw_pcm_bytes_round_trip() {
        let samples = vec![-5i16, 300, 0];
        assert_eq!(pcm16_from_bytes(&pcm16_to_bytes(&samples)), samples);
        assert_eq!(pcm16_from_bytes(&[1, 0, 9]), vec![1]);
    }
}
