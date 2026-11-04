//! HTTP/WebSocket layer: `GET /dialogs` lists dialogs, `GET /ws` runs a session.

use crate::claude::{ClaudeConfig, ClaudeSession};
use crate::dialog;
use crate::prompt::system_prompt;
use crate::session::{Out, Session};
use crate::stt::WhisperTranscriber;
use crate::tts::NoSpeech;
use axum::Router;
use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use futures_util::{SinkExt, StreamExt};
use protocol::{ClientMsg, ServerMsg};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

pub struct AppState {
    pub dialogs_dir: PathBuf,
    pub claude: ClaudeConfig,
    pub stt: Arc<WhisperTranscriber>,
    pub tts: Arc<NoSpeech>,
}

pub fn router(state: Arc<AppState>) -> Router {
    Router::new().route("/dialogs", get(list_dialogs)).route("/ws", get(ws_upgrade)).with_state(state)
}

async fn list_dialogs(State(st): State<Arc<AppState>>) -> Response {
    match dialog::list(&st.dialogs_dir) {
        Ok(names) => axum::Json(names).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("cannot list dialogs: {e}")).into_response(),
    }
}

async fn ws_upgrade(ws: WebSocketUpgrade, State(st): State<Arc<AppState>>) -> Response {
    ws.on_upgrade(move |socket| handle_socket(socket, st))
}

async fn handle_socket(socket: WebSocket, st: Arc<AppState>) {
    let (mut sink, mut stream) = socket.split();
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

    if let Err(message) = run_session(&mut stream, &tx, &st).await {
        tracing::warn!("session ended with error: {message}");
        if tx.send(Out::Msg(ServerMsg::Error { message })).await.is_err() {
            tracing::debug!("client already gone");
        }
    }
    drop(tx);
    if let Err(e) = writer.await {
        tracing::warn!("writer task failed: {e}");
    }
}

async fn run_session(
    stream: &mut futures_util::stream::SplitStream<WebSocket>,
    tx: &mpsc::Sender<Out>,
    st: &AppState,
) -> Result<(), String> {
    let first = tokio::time::timeout(Duration::from_secs(30), stream.next())
        .await
        .map_err(|_| "no hello message within 30 s".to_string())?;
    let name = match first {
        Some(Ok(Message::Text(t))) => match serde_json::from_str::<ClientMsg>(&t) {
            Ok(ClientMsg::Hello { dialog }) => dialog,
            Ok(_) => return Err("first message must be hello".into()),
            Err(e) => return Err(format!("bad hello: {e}")),
        },
        _ => return Err("first message must be a text hello".into()),
    };
    let dialog = dialog::load(&st.dialogs_dir, &name).map_err(|e| e.to_string())?;
    tracing::info!("session start: {} ({} cards)", dialog.title, dialog.cards.len());
    let tutor = ClaudeSession::spawn(&st.claude, &system_prompt(&dialog)).map_err(|e| format!("cannot start claude: {e}"))?;
    let mut session = Session::new(dialog, tutor, st.stt.clone(), st.tts.clone(), tx.clone());

    if session.begin().await.is_err() {
        return Ok(());
    }
    while let Some(frame) = stream.next().await {
        let frame = frame.map_err(|e| format!("websocket error: {e}"))?;
        let outcome = match frame {
            Message::Binary(pcm) => session.on_utterance(&pcm).await,
            Message::Text(t) => match serde_json::from_str::<ClientMsg>(&t) {
                Ok(ClientMsg::Text { text }) => session.on_text(&text).await,
                Ok(ClientMsg::Hello { .. }) => Ok(()),
                Err(e) => return Err(format!("bad message: {e}")),
            },
            Message::Close(_) => return Ok(()),
            _ => Ok(()),
        };
        if outcome.is_err() {
            return Ok(());
        }
    }
    Ok(())
}
