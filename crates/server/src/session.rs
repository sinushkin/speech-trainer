//! One learner's dialog session: turns utterances into tutor turns and the
//! tutor's tagged reply into text, links and audio for the client.

use crate::backend::{Synthesizer, Transcriber, Tutor};
use crate::dialog::Dialog;
use crate::markup::{self, Segment};
use protocol::{SAMPLE_RATE, ServerMsg, wav};
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::mpsc;

/// What the session sends towards the client connection.
#[derive(Debug, PartialEq, Eq)]
pub enum Out {
    Msg(ServerMsg),
    /// One complete WAV file.
    Audio(Vec<u8>),
}

/// The client connection went away; the session should stop.
#[derive(Debug)]
pub struct Closed;

/// Utterances shorter than this are treated as noise and ignored.
const MIN_UTTERANCE_SAMPLES: usize = (SAMPLE_RATE as usize) * 3 / 10;

pub struct Session<T, S, V> {
    dialog: Dialog,
    allowed_urls: HashSet<String>,
    tutor: T,
    stt: Arc<S>,
    tts: Arc<V>,
    out: mpsc::Sender<Out>,
}

impl<T: Tutor, S: Transcriber, V: Synthesizer> Session<T, S, V> {
    pub fn new(dialog: Dialog, tutor: T, stt: Arc<S>, tts: Arc<V>, out: mpsc::Sender<Out>) -> Self {
        let allowed_urls = dialog.cards.iter().map(|c| c.url.clone()).collect();
        Self { dialog, allowed_urls, tutor, stt, tts, out }
    }

    async fn emit(&self, o: Out) -> Result<(), Closed> {
        self.out.send(o).await.map_err(|_| Closed)
    }

    async fn msg(&self, m: ServerMsg) -> Result<(), Closed> {
        self.emit(Out::Msg(m)).await
    }

    /// Announces the dialog and lets the tutor open the lesson.
    pub async fn begin(&mut self) -> Result<(), Closed> {
        self.msg(ServerMsg::Ready { title: self.dialog.title.clone(), cards: self.dialog.cards.len() }).await?;
        self.run_turn("Begin the lesson.").await
    }

    /// `pcm` is one utterance: PCM16 little endian, mono, 16 kHz.
    pub async fn on_utterance(&mut self, pcm: &[u8]) -> Result<(), Closed> {
        let samples = wav::pcm16_from_bytes(pcm);
        if samples.len() < MIN_UTTERANCE_SAMPLES {
            return self.msg(ServerMsg::TurnDone).await;
        }
        let heard = self.stt.transcribe(wav::encode_pcm16_mono(&samples, SAMPLE_RATE)).await;
        match heard {
            Ok(text) if text.trim().is_empty() => self.msg(ServerMsg::TurnDone).await,
            Ok(text) => self.on_text(&text).await,
            Err(e) => self.fail(format!("speech recognition failed: {e}")).await,
        }
    }

    /// Learner input that is already text (recognised or typed).
    pub async fn on_text(&mut self, text: &str) -> Result<(), Closed> {
        self.msg(ServerMsg::Transcript { text: text.to_string() }).await?;
        self.run_turn(text).await
    }

    async fn fail(&self, message: String) -> Result<(), Closed> {
        tracing::warn!("{message}");
        self.msg(ServerMsg::Error { message }).await?;
        self.msg(ServerMsg::TurnDone).await
    }

