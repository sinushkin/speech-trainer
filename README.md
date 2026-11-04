# speech-trainer

A voice trainer for spoken English idioms, written in Rust. It takes a prepared
dialog, starts an in-memory Claude session for it, and runs a spoken
conversation loop: you talk, it listens, Claude answers, you hear the answer.

```
 client                                   server
 ──────                                   ──────
 microphone ─▶ VAD ─▶ utterance (PCM16) ─▶ speech-to-text ─▶ Claude session
 speaker   ◀─ WAV segments ◀──────────────  text-to-speech ◀─ reply (EN/RU)
```

* **Client**: detects voice activity, cuts the speech into utterances and sends
  each one to the server over a WebSocket; plays back the audio it receives.
  The mic is closed while the tutor is speaking, so no push-to-talk is needed.
* **Server**: turns the utterance into text, sends it to a resident
  `claude -p` process (stream-json, one per session, context lives in the
  process), turns the reply into speech and streams it back.
* **Dialogs**: Markdown files in the format of
  [english-transcripts/dialogs](https://github.com/sinushkin/english-transcripts/tree/master/dialogs):
  a dialogue plus quiz cards (English question, Russian hint, model answer,
  link to the exact moment in the source video).

Related: [LangTeacher](https://github.com/janosdios/langteacher) (Python) is the
model for this project; the resident-Claude approach comes from its
`claude-llama-proxy`. Everything here is Rust, with no Python.

## Status

Early development; see `docs/ARCHITECTURE.md`. English speech recognition
(whisper.cpp) and English/Russian speech synthesis (Piper voices) work; the whole
audio loop (speech, mp3, recognition, a server turn) is covered by one unit test, `audio_loop.rs`.

## Run

```
# one-time: the Piper binary and voices in ~/.cache/speech-trainer (see CLAUDE.md), and a whisper.cpp model, e.g. ggml-small.en.bin from huggingface.co/ggerganov/whisper.cpp
export SPEECH_TRAINER_MODEL=~/.cache/speech-trainer/ggml-small.en.bin
# edit Instruction.md: dialogs_dir, what to learn, session timeout
cargo run --release -p server -- --instruction Instruction.md   # listens on 127.0.0.1:8765
cargo run --release -p client                                   # speak ("another dialog" switches)
cargo run --release -p client -- --text                         # type
```

Every connection gets its own Claude process (Haiku, read-only tools inside the dialogs
folder) that is stopped after the idle timeout (30 minutes). Settings: `SPEECH_TRAINER_BIND`, `SPEECH_TRAINER_CLAUDE_MODEL` (overrides `model` in `Instruction.md`),
`SPEECH_TRAINER_CLAUDE_PROXY` (optional HTTP proxy for the claude process),
`SPEECH_TRAINER_URL` (client), `SPEECH_TRAINER_ARECORD_DEVICE`, `SPEECH_TRAINER_APLAY_DEVICE`.
Needs the `claude` CLI, `arecord`/`aplay` (alsa-utils), cmake and a C++ compiler to build whisper.cpp.

## License

MIT
