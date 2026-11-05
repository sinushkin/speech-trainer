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

* Client text frames: `{"type":"hello"}` (first message; `"video":true` starts video mode; optional `"dialog":"16_the_job_interview"` names the dialog to start with),
  `{"type":"text","text":"..."}` (typed input, bypasses recognition).
* Client binary frames: one whole utterance, PCM16 little endian, mono, 16 kHz.
* Server text frames: `watch` (send the learner to a whole video; the client keeps the microphone closed until Enter), `ready`, `transcript`, `reply`, `link`, `turn_done`, `error`, `session_ended` (idle timeout; the Claude process was stopped and the connection closes).
* Server binary frames: one complete WAV file per spoken segment.
* `turn_done` means the tutor finished and the microphone may open again. The
  client is half duplex: it ignores the microphone until `turn_done`, so the
  tutor's own voice is never recorded and no push-to-talk key is needed.

`GET /dialogs` returns the list of dialog names.

## Server (`crates/server`)

| module | job |
|---|---|
| `instruction` | parses `Instruction.md`: a `---` header (`dialogs_dir`, `model`, `session_timeout_minutes`, `max_sessions`) and the free text on what is learned and how |
| `dialog`, `library` | parse the dialog Markdown; the library scans the folder at start: a catalogue for the prompt and the set of all card video links |
| `prompt` | the tutor's system prompt: rules and reply format, the instruction text, the catalogue |
| `claude` | one resident `claude -p --input-format stream-json` process per session, working directory = the dialogs folder, tools limited to `Read,Glob,Grep`, `dontAsk` permission mode. The tutor itself finds and switches dialogs when the learner asks |
| `session` | one turn: utterance, text, tutor, speech and links, back to the client; `drive` serves a connection and ends it after the idle timeout. Generic over `Tutor`, `Transcriber`, `Synthesizer`, so it is tested with in-memory fakes |
| `stt` | whisper.cpp through `whisper-rs`, English, model loaded once |
| `markup` | parses the tutor reply; `split_by_script` picks the voice from the alphabet of the text (Cyrillic: Russian voice, Latin: English voice), the `lang` attribute is only a fallback |
| `videos` | the video transcripts for video mode: which are drillable, a random queue per session (shuffled in code), the set of known video links |
| `tts` | Piper voices: one resident `piper --json-input` process per language (en, ru) |
| `audio_loop` | unit test of the whole audio loop: mixed ru/en speech, mp3, recognition, a server turn (no microphone or speakers) |
| `app` | axum routes; per connection a semaphore slot (`max_sessions`), a new Claude process and `Session::drive` with the idle timeout |

The tutor may only link to videos that appear on some card in the dialogs folder;
any other URL in a reply is dropped.

## Client (`crates/client`)

* `vad`: energy-based voice activity detection, same defaults as LangTeacher
  (30 ms frames, RMS floor 120, 800 ms trailing silence, 300 ms minimum, 15 s
  cap), plus noise-floor calibration and 240 ms pre-roll so the first word is not cut.
* `audio`: `arecord`/`aplay` subprocesses (work through PipeWire/Pulse default devices).
* At start (before connecting) the client runs a microphone check: quiet, beep, "say something", playback of what was heard; repeatable, `--no-calibration` skips it.
* `--text` mode types answers instead of speaking.

## Status

Done: protocol, dialogs, tutor session, English speech recognition, English and Russian
speech synthesis, WebSocket server, client with VAD, tests with fakes plus one real-model audio loop unit test.
Not done: phone/web client, native (ort) Piper, live microphone verification.
