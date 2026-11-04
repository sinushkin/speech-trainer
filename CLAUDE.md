# speech-trainer

Голосовой тренажёр английских идиом на Rust. Берёт заготовленный диалог, на каждую
сессию держит в памяти процесс `claude -p`, и крутит цикл: клиент слушает микрофон
(VAD) и шлёт реплику на сервер, сервер распознаёт речь, отдаёт текст Claude, ответ
превращает в звук и возвращает клиенту. Публичный репозиторий:
https://github.com/sinushkin/speech-trainer (ветка `main`, лицензия MIT).

Прототип на Python: `~/langteacher` (форк janosdios/langteacher). Подход с резидентным
`claude -p` взят из его `claude-llama-proxy`. **В этом проекте Python не используется.**

## Структура

Cargo-воркспейс (edition 2024), три крейта:

| крейт / модуль | за что отвечает |
|---|---|
| `crates/protocol` | общий протокол клиент↔сервер: `ClientMsg`, `ServerMsg` (JSON по тегу `type`), `wav` (запись/чтение PCM16 mono WAV, байты ↔ сэмплы), `SAMPLE_RATE = 16000` |
| `crates/server/src/dialog.rs` | разбор диалогов в формате `english-transcripts/dialogs/*.md` (реплики + карточки: вопрос, подсказка, образцовый ответ, ссылка). Имя диалога из сети допускает только `[A-Za-z0-9_-]` |
| `.../prompt.rs` | собирает системный промпт тьютора из диалога (правила, формат ответа, список карточек) |
| `.../claude.rs` | `ClaudeSession`: один постоянный процесс `claude -p --input-format stream-json` на сессию, контекст живёт в процессе; запрещены все инструменты (тьютор не должен вести себя как кодовый агент) |
| `.../markup.rs` | разбор ответа тьютора: `<say lang="en\|ru">…</say>` (что озвучить) и `<link url="…">…</link>`. Текст без тегов озвучивается как английский |
| `.../backend.rs` | трейты `Tutor`, `Transcriber`, `Synthesizer` и `BackendError`; реальные реализации и тестовые заглушки подставляются через них |
| `.../session.rs` | один ход диалога: реплика → распознавание → тьютор → озвучка и ссылки → клиенту. Ссылку пропускает, только если её URL есть на карточке текущего диалога. Тестируется на заглушках |
| `.../stt.rs` | распознавание речи: whisper.cpp через `whisper-rs`, только английский, модель грузится один раз, декодирование в `spawn_blocking` |
| `.../tts.rs` | **заглушка `NoSpeech`**: озвучки пока нет, клиент получает только текст. Сюда придёт Piper (английский и русский) |
| `.../app.rs` | axum: `GET /dialogs` (список), `GET /ws` (сессия), склейка сокета и `Session` |
| `.../main.rs` | настройка из переменных окружения, запуск |
| `crates/client/src/vad.rs` | детектор голоса по энергии (порт параметров из LangTeacher): калибровка шума, 240 мс предзахвата, 800 мс тишины в конце, минимум 300 мс, максимум 15 с |
| `.../audio.rs` | микрофон и динамик через подпроцессы `arecord`/`aplay` (работает через PipeWire/Pulse) |
| `.../main.rs` | цикл клиента: half-duplex, пока тьютор говорит, микрофон игнорируется до `turn_done`. Режим `--text` вводит ответы с клавиатуры |

Подробнее про устройство и протокол: `docs/ARCHITECTURE.md`.

## Где что лежит

- **Модель Whisper**: `~/.cache/speech-trainer/ggml-small.en.bin` (487 614 201 байт,
  ggml `small.en` из `ggerganov/whisper.cpp` на Hugging Face). Физически `~/.cache` это
  симлинк на `/home/user/data/user-cache/.cache` (диск `/home/user/data`). В git не
  входит. Рядом: `fetch.sh` (скрипт параллельной докачки) и логи.
- **Голос Piper** (понадобится для озвучки): `~/langteacher/piper_voices/en_US-amy-medium.onnx`
  (+ `.onnx.json`), 63 МБ. **Русского голоса пока нет**: его нужно докачать
  (`rhasspy/piper-voices` на Hugging Face, например `ru_RU-irina-medium`).
