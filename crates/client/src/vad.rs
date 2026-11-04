//! Energy-based voice activity detection. Same idea and defaults as
//! LangTeacher's `capture_until_silence`: calibrate a noise floor from the first
//! frames, treat louder frames as speech, end the utterance after trailing
//! silence, drop blips, and cap the length.

use std::collections::VecDeque;

#[derive(Debug, Clone)]
pub struct VadConfig {
    pub sample_rate: u32,
    /// Frames quieter than this never count as speech.
    pub silence_rms_floor: f32,
    /// Speech must exceed the calibrated noise floor times this factor.
    pub noise_factor: f32,
    pub trailing_silence_ms: u32,
    pub min_utterance_ms: u32,
    pub max_utterance_ms: u32,
    /// Audio kept from before the detected start, so the first word is not clipped.
    pub preroll_ms: u32,
    /// How many initial frames are used to measure the noise floor.
    pub calibration_frames: usize,
}

impl Default for VadConfig {
    fn default() -> Self {
        Self {
            sample_rate: protocol::SAMPLE_RATE,
            silence_rms_floor: 120.0,
            noise_factor: 2.5,
            trailing_silence_ms: 2000,
            min_utterance_ms: 300,
            max_utterance_ms: 30_000,
            preroll_ms: 240,
            calibration_frames: 10,
        }
    }
}

pub struct Vad {
    cfg: VadConfig,
    calibration: Vec<f32>,
    threshold: Option<f32>,
    preroll: VecDeque<Vec<i16>>,
    preroll_samples: usize,
    utterance: Vec<i16>,
    in_speech: bool,
    silence_samples: usize,
    voiced_samples: usize,
}

pub fn rms(frame: &[i16]) -> f32 {
    if frame.is_empty() {
        return 0.0;
    }
    let sum: f64 = frame.iter().map(|&s| f64::from(s) * f64::from(s)).sum();
    (sum / frame.len() as f64).sqrt() as f32
}

impl Vad {
    pub fn new(cfg: VadConfig) -> Self {
        Self {
            cfg,
            calibration: Vec::new(),
            threshold: None,
            preroll: VecDeque::new(),
            preroll_samples: 0,
            utterance: Vec::new(),
            in_speech: false,
            silence_samples: 0,
            voiced_samples: 0,
        }
    }

    /// Measures the noise floor from a frame of (assumed) background noise.
    /// Calibrates once enough frames have been seen; later calls do nothing.
    pub fn calibrate(&mut self, frame: &[i16]) {
        if self.threshold.is_some() {
            return;
        }
        self.calibration.push(rms(frame));
        if self.calibration.len() >= self.cfg.calibration_frames {
            let mut sorted = self.calibration.clone();
            sorted.sort_by(|a, b| a.total_cmp(b));
            let median = sorted[sorted.len() / 2];
            self.threshold = Some(self.cfg.silence_rms_floor.max(median * self.cfg.noise_factor));
        }
    }

    fn ms_to_samples(&self, ms: u32) -> usize {
        (self.cfg.sample_rate as usize * ms as usize) / 1000
    }

    /// True while a phrase is being captured (speech has started and not ended yet).
    pub fn in_speech(&self) -> bool {
        self.in_speech
    }

    /// Forget any partial utterance (used while the tutor is speaking). The noise
    /// floor stays calibrated.
    pub fn reset(&mut self) {
        self.preroll.clear();
        self.preroll_samples = 0;
        self.utterance.clear();
        self.in_speech = false;
        self.silence_samples = 0;
        self.voiced_samples = 0;
    }

