//! Wire protocol shared by the speech-trainer server and client.
//!
//! One WebSocket carries both kinds of frames:
//! * text frames: JSON-encoded [`ClientMsg`] (client to server) and
//!   [`ServerMsg`] (server to client);
//! * binary frames: client to server, one whole utterance as raw PCM16 little
//!   endian, mono, [`SAMPLE_RATE`] Hz; server to client, one complete WAV file
//!   per spoken segment (the header carries its own sample rate).

use serde::{Deserialize, Serialize};

pub mod wav;

/// Sample rate of audio the client sends (what speech recognisers expect).
pub const SAMPLE_RATE: u32 = 16_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMsg {
    /// First message. `dialog` optionally names the dialog to start with (file
    /// stem, e.g. `16_the_job_interview`); without it the tutor chooses.
    Hello {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        dialog: Option<String>,
    },
    /// Typed input that bypasses speech recognition (development and tests).
    Text { text: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMsg {
    /// The session started (the tutor has its own Claude process) and the first
    /// tutor turn follows. `dialogs` is how many dialogs are available.
    Ready { dialogs: usize },
    /// What the recogniser heard from the learner.
    Transcript { text: String },
    /// The tutor's reply as plain text, for display.
    Reply { text: String },
    /// A link to the source video moment, shown after feedback.
    Link { url: String, label: String },
    /// The tutor finished this turn: all audio was sent, the mic may open again.
    TurnDone,
    Error { message: String },
    /// The server ended the session (for example after the idle timeout) and
    /// stopped its tutor; the connection closes next.
    SessionEnded { reason: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_messages_round_trip_as_tagged_json() -> Result<(), serde_json::Error> {
        let msg = ClientMsg::Hello { dialog: Some("16_the_job_interview".into()) };
        let json = serde_json::to_string(&msg)?;
        assert_eq!(json, r#"{"type":"hello","dialog":"16_the_job_interview"}"#);
        assert_eq!(serde_json::from_str::<ClientMsg>(&json)?, msg);
        Ok(())
    }

    #[test]
    fn hello_without_a_dialog_is_valid() -> Result<(), serde_json::Error> {
        assert_eq!(serde_json::from_str::<ClientMsg>(r#"{"type":"hello"}"#)?, ClientMsg::Hello { dialog: None });
        assert_eq!(serde_json::to_string(&ClientMsg::Hello { dialog: None })?, r#"{"type":"hello"}"#);
        Ok(())
    }

    #[test]
    fn server_messages_round_trip_as_tagged_json() -> Result<(), serde_json::Error> {
        for msg in [
            ServerMsg::Ready { dialogs: 45 },
            ServerMsg::Transcript { text: "hello".into() },
            ServerMsg::Link { url: "https://youtu.be/x?t=1".into(), label: "x @ 0:01".into() },
            ServerMsg::TurnDone,
            ServerMsg::Error { message: "boom".into() },
            ServerMsg::SessionEnded { reason: "idle".into() },
        ] {
            let json = serde_json::to_string(&msg)?;
            assert_eq!(serde_json::from_str::<ServerMsg>(&json)?, msg);
        }
        Ok(())
    }
}