    async fn run_turn(&mut self, user_text: &str) -> Result<(), Closed> {
        let raw = match self.tutor.ask(user_text).await {
            Ok(r) => r,
            Err(e) => return self.fail(format!("tutor failed: {e}")).await,
        };
        let segments = markup::parse_reply(&raw);
        self.msg(ServerMsg::Reply { text: markup::display_text(&segments) }).await?;
        for segment in segments {
            match segment {
                Segment::Say { lang, text } => match self.tts.synthesize(lang, &text).await {
                    Ok(audio) => self.emit(Out::Audio(audio)).await?,
                    // A missing voice must not end the lesson: the text was already sent.
                    Err(e) => tracing::warn!("speech synthesis ({}) failed: {e}", lang.code()),
                },
                Segment::Link { url, label } => {
                    if self.allowed_urls.contains(&url) {
                        self.msg(ServerMsg::Link { url, label }).await?;
                    } else {
                        tracing::warn!("dropping link not present on any card: {url}");
                    }
                }
            }
        }
        self.msg(ServerMsg::TurnDone).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::BackendError;
    use crate::dialog::parse;
    use crate::markup::Lang;
    use std::sync::Mutex;

    struct FakeTutor {
        replies: Vec<Result<String, String>>,
        heard: Arc<Mutex<Vec<String>>>,
    }

    impl Tutor for FakeTutor {
        async fn ask(&mut self, user_text: &str) -> Result<String, BackendError> {
            if let Ok(mut h) = self.heard.lock() {
                h.push(user_text.to_string());
            }
            if self.replies.is_empty() {
                return Err(BackendError::Exited);
            }
            self.replies.remove(0).map_err(BackendError::Failed)
        }
    }

    struct FakeStt(&'static str);
    impl Transcriber for FakeStt {
        async fn transcribe(&self, wav: Vec<u8>) -> Result<String, BackendError> {
            assert_eq!(&wav[0..4], b"RIFF");
            Ok(self.0.to_string())
        }
    }

    struct FakeTts;
    impl Synthesizer for FakeTts {
        async fn synthesize(&self, lang: Lang, text: &str) -> Result<Vec<u8>, BackendError> {
            if lang == Lang::Ru && text.contains("FAIL") {
                return Err(BackendError::Failed("no ru voice".into()));
            }
            Ok(wav::encode_pcm16_mono(&[0i16; 8], 22_050))
        }
    }

    const FIXTURE: &str = include_str!("../tests/fixtures/16_the_job_interview.md");

    struct Rig {
        session: Session<FakeTutor, FakeStt, FakeTts>,
        rx: mpsc::Receiver<Out>,
        heard: Arc<Mutex<Vec<String>>>,
        url: String,
    }

    fn rig(replies: Vec<Result<String, String>>, stt_text: &'static str) -> Result<Rig, crate::dialog::DialogError> {
        let dialog = parse(FIXTURE)?;
        let url = dialog.cards[0].url.clone();
        let (tx, rx) = mpsc::channel(64);
        let heard = Arc::new(Mutex::new(Vec::new()));
        let tutor = FakeTutor { replies, heard: heard.clone() };
        let session = Session::new(dialog, tutor, Arc::new(FakeStt(stt_text)), Arc::new(FakeTts), tx);
        Ok(Rig { session, rx, heard, url })
    }

    fn drain(rx: &mut mpsc::Receiver<Out>) -> Vec<Out> {
        let mut v = Vec::new();
        while let Ok(o) = rx.try_recv() {
            v.push(o);
        }
        v
    }

    fn kinds(outs: &[Out]) -> Vec<&'static str> {
        outs.iter()
            .map(|o| match o {
                Out::Audio(_) => "audio",
                Out::Msg(ServerMsg::Ready { .. }) => "ready",
                Out::Msg(ServerMsg::Reply { .. }) => "reply",
                Out::Msg(ServerMsg::Transcript { .. }) => "transcript",
                Out::Msg(ServerMsg::Link { .. }) => "link",
                Out::Msg(ServerMsg::TurnDone) => "done",
                Out::Msg(ServerMsg::Error { .. }) => "error",
            })
            .collect()
    }

    #[tokio::test]
    async fn the_lesson_opens_with_ready_reply_audio_and_done() -> Result<(), Box<dyn std::error::Error>> {
        let reply = "<say lang=\"en\">Hi! Question one.</say><say lang=\"ru\">Скажи про работу.</say>".to_string();
        let mut r = rig(vec![Ok(reply)], "")?;
        r.session.begin().await.map_err(|_| "closed")?;
        let outs = drain(&mut r.rx);
        assert_eq!(kinds(&outs), vec!["ready", "reply", "audio", "audio", "done"]);
        assert_eq!(outs[1], Out::Msg(ServerMsg::Reply { text: "Hi! Question one. Скажи про работу.".into() }));
        assert_eq!(r.heard.lock().map_err(|_| "poisoned")?.as_slice(), ["Begin the lesson."]);
        Ok(())
    }

    #[tokio::test]
    async fn an_utterance_is_transcribed_then_sent_to_the_tutor() -> Result<(), Box<dyn std::error::Error>> {
        let reply = format!("<say lang=\"en\">Nice.</say><link url=\"{}\">label</link>", "PLACEHOLDER");
        let mut r = rig(vec![Ok(reply.replace("PLACEHOLDER", "x"))], "I work around the clock")?;
        let pcm = wav::pcm16_to_bytes(&vec![100i16; 8_000]);
        r.session.on_utterance(&pcm).await.map_err(|_| "closed")?;
        let outs = drain(&mut r.rx);
        // The link url is not on any card, so it is dropped.
        assert_eq!(kinds(&outs), vec!["transcript", "reply", "audio", "done"]);
        assert_eq!(outs[0], Out::Msg(ServerMsg::Transcript { text: "I work around the clock".into() }));
        assert_eq!(r.heard.lock().map_err(|_| "poisoned")?.as_slice(), ["I work around the clock"]);
        Ok(())
    }

    #[tokio::test]
    async fn links_that_match_a_card_are_forwarded() -> Result<(), Box<dyn std::error::Error>> {
        let mut r = rig(vec![], "")?;
        r.session.tutor.replies = vec![Ok(format!("<say lang=\"en\">Good.</say><link url=\"{}\">lbl</link>", r.url))];
        r.session.on_text("hi").await.map_err(|_| "closed")?;
        let outs = drain(&mut r.rx);
        assert_eq!(kinds(&outs), vec!["transcript", "reply", "audio", "link", "done"]);
        Ok(())
    }

    #[tokio::test]
    async fn noise_and_silence_do_not_reach_the_tutor() -> Result<(), Box<dyn std::error::Error>> {
        let mut r = rig(vec![], "   ")?;
        r.session.on_utterance(&wav::pcm16_to_bytes(&[5i16; 100])).await.map_err(|_| "closed")?; // too short
        r.session.on_utterance(&wav::pcm16_to_bytes(&vec![5i16; 8_000])).await.map_err(|_| "closed")?; // empty text
        assert_eq!(kinds(&drain(&mut r.rx)), vec!["done", "done"]);
        assert!(r.heard.lock().map_err(|_| "poisoned")?.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn tutor_failure_reports_error_and_releases_the_mic() -> Result<(), Box<dyn std::error::Error>> {
        let mut r = rig(vec![Err("rate limit".into())], "")?;
        r.session.on_text("hello").await.map_err(|_| "closed")?;
        assert_eq!(kinds(&drain(&mut r.rx)), vec!["transcript", "error", "done"]);
        Ok(())
    }

    #[tokio::test]
    async fn a_missing_voice_skips_the_audio_but_not_the_turn() -> Result<(), Box<dyn std::error::Error>> {
        let mut r = rig(vec![Ok("<say lang=\"ru\">FAIL это</say><say lang=\"en\">ok</say>".into())], "")?;
        r.session.on_text("x").await.map_err(|_| "closed")?;
        assert_eq!(kinds(&drain(&mut r.rx)), vec!["transcript", "reply", "audio", "done"]);
        Ok(())
    }

    #[tokio::test]
    async fn a_closed_connection_stops_the_session() -> Result<(), Box<dyn std::error::Error>> {
        let mut r = rig(vec![Ok("hello".into())], "")?;
        drop(r.rx);
        assert!(r.session.on_text("x").await.is_err());
        Ok(())
    }
}
