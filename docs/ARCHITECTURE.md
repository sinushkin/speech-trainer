# Architecture

```
client                                         server
──────                                         ──────
arecord ─▶ frames (30 ms, 16 kHz) ─▶ VAD
              │ utterance (PCM16)
              └──────────── WebSocket binary ─▶ WAV ─▶ whisper.cpp ─▶ text
                                                                        │
              ┌─────────── WebSocket text ◀── Transcript / Reply / Link │
aplay ◀── WAV ◀── WebSocket binary ◀── TTS ◀── tagged reply ◀── claude -p (resident)
```

## Wire protocol (`crates/protocol`)

One WebSocket, `GET /ws`.

* Client text frames: `{"type":"hello","dialog":"16_the_job_interview"}` (first message),
  `{"type":"text","text":"..."}` (typed input, bypasses recognition).
* Client binary frames: one whole utterance, PCM16 little endian, mono, 16 kHz.
* Server text frames: `ready`, `transcript`, `reply`, `link`, `turn_done`, `error`.
* Server binary frames: one complete WAV file per spoken segment.
* `turn_done` means the tutor finished and the microphone may open again. The
  client is half duplex: it ignores the microphone until `turn_done`, so the
  tutor's own voice is never recorded and no push-to-talk key is needed.

`GET /dialogs` returns the list of dialog names.

## Server (`crates/server`)

| module | job |
|---|---|
| `dialog` | parses the `english-transcripts/dialogs` Markdown (dialogue + quiz cards); dialog names from the network are restricted to `[A-Za-z0-9_-]` |
| `prompt` | builds the tutor's system prompt from a dialog |
| `claude` | one resident `claude -p --input-format stream-json` process per session; context lives in the process (same approach as LangTeacher's `claude-llama-proxy`) |
| `markup` | parses the tutor's reply: `<say lang="en|ru">`, `<link url="...">` |
| `session` | one turn: utterance, text, tutor, speech and links, back to the client. Generic over `Tutor`, `Transcriber`, `Synthesizer`, so it is tested with in-memory fakes |
| `stt` | whisper.cpp through `whisper-rs`, English, model loaded once |
| `tts` | placeholder: no voice yet, replies are text only |
| `app` | axum routes and the WebSocket plumbing |

The tutor may only link to videos that appear on a card of the current dialog;
any other URL in a reply is dropped.

## Client (`crates/client`)

* `vad`: energy-based voice activity detection, same defaults as LangTeacher
  (30 ms frames, RMS floor 120, 800 ms trailing silence, 300 ms minimum, 15 s
  cap), plus noise-floor calibration and 240 ms pre-roll so the first word is not cut.
* `audio`: `arecord`/`aplay` subprocesses (work through PipeWire/Pulse default devices).
* `--text` mode types answers instead of speaking.

## Status

Done: protocol, dialogs, tutor session, speech recognition (English), WebSocket server,
client with VAD, tests with fakes. Not done: speech synthesis (English and Russian),
phone/web client, multiple sessions share one recogniser (decoding is per utterance).
