# Voice-to-Text Caption Bot

Captures Discord voice chat audio, transcribes it with Whisper, and writes timestamped caption lines per channel.

## Prerequisites

- Rust toolchain (edition 2024)
- Discord bot token with the `MESSAGE_CONTENT` and `GUILD_VOICE_STATES` intents enabled
- A Whisper GGML/GGUF model file on disk

## Required environment

| Variable | Description |
| --- | --- |
| `DISCORD_TOKEN` | Bot token from the Discord developer portal |
| `WHISPER_MODEL_PATH` | Absolute path to the Whisper model file |
| `CAPTION_OUTPUT_DIR` | Optional directory for caption files (defaults to `captions/`) |
| `CAPTION_CHUNK_SECS` | Optional chunk length in seconds (default `3.0`) |
| `DECODE_SAMPLE_RATE` | Optional decode sample rate (default `16000`) |
| `WHISPER_LANGUAGE` | Optional language hint for Whisper |
| `WHISPER_MODEL_NAME` | Model identifier passed to the Whisper CLI when auto-downloading (default `base`) |
| `WHISPER_MODEL_DIR` | Optional directory used when auto-generating a model path (default `models/`) |
| `WHISPER_CLI_PATH` | Optional path to a `whisper` CLI binary; if omitted the PATH is searched |
| `WHISPER_USE_GPU` | Set to `false` to force CPU transcription; defaults to `true` when CUDA is available |
| `WHISPER_GPU_DEVICE` | CUDA device index to run inference on (default `0`) |

If `WHISPER_MODEL_PATH` is omitted but the `whisper` CLI is available, the bot assumes the model should live in `WHISPER_MODEL_DIR/<WHISPER_MODEL_NAME>.bin` and invokes the CLI with `--download-only` to fetch it. When an explicit `WHISPER_MODEL_PATH` is provided, the parent directory of that path is used instead.

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

Caption text lands in `CAPTION_OUTPUT_DIR/<channel_id>/<YYYY-MM-DD>.txt` with timestamps and speaker metadata.