- **Диалоги**: `~/repos/english-transcripts/dialogs/` (45 штук), путь задаётся
  `SPEECH_TRAINER_DIALOGS`. Тестовый образец лежит в `crates/server/tests/fixtures/`.
- **Старые модели из langteacher, здесь не используются**: кэш faster-whisper
  `~/.cache/whisper/models--Systran--faster-whisper-small` (формат CTranslate2, для
  whisper.cpp не подходит) и веса OmniVoice в `~/langteacher/omnivoice-model/` (2,4 ГБ,
  от него отказались как от слишком медленного).

### Как докачивать большие файлы (сеть здесь медленная)

Одно соединение даёт ~30 КБ/с, а 15 параллельных потоков с диапазонами около 500 КБ/с.
Образец: `~/.cache/speech-trainer/fetch.sh` (поменяйте `URL` и `TOTAL`). Копировать файл с VPS
`profit` по `scp` ещё медленнее (~17 КБ/с), поэтому через него качать не стоит.

## Запуск

```
export SPEECH_TRAINER_MODEL=~/.cache/speech-trainer/ggml-small.en.bin   # по умолчанию тот же
export SPEECH_TRAINER_DIALOGS=~/repos/english-transcripts/dialogs         # по умолчанию ./dialogs
cargo run --release -p server                                  # 127.0.0.1:8765
cargo run --release -p client -- 16_the_job_interview          # голосом
cargo run --release -p client -- 16_the_job_interview --text   # с клавиатуры
```

Переменные окружения: `SPEECH_TRAINER_BIND`, `SPEECH_TRAINER_MODEL`,
`SPEECH_TRAINER_DIALOGS`, `SPEECH_TRAINER_CLAUDE_BIN` (по умолчанию `claude`),
`SPEECH_TRAINER_CLAUDE_MODEL` (по умолчанию `haiku`), `SPEECH_TRAINER_CLAUDE_PROXY`
(HTTP-прокси для процесса claude; на этой машине `http://127.0.0.1:8118`),
клиент: `SPEECH_TRAINER_URL`, `SPEECH_TRAINER_ARECORD_DEVICE`, `SPEECH_TRAINER_APLAY_DEVICE`.

Для сборки нужны `cmake` и C++-компилятор (whisper.cpp собирается вместе с `whisper-rs-sys`).

## Тесты

`cargo test` проходит без моделей: сессия проверяется на заглушках, `claude.rs` на
shell-скрипте, который говорит по протоколу stream-json, VAD на синтетических сигналах.
Один тест требует реальную модель и помечен `#[ignore]`:
`SPEECH_TRAINER_MODEL=~/.cache/speech-trainer/ggml-small.en.bin cargo test -p server --release -- --ignored`.
`cargo clippy --all-targets` чистый.

## Соглашения

- В продакшн-коде нельзя `unwrap()` и `let _ =`: ошибки возвращаются через `Result`,
  а там, где отправка может не удаться (клиент ушёл), это логируется или обрабатывается.
- Для тестов и сборки внешних сервисов не нужны. Реальный `claude -p` в тестах не
  запускается (чтобы не тратить подписку), вместо него shell-заглушка.
- Коммиты оканчиваются строкой `Co-Authored-By`, заданной в настройках сессии.

## Состояние и что дальше

Готово: протокол, разбор диалогов, сессия тьютора, распознавание английской речи
(проверено на реальной модели), WebSocket-сервер, клиент с VAD, сквозной прогон с
заглушкой вместо `claude`.

Не готово и не проверено:
1. **Озвучка** (английский и русский): Piper. Два пути: готовый бинарник `piper` как
   постоянный процесс (быстрее сделать) или нативно через `ort` + espeak-ng.
2. Работа с живым `claude -p` и с микрофоном (VAD пока проверен только синтетикой).
3. Клиент для телефона (браузер, WebSocket), если нужна ходьба по локальной сети.
4. Распознавание русской речи не нужно: учащийся отвечает по-английски.