    /// Feeds one frame; returns a finished utterance when speech ended.
    pub fn feed(&mut self, frame: &[i16]) -> Option<Vec<i16>> {
        let level = rms(frame);
        let Some(threshold) = self.threshold else {
            self.calibrate(frame);
            return None;
        };
        let voiced = level > threshold;

        if !self.in_speech {
            self.preroll.push_back(frame.to_vec());
            self.preroll_samples += frame.len();
            let keep = self.ms_to_samples(self.cfg.preroll_ms);
            while self.preroll_samples > keep + frame.len() {
                if let Some(old) = self.preroll.pop_front() {
                    self.preroll_samples -= old.len();
                }
            }
            if voiced {
                self.in_speech = true;
                self.utterance = self.preroll.drain(..).flatten().collect();
                self.preroll_samples = 0;
                self.silence_samples = 0;
                self.voiced_samples = frame.len();
            }
            return None;
        }

        self.utterance.extend_from_slice(frame);
        if voiced {
            self.silence_samples = 0;
            self.voiced_samples += frame.len();
        } else {
            self.silence_samples += frame.len();
        }
        let ended = self.silence_samples >= self.ms_to_samples(self.cfg.trailing_silence_ms)
            || self.utterance.len() >= self.ms_to_samples(self.cfg.max_utterance_ms);
        if !ended {
            return None;
        }
        let long_enough = self.voiced_samples >= self.ms_to_samples(self.cfg.min_utterance_ms);
        let done = std::mem::take(&mut self.utterance);
        self.reset();
        long_enough.then_some(done)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRAME: usize = 480; // 30 ms at 16 kHz

    /// The quick settings most tests were written for: 0.8 s pause, 15 s cap.
    fn short_pause() -> VadConfig {
        VadConfig { trailing_silence_ms: 800, max_utterance_ms: 15_000, ..VadConfig::default() }
    }

    fn frames(amplitude: i16, ms: usize) -> Vec<Vec<i16>> {
        // Alternating sign keeps rms == |amplitude| for a constant-level signal.
        (0..ms / 30).map(|_| (0..FRAME).map(|i| if i % 2 == 0 { amplitude } else { -amplitude }).collect()).collect()
    }

    fn run(vad: &mut Vad, parts: &[Vec<Vec<i16>>]) -> Vec<Vec<i16>> {
        parts.iter().flatten().filter_map(|f| vad.feed(f)).collect()
    }

    #[test]
    fn rms_of_constant_level_signal() {
        assert!((rms(&[100, -100, 100, -100]) - 100.0).abs() < 0.01);
        assert_eq!(rms(&[]), 0.0);
    }

    #[test]
    fn silence_alone_produces_nothing() {
        let mut vad = Vad::new(short_pause());
        assert!(run(&mut vad, &[frames(20, 3_000)]).is_empty());
    }

    #[test]
    fn a_spoken_phrase_is_cut_out_with_preroll() {
        let mut vad = Vad::new(short_pause());
        let out = run(&mut vad, &[frames(20, 600), frames(3_000, 900), frames(20, 1_200)]);
        assert_eq!(out.len(), 1);
        let ms = out[0].len() * 1000 / 16_000;
        // preroll (240) + speech (900) + trailing silence (~810)
        assert!((1_800..2_100).contains(&ms), "utterance was {ms} ms");
    }

    #[test]
    fn in_speech_is_true_only_while_a_phrase_is_being_captured() {
        let mut vad = Vad::new(short_pause());
        assert!(!vad.in_speech());
        run(&mut vad, &[frames(20, 600)]);
        assert!(!vad.in_speech(), "silence is not speech");
        run(&mut vad, &[frames(3_000, 300)]);
        assert!(vad.in_speech(), "a phrase has started");
        run(&mut vad, &[frames(20, 1_000)]);
        assert!(!vad.in_speech(), "the phrase has ended");
    }

    #[test]
    fn short_blips_are_ignored() {
        let mut vad = Vad::new(short_pause());
        let out = run(&mut vad, &[frames(20, 600), frames(3_000, 90), frames(20, 1_500)]);
        assert!(out.is_empty());
    }

    #[test]
    fn a_stuck_open_mic_is_cut_at_the_maximum_length() {
        let mut vad = Vad::new(short_pause());
        let out = run(&mut vad, &[frames(20, 600), frames(3_000, 16_000)]);
        assert_eq!(out.len(), 1);
        assert!(out[0].len() * 1000 / 16_000 >= 15_000);
    }

    #[test]
    fn constant_background_noise_is_not_speech() {
        // Noise at rms 150 is louder than the floor of 120 but is the calibrated baseline.
        let mut vad = Vad::new(short_pause());
        assert!(run(&mut vad, &[frames(150, 4_000)]).is_empty());
    }

    #[test]
    fn two_phrases_give_two_utterances() {
        let mut vad = Vad::new(short_pause());
        let out = run(&mut vad, &[frames(20, 600), frames(3_000, 600), frames(20, 1_000), frames(3_000, 600), frames(20, 1_000)]);
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn calibration_survives_a_reset() {
        // After a reset speech must be detected at once, not swallowed by a new calibration.
        let mut vad = Vad::new(short_pause());
        run(&mut vad, &[frames(20, 600)]);
        vad.reset();
        let out = run(&mut vad, &[frames(3_000, 600), frames(20, 1_000)]);
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn reset_drops_a_partial_utterance() {
        let mut vad = Vad::new(short_pause());
        run(&mut vad, &[frames(20, 600), frames(3_000, 300)]);
        vad.reset();
        assert!(run(&mut vad, &[frames(20, 2_000)]).is_empty());
    }

    #[test]
    fn by_default_a_thinking_pause_of_one_and_a_half_seconds_stays_inside_one_answer() {
        let mut vad = Vad::new(VadConfig::default());
        let out = run(&mut vad, &[frames(20, 600), frames(3_000, 600), frames(20, 1_500), frames(3_000, 600), frames(20, 2_400)]);
        assert_eq!(out.len(), 1, "one answer expected, got {}", out.len());
        let ms = out[0].len() * 1000 / 16_000;
        assert!(ms > 4_500, "both bursts and the pause must be inside, got {ms} ms");
    }

    #[test]
    fn by_default_two_seconds_of_silence_end_the_answer() {
        let mut vad = Vad::new(VadConfig::default());
        let out = run(&mut vad, &[frames(20, 600), frames(3_000, 600), frames(20, 2_100)]);
        assert_eq!(out.len(), 1);
    }
}
