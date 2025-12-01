# Hammock

Hammock captures Discord voice chat audio, transcribes it with Whisper, and writes timestamped caption lines per channel.

## Prerequisites

- Rust toolchain (edition 2024)
- Discord bot token with the `MESSAGE_CONTENT` and `GUILD_VOICE_STATES` intents enabled
- A Whisper GGML/GGUF model file on disk

## Configuration & environment

Copy `.env.sample` to `.env` (or export the variables directly) and fill in the values that match your deployment. The bot loads `.env` automatically via `dotenvy` when `cargo run` starts.

| Variable | Required | Default | Description |
| --- | --- | --- | --- |
| `DISCORD_TOKEN` | ✅ | – | Bot token from the Discord developer portal with `MESSAGE_CONTENT` and `GUILD_VOICE_STATES` intents. |
| `WHISPER_MODEL_PATH` | ⚠️* | Auto-generated under `WHISPER_MODEL_DIR` | Absolute path to the Whisper GGML/GGUF model. Omit it to let the bot download `ggml-<WHISPER_MODEL_NAME>.bin` next to `WHISPER_MODEL_DIR`. |
| `WHISPER_MODEL_DIR` | ❌ | `models/` | Directory used when inferring `WHISPER_MODEL_PATH` or when the model download runs. |
| `WHISPER_MODEL_NAME` | ❌ | `base` | Whisper model slug passed to the CLI / download URL (e.g., `small`, `medium`). |
| `WHISPER_CLI_PATH` | ❌ | `whisper` on `PATH` | Path to a `whisper` CLI binary. Enables CLI-based downloads when the model file is missing. |
| `WHISPER_LANGUAGE` | ❌ | Whisper auto-detect | Two-letter language hint that is forwarded to `whisper_rs`. |
| `WHISPER_USE_GPU` | ❌ | `true` when compiled with `--features cuda`, otherwise `false` | Toggle GPU inference. If CUDA support is missing at build time the setting is ignored. |
| `WHISPER_GPU_DEVICE` | ❌ | `0` | CUDA device index to run inference on when the GPU path is enabled. |
| `CAPTION_OUTPUT_DIR` | ❌ | `captions/` | Root folder where JSON caption session files are written. Created on startup. |
| `CAPTION_CHUNK_SECS` | ❌ | `3.0` (min `0.5`) | Duration (seconds) of PCM buffered before each transcription job. Influences latency vs. accuracy. |
| `DECODE_SAMPLE_RATE` | ❌ | `16000` | Decode sample rate requested from Songbird/Symphonia. Must match `CAPTION_CHUNK_SECS` to control chunk sample counts. |
| `ENTRY_SOUND_PATH` | ❌ | `resources/announce.mp3` | Optional MP3 announcement that plays (and must finish) before transcription starts. Set to an empty string to disable. |
| `ENTRY_SOUND_VOLUME` | ❌ | `0.5` | Linear volume multiplier for the entry sound (`1.0` = 100%, `0.0` = muted). Values outside 0–1 are clamped. |
| `ALLOW_SONGBIRD_UDP_ERRORS` | ❌ | `0` | Flip to `1` to re-enable Songbird "Illegal RTP message" logs for low-level debugging. |
| `OPENAPI_KEY` | ❌ | – | Provide an OpenAI API key to enable automatic transcript summaries via the Responses API. Captions are uploaded temporarily and deleted once the response arrives. |
| `OPENAPI_MODEL` | ❌ | `gpt-4o-mini` | Model sent to the OpenAI responses endpoint when producing summaries. |
| `INCLUDE_TRANSCRIPTS_WITH_SUMMARY` | ❌ | `true` | When summaries are enabled, control whether the raw JSON transcript is also uploaded to Discord alongside the summary message. Setting this to `false` requires `OPENAPI_KEY`. |

\* If `WHISPER_MODEL_PATH` is omitted but the `whisper` CLI is available, the bot assumes the model should live in `WHISPER_MODEL_DIR/ggml-<WHISPER_MODEL_NAME>.bin` and invokes the CLI with `--download-only` to fetch it. When an explicit `WHISPER_MODEL_PATH` is provided, the parent directory of that path is reused for future downloads.

### GPU acceleration

Build with `cargo run --release --features cuda` to compile Whisper with cuBLAS support. With the CUDA toolkit (including `nvcc`) and NVIDIA drivers installed inside WSL, inference will automatically use the GPU. Use `WHISPER_USE_GPU=false` to temporarily fall back to CPU if the GPU stack is unavailable.

### Logging noise

Discord occasionally delivers malformed UDP packets that Songbird flags as “Illegal RTP message received.” The bot now suppresses those error-level logs by default to avoid flooding the console. Set `ALLOW_SONGBIRD_UDP_ERRORS=1` if you need the raw Songbird UDP logs for troubleshooting.

## Running the bot

```bash
cargo run --release
```

In Discord, use the registered slash commands:

- `/join [voice_channel]` – start listening in a channel or omit the option to join your current voice channel
- `/leave` – disconnect and stop captioning
- `/ping` – health check

Caption sessions are rewritten into JSON under `CAPTION_OUTPUT_DIR` using the schema emitted by `src/captions/json.rs` (files look like `<guild>_<channel>_<timestamp>[_slug].json`). Each entry includes timestamps, speaker metadata, and the transcribed comment, and the `/leave` command uploads the finished file back to the invoking channel when possible.

### Transcript summaries

With `OPENAPI_KEY` set the `/leave` command also uploads the finished JSON transcript to OpenAI's Responses API, asks the configured `OPENAPI_MODEL` for concise Markdown notes, and posts the result underneath the transcription attachment before deleting the temporary upload.

If you prefer to share only the AI summary (and keep the JSON transcript private) set `INCLUDE_TRANSCRIPTS_WITH_SUMMARY=false`. This mode requires `OPENAPI_KEY`; the bot will fail fast if a summary is requested but no key is configured. The transcript is always uploaded when summarization is disabled.
