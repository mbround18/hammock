# Hammock

Hammock captures Discord voice chat audio, transcribes it with Whisper, and writes timestamped captions per channel. It is purpose-built for self-hosting: you run it, you keep the data, and you decide where the JSON transcripts live.

## Highlights

- Self-hosted, privacy-first captioning for Discord voice channels
- Real-time transcription backed by Whisper (GGML/GGUF) with GPU acceleration support
- Built-in Actix control plane exposing health probes, metrics, invite links, and OpenAPI docs
- Production-ready Docker + Compose workflow with persistent volumes for models and transcripts
- Optional OpenAI-powered summaries that can be shared alongside or instead of transcripts

## Table of Contents

1. [Quick Links](#quick-links)
2. [Prerequisites](#prerequisites)
3. [Configuration & Environment](#configuration--environment)
4. [Running Locally](#running-locally)
5. [Docker & Compose](#docker--compose)
6. [Health & Telemetry](#health--telemetry)
7. [Privacy & Data Handling](#privacy--data-handling)
8. [Slash Commands](#slash-commands)
9. [Transcript Summaries](#transcript-summaries)
10. [Performance Notes](#performance-notes)
11. [License](#license)

## Quick Links

- [License](../LICENSE)
- [Privacy Notice](./PRIVACY.md)
- [Disclaimer](./DISCLAIMER.md)
- [Contributing](./CONTRIBUTING.md)
- [GPU acceleration](./GPU.md)

## Prerequisites

- Rust toolchain (edition 2024)
- Discord bot token with the `MESSAGE_CONTENT` and `GUILD_VOICE_STATES` intents enabled
- Whisper GGML/GGUF model file available on disk (or the `whisper` CLI to download one)

## Configuration & Environment

Copy `.env.sample` to `.env` (or export the variables directly) and fill in the values that match your deployment. The bot loads `.env` automatically via `dotenvy` when `cargo run` starts.

| Variable                           | Required | Default                                                        | Description                                                                                                                                                                    |
| ---------------------------------- | -------- | -------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `DISCORD_TOKEN`                    | ✅       | –                                                              | Bot token from the Discord developer portal with `MESSAGE_CONTENT` and `GUILD_VOICE_STATES` intents.                                                                           |
| `WHISPER_MODEL_PATH`               | ⚠️\*     | Auto-generated under `WHISPER_MODEL_DIR`                       | Absolute path to the Whisper GGML/GGUF model. Omit it to let the bot download `ggml-<WHISPER_MODEL_NAME>.bin` next to `WHISPER_MODEL_DIR`.                                     |
| `WHISPER_MODEL_DIR`                | ❌       | `models/`                                                      | Directory used when inferring `WHISPER_MODEL_PATH` or when the model download runs.                                                                                            |
| `WHISPER_MODEL_NAME`               | ❌       | `base`                                                         | Whisper model slug passed to the CLI / download URL (e.g., `small`, `medium`).                                                                                                 |
| `WHISPER_CLI_PATH`                 | ❌       | `whisper` on `PATH`                                            | Path to a `whisper` CLI binary. Enables CLI-based downloads when the model file is missing.                                                                                    |
| `WHISPER_LANGUAGE`                 | ❌       | Whisper auto-detect                                            | Two-letter language hint that is forwarded to `whisper_rs`.                                                                                                                    |
| `WHISPER_USE_GPU`                  | ❌       | `true` on the accelerated image, `false` on the CPU image      | Toggle GPU inference. On a CPU build this warns and continues on CPU; setting it to `false` on an accelerated build is honored silently. See [GPU acceleration](./GPU.md).      |
| `WHISPER_GPU_DEVICE`               | ❌       | `0`                                                            | CUDA device index to run inference on when the GPU path is enabled. An index that does not exist is reported alongside the device count, and the bot continues on CPU.          |
| `UTTERANCE_SILENCE_MS`             | ❌       | `500`                                                          | Quiet after which an utterance is closed. Range 100-5000; below 200 warns about fragmentation. See [Utterance segmentation](#utterance-segmentation).                            |
| `UTTERANCE_MAX_SECS`               | ❌       | `20`                                                           | Longest an utterance may run before splitting. Range 1-30 — Whisper truncates beyond a 30-second window.                                                                        |
| `UTTERANCE_MIN_SPEECH_MS`          | ❌       | `300`                                                          | Speech shorter than this is not transcribed. Range 0-5000; 0 disables.                                                                                                          |
| `CAPTION_OUTPUT_DIR`               | ❌       | `captions/`                                                    | Root folder where JSON caption session files are written. Created on startup.                                                                                                  |
| `CAPTION_CHUNK_SECS`               | ❌       | `3.0` (min `0.5`)                                              | Duration (seconds) of PCM buffered before each transcription job. Influences latency vs. accuracy.                                                                             |
| `DECODE_SAMPLE_RATE`               | ❌       | `16000`                                                        | Decode sample rate requested from Songbird/Symphonia. Must match `CAPTION_CHUNK_SECS` to control chunk sample counts.                                                          |
| `ENTRY_SOUND_PATH`                 | ❌       | `resources/announce.mp3`                                       | Optional MP3 announcement that plays (and must finish) before transcription starts. Set to an empty string to disable.                                                         |
| `ENTRY_SOUND_VOLUME`               | ❌       | `0.5`                                                          | Linear volume multiplier for the entry sound (`1.0` = 100%, `0.0` = muted). Values outside 0–1 are clamped.                                                                    |
| `ALLOW_SONGBIRD_UDP_ERRORS`        | ❌       | `0`                                                            | Flip to `1` to re-enable Songbird "Illegal RTP message" logs for low-level debugging.                                                                                          |
| `OPENAPI_KEY`                      | ❌       | –                                                              | Provide an OpenAI API key to enable automatic transcript summaries via the Responses API. Captions are uploaded temporarily and deleted once the response arrives.             |
| `OPENAPI_MODEL`                    | ❌       | `gpt-4o-mini`                                                  | Model sent to the OpenAI responses endpoint when producing summaries.                                                                                                          |
| `INCLUDE_TRANSCRIPTS_WITH_SUMMARY` | ❌       | `true`                                                         | When summaries are enabled, control whether the raw JSON transcript is also uploaded to Discord alongside the summary message. Setting this to `false` requires `OPENAPI_KEY`. |

\* If `WHISPER_MODEL_PATH` is omitted but the `whisper` CLI is available, the bot assumes the model should live in `WHISPER_MODEL_DIR/ggml-<WHISPER_MODEL_NAME>.bin` and invokes the CLI with `--download-only` to fetch it. When an explicit `WHISPER_MODEL_PATH` is provided, the parent directory of that path is reused for future downloads.

## Running Locally

```bash
cargo run --release
```

Provide the `.env` file and ensure `models/` and `captions/` exist if you plan to persist data locally.

## Docker & Compose

The repository includes a multi-stage `Dockerfile` and a production-friendly `compose.yml`. Build and launch with:

```bash
docker compose up --build -d
```

Compose builds the optimized binary, installs Python dependencies with `uv`, mounts `./captions` and `./models` into the container, and exposes the internal web server on `8080`. Provide your `.env` file to the service (Compose already loads it) and mount persistent volumes if you want transcripts to survive container recreation.

## Health & Telemetry

Hammock exposes a lightweight Actix web server (default bind `0.0.0.0:8080`, configurable via `HTTP_BIND_ADDR`). Endpoints are designed for Kubernetes or any other health/metrics consumer:

- `GET /k8s/readyz` – readiness probe (includes uptime)
- `GET /k8s/livez` – liveness probe driven by the active guild/channel state
- `GET /k8s/metrics` – JSON metrics payload with guild/channel counts, participant totals, rolling transcription volumes (1h/30m/15m/5m/1m/30s), error and discard counts, queue depth, in-flight work, transcription duration percentiles, and `compute_backend` (whether transcription is running on GPU or CPU, which device, and why if GPU was requested and not used — see [GPU acceleration](./GPU.md))
- `GET /invite` – HTTP redirect to the discovered Discord invite link
- `GET /docs` – OpenAPI document describing every endpoint

Expose port `8080` (the `Dockerfile` already uses `EXPOSE 8080`) and wire the probes directly into your orchestration platform. The built-in Docker health check monitors `/k8s/readyz` automatically.

## Privacy & Data Handling

Hammock never uploads audio or text to third parties unless you supply an `OPENAPI_KEY` for optional summaries. JSON transcript files stay under `CAPTION_OUTPUT_DIR`. You, as the operator, are responsible for disclosure, consent, retention, and compliance. Two participant-labeling modes exist because of Discord's encryption model:

1. **Transparent mode** – the bot joins first, so Discord exposes usernames and Hammock uses them verbatim.
2. **Randomized mode** – the bot joins mid-call, so each speaker receives a stable numeric placeholder for that session.

See [`docs/PRIVACY.md`](./PRIVACY.md) for the full policy and operator obligations. Do not deploy Hammock unless you are comfortable owning the data it produces.

## Slash Commands

- `/join [voice_channel]` – start listening in a channel or omit the option to join your current voice channel
- `/leave` – disconnect and stop captioning
- `/ping` – lightweight health check

Caption sessions are rewritten into JSON under `CAPTION_OUTPUT_DIR` using the schema emitted by `src/captions/json.rs` (files look like `<guild>_<channel>_<timestamp>[_slug].json`). Each entry includes timestamps, speaker metadata (real names or numeric placeholders), and the transcribed comment. `/leave` uploads the finished file back to the invoking channel when possible.

## Transcript Summaries

With `OPENAPI_KEY` set, the `/leave` command uploads the finished JSON transcript to OpenAI's Responses API, asks the configured `OPENAPI_MODEL` for concise Markdown notes, and posts the result underneath the transcription attachment before deleting the temporary upload.

Set `INCLUDE_TRANSCRIPTS_WITH_SUMMARY=false` if you want to share only the AI summary (and keep the JSON transcript private). This mode requires `OPENAPI_KEY`; the bot fails fast if a summary is requested but no key is configured. The transcript is always uploaded when summarization is disabled.

## Performance Notes

### GPU acceleration

Hammock publishes two image variants. The unprefixed tags (`latest`, `sha-…`) are the CPU build; tags prefixed `cuda-runtime-` are the accelerated build, with CUDA compiled in and the CUDA runtime present:

```sh
docker run --rm --gpus all --env-file .env -p 8080:8080 \
  ghcr.io/mbround18/hammock:cuda-runtime-latest
```

No compilation step is needed. The host supplies the NVIDIA driver and the NVIDIA Container Toolkit; the image installs neither.

Neither variant needs configuration to do the expected thing — the accelerated image uses the GPU by default, the CPU image does not look for one. If a GPU is requested and unavailable, the bot says which of four things went wrong, and transcribes on the CPU rather than failing to start.

**See [GPU acceleration](./GPU.md)** for choosing a variant, granting a container GPU access, selecting among multiple GPUs, and the full fallback table.

Building from source still works: `cargo build --release --features cuda`, which needs the CUDA toolkit (including `nvcc`) on the build machine.

### Utterance segmentation

Audio is cut where speech stops, not on a timer. Each participant's stream is
segmented independently using the speech activity Discord already sends — a
client stops transmitting when nobody is talking, so the bot can tell where an
utterance ends without guessing from the audio.

Three settings control it, all optional:

| Variable | Default | Effect |
|---|---|---|
| `UTTERANCE_SILENCE_MS` | `500` | Quiet after which an utterance closes |
| `UTTERANCE_MAX_SECS` | `20` | Longest an utterance may run before splitting |
| `UTTERANCE_MIN_SPEECH_MS` | `300` | Speech shorter than this is not transcribed |

```text
INFO utterance segmentation: close after 500ms silence, max 20s, min speech 300ms
```

`UTTERANCE_SILENCE_MS` is the one worth understanding. Lower it and utterances
fragment mid-sentence — a 200 ms setting splits on an ordinary breath, and word
error rate rises. Raise it and captions lag, because the delay between someone
finishing and their caption appearing is roughly this value plus transcription
time. Below 200 ms the bot warns you rather than silently accepting a setting
that undoes the feature.

`UTTERANCE_MAX_SECS` is capped at 30 for a concrete reason: Whisper processes a
fixed 30-second window per request regardless of input length, so a longer
utterance would be truncated. It also bounds memory — 20 s of audio is about
640 KB per speaking participant.

**Silence and background noise produce no captions.** Whisper narrates non-speech
audio rather than staying quiet — handed music it emits `(soft music)`, handed
applause `[APPLAUSE]`. Those are the model correctly reporting that nothing was
said, so they are discarded rather than written to the transcript. An annotation
sitting next to real words is real speech and is kept.

Buffered audio is transcribed when a speaker disconnects, when the bot leaves a
channel, and on shutdown, so a partial sentence is not lost to a `/leave`.

**`CAPTION_CHUNK_SECS` is deprecated** and no longer affects segmentation. It is
still read, and setting it produces a startup warning so a tuned value does not
appear to keep working when it does not.

#### Reading the metrics

| Reading | Meaning |
|---|---|
| `utterance_length_ms.p50` around 5-10 s | Healthy conversational segmentation |
| `p50` collapsing toward `UTTERANCE_MIN_SPEECH_MS` | Silence threshold too low — utterances fragmenting |
| `p50` pinned at `UTTERANCE_MAX_SECS` | Threshold too high, or someone transmits continuously |
| `total_segments_rejected_as_non_speech` rising | An open microphone in a noisy room. Working as intended |

Note that `total_segments_rejected_as_non_speech` and
`total_utterances_discarded` mean different things: a rejection means the audio
contained no speech and nothing was lost, a discard means the transcription
queue was full and work **was** lost.

### Transcription throughput

Several speakers are transcribed concurrently. How many at once is set by
`TRANSCRIPTION_CONCURRENCY`, which defaults to available cores / 4 (capped at 4)
on CPU and 2 on GPU. The bot reports the effective value and where it came from
at startup:

```text
INFO transcription concurrency: 4 workers (default for cpu backend, 32 cores available)
```

The CPU default divides by four because Whisper already uses up to 4 threads
inside a single transcription — one worker per core would oversubscribe the
machine and run slower than doing less. The GPU default is small because each
concurrent transcription needs its own decoder state, which costs a few hundred
megabytes for a small model and more for a large one.

**When transcription cannot keep up, the bot drops work rather than stalling.**
This matters: audio reception runs on a 20 ms clock shared by everyone in the
channel, so waiting for a busy transcriber would lose audio for every speaker,
including those whose captions were fine. Instead the newest utterance is
discarded, counted, and logged with the guild, channel and speaker it belonged
to.

Whether your setting is right is answerable from `/k8s/metrics` alone:

| Reading | Meaning |
|---|---|
| `transcription_queue_depth` near 0, `transcription_in_flight` below the limit | Spare capacity |
| `transcription_queue_depth` near 0, `in_flight` at the limit | Saturated but keeping up |
| `transcription_queue_depth` rising across polls, `total_utterances_discarded` flat | Falling behind, not yet losing work |
| `total_utterances_discarded` rising | Losing work now — raise the limit, or use a smaller model |
| `transcription_in_flight` at 0 while `queue_depth` is above 0 | The pool is stuck. That is a bug, not overload — please report it |

`transcription_duration_ms` carries `p50_ms`/`p95_ms` over recent work, which is
the fastest way to see whether a model change made transcription slower.

On shutdown the bot gives in-flight transcriptions up to 10 seconds to finish
and reports anything still outstanding, so captions lost to a restart are
visible rather than silent.

### Logging noise

Discord occasionally delivers malformed UDP packets that Songbird flags as “Illegal RTP message received.” Hammock suppresses those error-level logs by default to avoid flooding the console. Set `ALLOW_SONGBIRD_UDP_ERRORS=1` if you need the raw Songbird UDP logs for troubleshooting.

## License

Hammock is distributed under the [GNU Affero General Public License v3.0](../LICENSE).

### Why AGPL 3.0?

AGPL 3.0 encourages transparency and protects users. This project handles live audio capture and AI-generated transcripts, so the license ensures that anyone who hosts or modifies the software must keep their changes open and disclose how data is handled. It also provides strong liability protection and prevents private, closed-source services from taking the code without sharing improvements. We are open to future license changes if a compelling need arises.
