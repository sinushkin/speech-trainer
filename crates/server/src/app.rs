//! HTTP/WebSocket layer: `GET /dialogs` lists dialogs, `GET /ws` runs a session.
//!
//! Every connection gets its own session: a fresh Claude process (read-only
//! tools, working directory = the dialogs folder) that lives as long as the
//! connection is active. The tutor itself finds and switches dialogs when the
//! learner asks. After `session_timeout` without any frame from the client the
//! session is ended and the process is killed.

use crate::claude::{ClaudeConfig, ClaudeSession};
use crate::instruction::Instruction;
use crate::library::Library;
use crate::progress::Progress;
use crate::prompt::{VideoMode, system_prompt};
use crate::session::{End, Incoming, Out, Session};
use crate::stt::WhisperTranscriber;
use crate::tts::PiperSynthesizer;
use crate::videos::{VideoLibrary, queue_text};
use axum::Router;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use futures_util::stream::SplitStream;
use futures_util::{SinkExt, StreamExt};
use protocol::{ClientMsg, ServerMsg};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Semaphore, mpsc};

/// How many videos are put in a session's random queue.
const VIDEO_QUEUE_LEN: usize = 30;

pub struct AppState {
    pub instruction: Instruction,
    pub library: Library,
    /// `None` when video mode is off (no `videos_dir`, or no usable transcripts).
    pub videos: Option<VideoLibrary>,
    /// Finished dialogs and watched videos, kept between sessions.
    pub progress: Arc<Progress>,
    /// Links that exist on some dialog card or in some video transcript, shared by all sessions.
    pub urls: Arc<std::collections::HashSet<String>>,
    pub claude: ClaudeConfig,
    pub stt: Arc<WhisperTranscriber>,
    pub tts: Arc<PiperSynthesizer>,
    /// One permit per running session (each is a Claude process).
    pub slots: Arc<Semaphore>,
}

impl AppState {
    pub fn new(
        instruction: Instruction,
        library: Library,
        videos: Option<VideoLibrary>,
        progress: Progress,
        claude: ClaudeConfig,
        stt: Arc<WhisperTranscriber>,
        tts: Arc<PiperSynthesizer>,
    ) -> Self {
        let slots = Arc::new(Semaphore::new(instruction.max_sessions));
        let mut urls = library.urls.clone();
        if let Some(v) = &videos {
            urls.extend(v.urls.iter().cloned());
        }
        Self { instruction, library, videos, progress: Arc::new(progress), urls: Arc::new(urls), claude, stt, tts, slots }
    }

    /// The system prompt of one session and the dialog to begin with when the
    /// learner named none: a random one they have not finished yet. The video
    /// queue is shuffled here too and leaves out the videos already watched.
    fn plan(&self) -> (String, Option<String>) {
        use rand::seq::IndexedRandom;
        let mut rng = rand::rng();
        let done = self.progress.done_dialogs(self.library.entries.iter().map(|e| e.name.as_str()));
        let catalogue = self.library.catalogue_marking(&done);
        let fresh: Vec<&str> = self.library.entries.iter().map(|e| e.name.as_str()).filter(|n| !done.contains(*n)).collect();
        let start = fresh.choose(&mut rng).map(|n| n.to_string());
        let prompt = match (&self.videos, &self.instruction.videos_dir) {
            (Some(videos), Some(dir)) => {
                let seen = self.progress.seen_videos(videos.videos.iter().map(|v| v.id.as_str()));
                let text = queue_text(&videos.queue(&mut rng, VIDEO_QUEUE_LEN, &seen));
                system_prompt(&self.instruction.body, &catalogue, Some(VideoMode { transcripts_dir: dir, queue: &text }))
            }
            _ => system_prompt(&self.instruction.body, &catalogue, None),
        };
        (prompt, start)
    }
}

pub fn router(state: Arc<AppState>) -> Router {
    Router::new().route("/dialogs", get(list_dialogs)).route("/ws", get(ws_upgrade)).with_state(state)
}

async fn list_dialogs(State(st): State<Arc<AppState>>) -> Response {
    axum::Json(st.library.entries.iter().map(|e| e.name.clone()).collect::<Vec<_>>()).into_response()
}

async fn ws_upgrade(ws: WebSocketUpgrade, State(st): State<Arc<AppState>>) -> Response {
    ws.on_upgrade(move |socket| handle_socket(socket, st))
}

