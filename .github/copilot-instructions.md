# Copilot Instructions

## Architecture map

- `src/main.rs` wires the Poise slash-command framework, Songbird voice gateway, `BotConfig::from_env`, Whisper worker bootstrap, and application state shared via `Arc<BotState>`.
- `voice/mod.rs` attaches the caption pipeline to a `Call`, aggregates PCM per SSRC, maps it to speakers (using `VoiceRoster` fallbacks), and forwards fixed-size chunks to the transcription worker.
- `voice/roster.rs` tracks who is in the tracked voice channel plus recent joins/spoken events so unidentified SSRCs can be heuristically mapped to real `UserId`s.
- `transcription.rs` hosts the background Whisper worker (`spawn_worker`) that down-samples PCM to 16 kHz, runs `whisper_rs`, and appends structured entries to the JSON sink.
- `captions/json.rs` persistently tracks per-guild/channel sessions, rewriting a single JSON document that contains `metadata` and `transcriptions`; `CaptionSink::relabel_placeholder` patches past entries once a placeholder speaker is resolved.
- `utils/discord.rs` centralises user-name resolution so cache misses fall back to REST lookups with consistent logging.

## Data flow & lifecycle

- `/join` (`main.rs`) resolves the target channel, plays `resources/announce.mp3` if present, self-mutes the bot, prepares the roster, starts a caption session file, and calls `attach_caption_pipeline` with chunk/sample parameters drawn from `BotConfig`.
- The `AudioAggregator` buffers decoded Songbird frames until `chunk_samples` (derived from `CAPTION_CHUNK_SECS` and `DECODE_SAMPLE_RATE`) is reached or `silence_flush` elapses, then submits a `TranscriptionJob` carrying message metadata.
- `SpeakerUpdateSender` broadcasts the current talker via a `watch` channel so `run_presence_task` can update the Discord presence string in near real time.
- `/leave` tears down the call, clears speaker updates and rosters, finalises the JSON session (adding duration metadata), and uploads it back to the invoking channel as an attachment when possible.
- `ensure_model_available` will either use the `whisper` CLI (if `WHISPER_CLI_PATH` or a PATH lookup succeeds) or fall back to downloading `ggml-<model>.bin` directly from Hugging Face; any new Whisper-related changes must respect this bootstrap path.

## Developer workflows

- Default run: `cargo run --release` with `DISCORD_TOKEN`, `WHISPER_MODEL_PATH` (or CLI), and optional tuning vars exported (`CAPTION_CHUNK_SECS`, `DECODE_SAMPLE_RATE`, etc.).
- GPU build: `cargo run --release --features cuda` plus `WHISPER_USE_GPU=true` (set `WHISPER_GPU_DEVICE` for multi-GPU setups); CPU fallback toggled via `WHISPER_USE_GPU=false` even in CUDA builds.
- JSON captions land under `CAPTION_OUTPUT_DIR` (default `captions/`) with file names `<guild>_<channel>_<timestamp>[_slug].json`; use the existing helper methods when emitting or relabeling entries rather than writing files directly.
- When adding slash commands, declare them in the `FrameworkOptions.commands` vector and keep them side-effect free until after `ctx.defer()` succeeds; the framework auto-registers globally during startup via `builtins::register_globally`.

## Patterns & gotchas

- I/O or CPU-heavy work (Whisper inference, JSON rewrites, relabeling) must stay off the async reactor via `tokio::task::spawn_blocking`, matching `transcription.rs` and `CaptionSink::relabel_placeholder`.
- Concurrency relies on `DashMap` for shared maps (`active_calls`, `voice_rosters`, SSRC buffers); mutate through the provided helpers to avoid holding locks longer than necessary.
- `CaptionSink::append_json` rewrites the entire document per entry, so keep caption payloads concise and batch upstream if you need extensions; large payloads will impact latency.
- SSRC mapping is lossy; always call `VoiceRoster::note_join/leave/spoke` when touching voice-state logic so speaker relabeling remains accurate and placeholders can be resolved retroactively.
- Presence updates depend on `SpeakerUpdateSender::notify`; any new pipeline that bypasses `AudioAggregator::dispatch_chunk` must still call `notify` to avoid a stale "Listening" status.
- Respect the existing env var contracts in `config.rs` (e.g., `CAPTION_CHUNK_SECS` floors at 0.5s, `WHISPER_MODEL_PATH` may be auto-generated) so new features don't break headless deployments.
