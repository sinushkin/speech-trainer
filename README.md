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

Related: [LangTeacher](https://github.com/janosdios/langteacher) (Python), whose
speech pieces this project reuses through pluggable workers.

## Status

Early development. See `docs/ARCHITECTURE.md` for the design and what is done.

## License

MIT