fn to_incoming(frame: Result<Message, axum::Error>) -> Incoming {
    match frame {
        Ok(Message::Binary(pcm)) => Incoming::Utterance(pcm.to_vec()),
        Ok(Message::Text(t)) => match serde_json::from_str::<ClientMsg>(&t) {
            Ok(ClientMsg::Text { text }) => Incoming::Text(text),
            Ok(ClientMsg::Continue) => Incoming::Continue,
            Ok(ClientMsg::Hello { .. }) => Incoming::Ignore,
            Err(e) => Incoming::Invalid(format!("bad message: {e}")),
        },
        Ok(Message::Close(_)) | Err(_) => Incoming::Close,
        Ok(_) => Incoming::Ignore,
    }
}

async fn handle_socket(socket: WebSocket, st: Arc<AppState>) {
    let (mut sink, stream) = socket.split();
    let (tx, mut rx) = mpsc::channel::<Out>(64);

    let writer = tokio::spawn(async move {
        while let Some(out) = rx.recv().await {
            let msg = match out {
                Out::Audio(bytes) => Message::Binary(bytes.into()),
                Out::Msg(m) => match serde_json::to_string(&m) {
                    Ok(json) => Message::Text(json.into()),
                    Err(e) => {
                        tracing::warn!("cannot encode server message: {e}");
                        continue;
                    }
                },
            };
            if sink.send(msg).await.is_err() {
                break;
            }
        }
    });

    if let Err(message) = run_session(stream, &tx, &st).await {
        tracing::warn!("session failed: {message}");
        notify(&tx, ServerMsg::Error { message }).await;
    }
    drop(tx);
    if let Err(e) = writer.await {
        tracing::warn!("writer task failed: {e}");
    }
}

async fn notify(tx: &mpsc::Sender<Out>, msg: ServerMsg) {
    if tx.send(Out::Msg(msg)).await.is_err() {
        tracing::debug!("client already gone");
    }
}

async fn run_session(
    mut stream: SplitStream<WebSocket>,
    tx: &mpsc::Sender<Out>,
    st: &AppState,
) -> Result<(), String> {
    // Held until this function returns: the slot frees when the session ends.
    let _slot = st.slots.clone().try_acquire_owned().map_err(|_| {
        format!("the server is busy: {} sessions are already running", st.instruction.max_sessions)
    })?;

    let first = tokio::time::timeout(Duration::from_secs(30), stream.next())
        .await
        .map_err(|_| "no hello message within 30 s".to_string())?;
    let (requested, video) = match first {
        Some(Ok(Message::Text(t))) => match serde_json::from_str::<ClientMsg>(&t) {
            Ok(ClientMsg::Hello { dialog, video }) => (dialog, video),
            Ok(_) => return Err("first message must be hello".into()),
            Err(e) => return Err(format!("bad hello: {e}")),
        },
        _ => return Err("first message must be a text hello".into()),
    };

    let (prompt, start_dialog) = st.plan();
    let first_message = if video {
        if st.videos.is_none() {
            return Err("video mode is not available: set videos_dir in Instruction.md".into());
        }
        "Begin a video lesson.".to_string()
    } else {
        match requested {
            Some(name) if st.library.has(&name) => format!("Begin the lesson with the dialog file {name}.md."),
            Some(name) => return Err(format!("unknown dialog {name:?}")),
            None => match start_dialog {
                Some(name) => format!("Begin the lesson with the dialog file {name}.md."),
                None => "Begin the lesson.".to_string(),
            },
        }
    };
    tracing::info!("session start (model {}, video mode {})", st.claude.model, if video { "first" } else { "on request" });

    // With video mode on, the tutor may also read the transcripts folder.
    let extra_dirs: Vec<std::path::PathBuf> = st.instruction.videos_dir.iter().filter(|_| st.videos.is_some()).cloned().collect();
    let tutor = ClaudeSession::spawn(&st.claude, &prompt, &st.instruction.dialogs_dir, &extra_dirs)
        .map_err(|e| format!("cannot start claude: {e}"))?;
    let mut session = Session::new(
        st.urls.clone(),
        tutor,
        st.stt.clone(),
        st.tts.clone(),
        tx.clone(),
        st.instruction.correction_pause,
        st.progress.clone(),
    );

    if session.begin(&first_message, st.library.entries.len()).await.is_err() {
        return Ok(());
    }
    let mut incoming = stream.map(to_incoming);
    match session.drive(&mut incoming, st.instruction.session_timeout).await {
        End::Expired => {
            let minutes = st.instruction.session_timeout.as_secs() / 60;
            tracing::info!("session expired after {minutes} minutes without activity");
            notify(tx, ServerMsg::SessionEnded { reason: format!("no activity for {minutes} minutes") }).await;
            Ok(())
        }
        End::Invalid(why) => Err(why),
        End::Closed | End::ClientGone => {
            tracing::info!("session ended");
            Ok(())
        }
    }
}
