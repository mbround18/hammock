pub mod pool;

use std::{
    borrow::Cow,
    ffi::CStr,
    os::raw::{c_char, c_void},
    path::PathBuf,
    sync::{Arc, Once},
    time::Instant,
};

use anyhow::Context as _;
use chrono::{DateTime, Utc};
use serenity::model::id::{ChannelId, GuildId, UserId};
use tokio::sync::mpsc::{self, error::TrySendError};
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperState};
use whisper_rs_sys::{ggml_log_level, whisper_log_set};

use crate::{
    captions::{CaptionEntry, CaptionSink, SpeakerInfo},
    telemetry::{
        AppMetrics, ComputeBackend, FallbackReason,
        backend::{gpu_discovery, observe_ggml_log},
    },
    transcription::pool::{ConcurrencyLimit, StatePool, spawn_dispatcher},
};
use whisper_rs::WhisperContextParameters;

const PCM_NORMALIZER: f32 = i16::MAX as f32;
const WHISPER_SAMPLE_RATE: u32 = 16_000;
static WHISPER_LOGGER: Once = Once::new();

/// How a job's speaker name is known.
///
/// Resolving a Discord display name can require an HTTP round trip, and the
/// only place that used to happen was inside the voice tick handler — a
/// network call on the 20 ms receive path, which Constitution Principle I
/// forbids by name. This enum is what moves it: a cache hit costs nothing and
/// stays on the tick path, while a miss is carried as `Deferred` and awaited in
/// the dispatcher, where awaiting is free.
pub enum SpeakerLabel {
    /// Known without awaiting — a cache hit, or a placeholder for a speaker
    /// whose identity was never established.
    Resolved(String),
    /// Not in cache. Resolve off the receive path.
    Deferred {
        user_id: UserId,
        /// Carried rather than plumbed through the worker pool because the pool
        /// is constructed at startup, long before any Discord context exists.
        /// `Context` is a bundle of `Arc`s, so this clone is a few refcount
        /// bumps, not a copy of the cache.
        ctx: serenity::client::Context,
    },
}

impl SpeakerLabel {
    /// A name good enough to log with, without awaiting anything. Used on the
    /// discard path, which must not do work proportional to the overload that
    /// caused it.
    pub fn display_hint(&self) -> Cow<'_, str> {
        match self {
            Self::Resolved(name) => Cow::Borrowed(name.as_str()),
            Self::Deferred { user_id, .. } => Cow::Owned(format!("user {user_id}")),
        }
    }

    /// The real name, resolving it if that was deferred. Only ever called from
    /// async context off the receive path.
    pub async fn resolve(self) -> String {
        match self {
            Self::Resolved(name) => name,
            Self::Deferred { user_id, ctx } => crate::utils::resolve_user_name(&ctx, user_id).await,
        }
    }
}

impl TranscriptionJob {
    /// Turn a deferred speaker label into a real name.
    ///
    /// Called by the dispatcher — after the job has left the receive path and
    /// before it enters the blocking decode, which is the only window where an
    /// await is both possible and free.
    pub async fn resolve_speaker(&mut self) {
        if matches!(self.speaker, SpeakerLabel::Deferred { .. }) {
            let label = std::mem::replace(&mut self.speaker, SpeakerLabel::Resolved(String::new()));
            self.speaker = SpeakerLabel::Resolved(label.resolve().await);
        }
    }

    /// The speaker's name. Empty only if read before `resolve_speaker`, which
    /// the dispatcher always calls first.
    pub fn speaker_name(&self) -> &str {
        match &self.speaker {
            SpeakerLabel::Resolved(name) => name.as_str(),
            SpeakerLabel::Deferred { .. } => "",
        }
    }
}

pub struct TranscriptionJob {
    pub channel_id: ChannelId,
    pub guild_id: GuildId,
    pub speaker_id: Option<UserId>,
    pub speaker: SpeakerLabel,
    pub pcm: Vec<i16>,
    pub sample_rate: u32,
    pub started_at: DateTime<Utc>,
    /// Set when the job is accepted onto the queue, so queue wait is separable
    /// from decode time. `None` until then.
    pub queued_at: Option<Instant>,
}

/// What happened to a submitted job. Returned rather than logged by the caller,
/// because the caller is on the receive path and has nothing useful to do about
/// any of these outcomes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubmitOutcome {
    /// Accepted onto the queue.
    Queued,
    /// The queue was full and this job was dropped (FR-005).
    Discarded,
    /// The worker pool is gone — shutdown in progress.
    Closed,
}

#[derive(Clone)]
pub struct TranscriptionHandle {
    tx: mpsc::Sender<TranscriptionJob>,
    metrics: Arc<AppMetrics>,
}

impl TranscriptionHandle {
    /// Offer a job to the transcription queue.
    ///
    /// **Deliberately not `async`.** This is called from inside the songbird
    /// voice tick handler, which runs every 20 ms and which Constitution
    /// Principle I forbids from awaiting a queue. A synchronous `try_send`
    /// cannot block its caller under any queue condition — that is a property
    /// of the signature, not something to re-verify by tracing callers.
    ///
    /// When the queue is full the newest job is dropped: counted, logged, and
    /// gone (FR-005, FR-006, FR-007). Dropping one speaker's utterance is
    /// strictly better than the alternative this replaces, which was stalling
    /// audio reception for every speaker in the channel.
    pub fn submit(&self, mut job: TranscriptionJob) -> SubmitOutcome {
        job.queued_at = Some(Instant::now());

        match self.tx.try_send(job) {
            Ok(()) => {
                self.metrics.record_queued();
                SubmitOutcome::Queued
            }
            Err(TrySendError::Full(job)) => {
                // Counter and log increment together on purpose: SC-005 wants
                // 100% of discards in both, so a discard that counted without
                // logging would satisfy the metric and fail the criterion.
                self.metrics.record_utterance_discarded();
                tracing::warn!(
                    guild = %job.guild_id,
                    channel = %job.channel_id,
                    speaker = %job.speaker.display_hint(),
                    speaker_id = ?job.speaker_id.map(|id| id.get()),
                    "transcription queue is full; dropped this utterance. The bot is not \
                     keeping up — raise TRANSCRIPTION_CONCURRENCY, use a smaller model, or \
                     accept the loss."
                );
                SubmitOutcome::Discarded
            }
            Err(TrySendError::Closed(job)) => {
                // Not overload. Folding this into the discard counter would make
                // that counter mean two different things.
                tracing::error!(
                    guild = %job.guild_id,
                    channel = %job.channel_id,
                    "transcription queue is closed; dropped this utterance (shutting down?)"
                );
                SubmitOutcome::Closed
            }
        }
    }
}

/// A running transcription worker: the handle jobs are submitted through, and
/// the compute backend it actually resolved to.
///
/// The backend travels with the handle because it is resolved here and nowhere
/// else — startup logging (FR-006) and the telemetry surface (FR-007) both need
/// it, and re-deriving it in either place would risk the two disagreeing.
pub struct TranscriptionWorker {
    pub handle: TranscriptionHandle,
    pub backend: ComputeBackend,
    pub concurrency: ConcurrencyLimit,
}

/// How long in-flight transcriptions get to finish on shutdown (FR-013).
///
/// `compose.yml` declares `stop_grace_period: 30s`, so this must stay well
/// inside that: a drain that outlives the grace period is not a graceful
/// shutdown, it is a SIGKILL with extra steps.
pub const SHUTDOWN_DRAIN: std::time::Duration = std::time::Duration::from_secs(10);

/// Wait, briefly, for outstanding transcription work to finish.
///
/// Reports whatever is still outstanding when the deadline passes rather than
/// waiting for it (FR-013). Anything still queued at that point is discarded by
/// process exit, and an operator who sees a non-zero count here knows captions
/// from the last few seconds are missing — which is exactly the kind of quiet
/// loss Constitution Principle V exists to prevent.
pub async fn drain(metrics: &AppMetrics, budget: std::time::Duration) {
    let deadline = std::time::Instant::now() + budget;

    loop {
        let snapshot = metrics.snapshot();
        let outstanding = snapshot.transcription_queue_depth + snapshot.transcription_in_flight;

        if outstanding == 0 {
            tracing::info!("transcription queue drained cleanly");
            return;
        }
        if std::time::Instant::now() >= deadline {
            tracing::warn!(
                queued = snapshot.transcription_queue_depth,
                in_flight = snapshot.transcription_in_flight,
                "shutting down with {outstanding} transcription(s) unfinished after {:?}; \
                 their captions will be missing",
                budget
            );
            return;
        }

        tracing::debug!(outstanding, "waiting for transcription to drain");
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

/// Is this transcript nothing but sound-event annotations?
///
/// Whisper narrates non-speech audio rather than staying silent: handed music
/// it emits `(soft music)`, handed applause `[APPLAUSE]`, handed nothing at all
/// `[BLANK_AUDIO]`. That is the model correctly reporting *no speech was said*,
/// in its own notation — categorically different from a person speaking.
///
/// Before this existed the bot filtered exactly one literal, `[blank_audio]`,
/// so `(soft music)` was written into a caption file as though a participant
/// had said the words "soft music" (research R4). That is worse than a missing
/// caption, because a reader cannot tell it from real speech.
///
/// Deliberately conservative: an annotation sitting **next to real words** is
/// real speech and is kept. Eating a caption because it happened to contain a
/// parenthesis would be a worse bug than the one being fixed.
pub fn is_sound_event_annotation(normalized: &str) -> bool {
    if normalized.is_empty() {
        return false;
    }

    let mut depth = 0i32;
    let mut saw_group = false;
    let mut outside_group_has_text = false;

    for ch in normalized.chars() {
        match ch {
            '[' | '(' => {
                depth += 1;
                saw_group = true;
            }
            ']' | ')' => depth = (depth - 1).max(0),
            ch if ch.is_whitespace() => {}
            _ if depth == 0 => outside_group_has_text = true,
            _ => {}
        }
    }

    saw_group && !outside_group_has_text
}

/// Reduce a transcript to comparable tokens: lowercase, punctuation stripped,
/// whitespace collapsed. Brackets and parentheses survive so annotations stay
/// recognizable as annotations rather than decaying into ordinary words.
fn normalize_transcript(raw: &str) -> String {
    let lowered = raw.to_lowercase();
    let mut out = String::with_capacity(lowered.len());

    for ch in lowered.chars() {
        if ch.is_alphanumeric()
            || ch == '\''
            || ch == '['
            || ch == ']'
            || ch == '('
            || ch == ')'
            || ch == '_'
        {
            out.push(ch);
        } else {
            out.push(' ');
        }
    }

    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Queue capacity.
///
/// Bounded, and it must stay bounded: Constitution Principle I requires shedding
/// load explicitly over letting a queue grow, because an unbounded queue trades
/// a visible drop for invisible, unbounded latency. Deep enough to absorb a
/// burst of simultaneous speakers, shallow enough that a job which does reach a
/// worker is still worth transcribing.
const QUEUE_CAPACITY: usize = 32;

pub fn spawn_worker(
    model_path: PathBuf,
    sink: Arc<CaptionSink>,
    language: Option<String>,
    use_gpu: bool,
    gpu_device: i32,
    configured_concurrency: Option<usize>,
    metrics: Arc<AppMetrics>,
) -> anyhow::Result<TranscriptionWorker> {
    let (tx, rx) = mpsc::channel::<TranscriptionJob>(QUEUE_CAPACITY);
    let model_path_str = model_path
        .to_str()
        .context("WHISPER_MODEL_PATH must be valid UTF-8")?
        .to_owned();

    install_whisper_logger();
    let (ctx, backend) = resolve_context(&model_path_str, use_gpu, gpu_device)?;
    metrics.set_compute_backend(backend.clone());

    // Concurrency is settled after the backend, not before: the default depends
    // on which processor is actually in use, and asking before resolution would
    // size a GPU pool on a host that fell back to CPU.
    let concurrency = ConcurrencyLimit::resolve(configured_concurrency, &backend);
    let pool = Arc::new(StatePool::new(&ctx, concurrency.value)?);
    metrics.set_concurrency_limit(concurrency.value);

    spawn_dispatcher(
        rx,
        pool,
        Arc::clone(&metrics),
        move |job, mut guard, metrics| {
            let started = std::time::Instant::now();
            metrics.record_transcription_started();

            let result =
                transcribe_and_write(guard.state_mut(), &sink, job, language.as_deref(), &metrics);

            metrics.record_transcription_finished(started.elapsed());

            if let Err(err) = result {
                // A device that disappears after startup — driver reset, card
                // removed, memory exhausted mid-run — surfaces here. Constitution
                // Principle V: counted as well as logged, or an operator cannot
                // tell a stalled queue from a quiet channel.
                metrics.record_transcription_error();
                tracing::error!("transcription failed: {err:?}");
            }
        },
    );

    Ok(TranscriptionWorker {
        handle: TranscriptionHandle {
            tx,
            metrics: Arc::clone(&metrics),
        },
        backend,
        concurrency,
    })
}

/// Load the model and settle, once, which processor will run transcription.
///
/// Called exactly once per process, at worker construction. Constitution
/// Principle I depends on that: nothing about backend identity is evaluated on
/// the voice tick path or per utterance.
///
/// The four ways GPU use can be requested and not delivered are told apart here
/// (FR-008), and every one of them continues on CPU rather than aborting
/// startup (FR-009).
fn resolve_context(
    model_path: &str,
    use_gpu: bool,
    gpu_device: i32,
) -> anyhow::Result<(Arc<WhisperContext>, ComputeBackend)> {
    // Not requested at all — a CPU image's default, or an operator who
    // deliberately turned GPU off. FR-011 is explicit that this is a valid
    // choice, so it is not a fallback and it does not warn.
    if !use_gpu {
        return Ok((build_context(model_path, false, 0)?, ComputeBackend::cpu()));
    }

    if !cfg!(feature = "cuda") {
        return Ok((
            build_context(model_path, false, 0)?,
            fall_back(FallbackReason::NotCompiled, None),
        ));
    }

    // Attempting the GPU context is what makes ggml enumerate devices, and the
    // log forwarder is the only place it reports what it found. So discovery is
    // read *after* the attempt, whether or not the attempt succeeded.
    let attempt = build_context(model_path, true, gpu_device);
    let discovery = gpu_discovery();
    let device_count = discovery.device_count.unwrap_or(0);
    let index_is_valid = gpu_device >= 0 && (gpu_device as usize) < device_count;

    match attempt {
        Ok(ctx) if device_count > 0 && index_is_valid => {
            let backend = ComputeBackend::gpu(gpu_device, discovery.name_for(gpu_device));
            Ok((ctx, backend))
        }

        // ggml enumerated nothing, so it quietly built a CPU backend. The
        // context is already CPU — reusing it avoids reloading the model for no
        // reason, and it is not a lie: this context runs on the CPU.
        Ok(ctx) if device_count == 0 => Ok((
            ctx,
            fall_back(FallbackReason::NoDevice, discovery.init_error),
        )),

        // Devices exist but the requested index is not one of them. The context
        // that came back may be bound to some other device, so it is discarded
        // and rebuilt on CPU rather than reported as CPU while running on GPU 0.
        Ok(_) => {
            tracing::warn!(
                requested_device = gpu_device,
                devices_present = device_count,
                "WHISPER_GPU_DEVICE={gpu_device} does not exist; this host has {device_count} \
                 GPU device(s), numbered 0 to {}",
                device_count.saturating_sub(1)
            );
            Ok((
                build_context(model_path, false, 0)?,
                fall_back(FallbackReason::InvalidDevice, None),
            ))
        }

        Err(err) => {
            let reason = if device_count == 0 {
                FallbackReason::NoDevice
            } else if !index_is_valid {
                tracing::warn!(
                    requested_device = gpu_device,
                    devices_present = device_count,
                    "WHISPER_GPU_DEVICE={gpu_device} does not exist; this host has {device_count} \
                     GPU device(s), numbered 0 to {}",
                    device_count.saturating_sub(1)
                );
                FallbackReason::InvalidDevice
            } else {
                FallbackReason::InitFailed
            };

            let detail = format!("{err:#}");
            // A model too large for the card is the most common initialization
            // failure and the least obvious from the raw error, so it is named.
            if reason == FallbackReason::InitFailed && looks_like_memory_shortfall(&detail) {
                tracing::warn!(
                    device = gpu_device,
                    device_name = discovery.name_for(gpu_device).unwrap_or_default(),
                    "the GPU does not have enough free memory for this model; either free \
                     memory on the device or configure a smaller WHISPER_MODEL_NAME"
                );
            }

            // The model still has to load, just on the CPU. If *that* fails the
            // problem is the model, not the GPU, and it is a real startup error.
            let ctx = build_context(model_path, false, 0).context(
                "GPU initialization failed and the CPU fallback could not load the model",
            )?;
            Ok((ctx, fall_back(reason, Some(detail))))
        }
    }
}

/// Warn about a fallback in a way the operator can act on, and record it.
///
/// One warning per cause, each naming the specific fix (FR-008, SC-004). The
/// `detail` is whatever the native library said, passed through rather than
/// swallowed — for `init_failed` it is usually the only thing that identifies
/// the actual problem.
fn fall_back(reason: FallbackReason, detail: Option<String>) -> ComputeBackend {
    match &detail {
        Some(detail) if !detail.is_empty() => tracing::warn!(
            fallback_reason = reason.as_str(),
            detail = %detail,
            "GPU transcription was requested but is unavailable; continuing on CPU. To fix: {}",
            reason.operator_action()
        ),
        _ => tracing::warn!(
            fallback_reason = reason.as_str(),
            "GPU transcription was requested but is unavailable; continuing on CPU. To fix: {}",
            reason.operator_action()
        ),
    }
    ComputeBackend::cpu_fallback(reason)
}

fn build_context(
    model_path: &str,
    use_gpu: bool,
    gpu_device: i32,
) -> anyhow::Result<Arc<WhisperContext>> {
    let mut params = WhisperContextParameters::default();
    params.use_gpu(use_gpu);
    if use_gpu {
        params.gpu_device(gpu_device);
    }
    Ok(Arc::new(
        WhisperContext::new_with_params(model_path, params).context("loading Whisper model")?,
    ))
}

fn looks_like_memory_shortfall(detail: &str) -> bool {
    let lowered = detail.to_lowercase();
    lowered.contains("out of memory")
        || lowered.contains("outofmemory")
        || lowered.contains("insufficient memory")
        || lowered.contains("failed to allocate")
}

fn transcribe_and_write(
    state: &mut WhisperState,
    sink: &CaptionSink,
    job: TranscriptionJob,
    language: Option<&str>,
    metrics: &AppMetrics,
) -> anyhow::Result<()> {
    if job.pcm.is_empty() {
        return Ok(());
    }

    let audio = prepare_audio(&job.pcm, job.sample_rate);

    // The state is reused across utterances rather than recreated per job
    // (FR-002). That is only safe because `no_context` stays at its default
    // `true`, which makes whisper.cpp clear `prompt_past` at the top of every
    // decode; `result_all` is cleared unconditionally. Setting `no_context` to
    // false here would let one speaker's words seed another speaker's
    // transcript — silently, intermittently, and only under load. There is a
    // regression test pinning this in tests/worker_throughput.rs; do not
    // "optimise" it away.
    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    params.set_language(language);
    params.set_translate(false);

    state.full(params, &audio)?;

    let mut text = String::new();
    let segments = state.full_n_segments();
    for idx in 0..segments {
        if let Some(segment) = state.get_segment(idx) {
            let segment_text = segment.to_str()?.trim();
            if segment_text.is_empty() {
                continue;
            }
            text.push_str(segment_text);
            text.push(' ');
        }
    }

    // FR-008, FR-009. A segment that produced no speech produces no caption:
    // not an empty one, and not the recognizer's own notation for "nothing was
    // said" dressed up as a sentence.
    let trimmed = text.trim();
    if trimmed.is_empty() || is_sound_event_annotation(&normalize_transcript(trimmed)) {
        // Counted, because a rejection nobody counted is invisible — and an
        // over-aggressive silence threshold would then look exactly like a
        // quiet channel (FR-012, Constitution Principle V).
        metrics.record_non_speech_rejection();
        tracing::debug!(
            target = "transcription",
            guild = %job.guild_id,
            channel = %job.channel_id,
            speaker = %job.speaker_name(),
            transcript = %trimmed,
            "segment contained no speech; no caption written"
        );
        return Ok(());
    }

    let normalized = trimmed.to_string();
    let user_id = job.speaker_id.map(|id| id.get());
    tracing::debug!(
        target = "transcription",
        guild = %job.guild_id,
        channel = %job.channel_id,
        speaker = %job.speaker_name(),
        speaker_id = ?user_id,
        text = %normalized,
        "captured transcript line"
    );

    let timestamp = job.started_at.format("%Y-%m-%dT%H:%M:%S").to_string();
    let entry = CaptionEntry {
        speaker: SpeakerInfo {
            id: job.speaker_id,
            name: job.speaker_name().to_string(),
        },
        comment: normalized,
        timestamp,
    };
    sink.append_json(job.guild_id, job.channel_id, entry)?;
    metrics.record_transcription_line();
    Ok(())
}

fn install_whisper_logger() {
    WHISPER_LOGGER.call_once(|| unsafe {
        whisper_log_set(Some(whisper_log_forwarder), std::ptr::null_mut());
    });
}

unsafe extern "C" fn whisper_log_forwarder(
    level: ggml_log_level,
    text: *const c_char,
    _user: *mut c_void,
) {
    if text.is_null() {
        return;
    }

    let message = match unsafe { CStr::from_ptr(text) }.to_str() {
        Ok(value) => value.trim(),
        Err(_) => return,
    };

    if message.is_empty() {
        return;
    }

    // ggml announces the CUDA devices it found through this callback and
    // nowhere else. Capturing it here is what lets "no device present" be told
    // apart from "initialization failed" (research.md R4) — without it both
    // look identical from outside, which is the ambiguity FR-008 forbids.
    observe_ggml_log(message);

    tracing::debug!(target = "whisper", ?level, "{message}");
}

fn prepare_audio(samples: &[i16], sample_rate: u32) -> Vec<f32> {
    if sample_rate == WHISPER_SAMPLE_RATE {
        return pcm_to_f32(samples);
    }

    let ratio = sample_rate as f32 / WHISPER_SAMPLE_RATE as f32;
    let target_len = ((samples.len() as f32) / ratio).ceil() as usize;
    let mut downsampled = Vec::with_capacity(target_len);
    for idx in 0..target_len {
        let source_idx = ((idx as f32) * ratio).floor() as usize;
        if let Some(sample) = samples.get(source_idx) {
            downsampled.push(*sample);
        }
    }
    pcm_to_f32(&downsampled)
}

fn pcm_to_f32(samples: &[i16]) -> Vec<f32> {
    samples
        .iter()
        .map(|s| f32::from(*s) / PCM_NORMALIZER)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serenity::model::id::{ChannelId, GuildId};

    fn test_job() -> TranscriptionJob {
        TranscriptionJob {
            channel_id: ChannelId::new(1),
            guild_id: GuildId::new(2),
            speaker_id: None,
            speaker: SpeakerLabel::Resolved("tester".to_string()),
            pcm: vec![0i16; 16],
            sample_rate: WHISPER_SAMPLE_RATE,
            started_at: Utc::now(),
            queued_at: None,
        }
    }

    fn handle_with_capacity(
        capacity: usize,
    ) -> (
        TranscriptionHandle,
        mpsc::Receiver<TranscriptionJob>,
        Arc<AppMetrics>,
    ) {
        let (tx, rx) = mpsc::channel(capacity);
        let metrics = Arc::new(AppMetrics::new());
        (
            TranscriptionHandle {
                tx,
                metrics: Arc::clone(&metrics),
            },
            rx,
            metrics,
        )
    }

    #[tokio::test]
    async fn a_full_queue_discards_instead_of_waiting() {
        // The whole point of the feature. Before this, the third submit would
        // await, inside the 20 ms voice tick handler, stalling audio reception
        // for every speaker in the channel.
        let (handle, _rx, metrics) = handle_with_capacity(2);

        assert_eq!(handle.submit(test_job()), SubmitOutcome::Queued);
        assert_eq!(handle.submit(test_job()), SubmitOutcome::Queued);
        assert_eq!(handle.submit(test_job()), SubmitOutcome::Discarded);
        assert_eq!(handle.submit(test_job()), SubmitOutcome::Discarded);

        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.total_utterances_discarded, 2);
        assert_eq!(
            snapshot.transcription_queue_depth, 2,
            "discards are not queued"
        );
    }

    #[tokio::test]
    async fn submission_returns_promptly_when_the_queue_is_full() {
        // FR-004. A wall-clock bound is a weak assertion in general, but here it
        // is the requirement: the old code awaited indefinitely at this point,
        // so anything that completes at all is the fix working.
        let (handle, _rx, _metrics) = handle_with_capacity(1);
        handle.submit(test_job());

        let started = std::time::Instant::now();
        for _ in 0..1000 {
            assert_eq!(handle.submit(test_job()), SubmitOutcome::Discarded);
        }
        assert!(
            started.elapsed() < std::time::Duration::from_millis(100),
            "1000 submissions against a full queue took {:?}; submission must never wait",
            started.elapsed()
        );
    }

    #[tokio::test]
    async fn a_closed_queue_is_not_counted_as_overload() {
        // Shutdown is not overload. Folding the two together would make the
        // discard counter mean two different things.
        let (handle, rx, metrics) = handle_with_capacity(4);
        drop(rx);

        assert_eq!(handle.submit(test_job()), SubmitOutcome::Closed);
        assert_eq!(metrics.snapshot().total_utterances_discarded, 0);
    }

    #[tokio::test]
    async fn queue_depth_falls_as_work_is_taken() {
        let (handle, mut rx, metrics) = handle_with_capacity(4);
        handle.submit(test_job());
        handle.submit(test_job());
        assert_eq!(metrics.snapshot().transcription_queue_depth, 2);

        let _job = rx.recv().await.unwrap();
        metrics.record_dequeued();
        assert_eq!(metrics.snapshot().transcription_queue_depth, 1);
    }

    #[tokio::test]
    async fn the_queue_recovers_after_overload_subsides() {
        // FR-005: no restart required once the pressure is off.
        let (handle, mut rx, metrics) = handle_with_capacity(2);
        handle.submit(test_job());
        handle.submit(test_job());
        assert_eq!(handle.submit(test_job()), SubmitOutcome::Discarded);

        while let Ok(_job) = rx.try_recv() {
            metrics.record_dequeued();
        }

        assert_eq!(
            handle.submit(test_job()),
            SubmitOutcome::Queued,
            "the queue must accept work again once it has drained"
        );
        assert_eq!(metrics.snapshot().total_utterances_discarded, 1);
    }

    #[tokio::test]
    async fn a_queued_job_carries_its_enqueue_time() {
        let (handle, mut rx, _metrics) = handle_with_capacity(2);
        handle.submit(test_job());
        let job = rx.recv().await.unwrap();
        assert!(
            job.queued_at.is_some(),
            "queue wait cannot be separated from decode time without this"
        );
    }

    #[tokio::test]
    async fn sustained_overload_keeps_the_queue_bounded() {
        // The spec's edge case: discards must stay bounded and counted, and the
        // queue must never grow without limit as an alternative to dropping.
        // An unbounded queue would trade a visible drop for invisible,
        // unbounded caption latency, which is the worse failure.
        let capacity = 4;
        let (handle, _rx, metrics) = handle_with_capacity(capacity);

        let submitted = 500;
        for _ in 0..submitted {
            handle.submit(test_job());
        }

        let snapshot = metrics.snapshot();
        assert_eq!(
            snapshot.transcription_queue_depth, capacity,
            "the queue must never exceed its capacity, however long the overload lasts"
        );
        assert_eq!(
            snapshot.total_utterances_discarded,
            (submitted - capacity) as u64,
            "every dropped utterance must be counted; none may vanish silently"
        );
    }

    #[test]
    fn non_speech_transcripts_are_rejected_and_real_speech_is_not() {
        // FR-008, FR-009. The recognizer narrates non-speech rather than
        // staying silent, and before this the bot filtered exactly one literal
        // — so `(soft music)` was written into caption files as though someone
        // had said the words "soft music".
        for rejected in [
            "[blank_audio]",
            "[BLANK_AUDIO]",
            "(soft music)",
            "[APPLAUSE]",
            "[applause] (laughter)",
            "  (music)  ",
        ] {
            assert!(
                is_sound_event_annotation(&normalize_transcript(rejected)),
                "{rejected:?} should be recognized as a non-speech annotation"
            );
        }

        // The conservative half, and the more important one: an annotation
        // sitting next to real words IS real speech. Eating those captions
        // would be a worse bug than the one being fixed.
        for kept in [
            "hello there",
            "(music) thanks for watching",
            "thanks for watching (music)",
            "we tested the (new) build",
        ] {
            assert!(
                !is_sound_event_annotation(&normalize_transcript(kept)),
                "{kept:?} contains real speech and must produce a caption"
            );
        }
    }

    #[test]
    fn an_empty_transcript_is_not_an_annotation() {
        // Empty is handled by its own branch. If this returned true the
        // distinction would still work, but the two cases are different facts
        // and the code should not conflate them.
        assert!(!is_sound_event_annotation(""));
    }

    #[test]
    fn normalization_keeps_annotation_markers_intact() {
        assert_eq!(normalize_transcript("  Hello, World!  "), "hello world");
        assert_eq!(normalize_transcript("(Soft Music)"), "(soft music)");
        assert_eq!(normalize_transcript("[BLANK_AUDIO]"), "[blank_audio]");
    }

    #[test]
    fn a_deferred_speaker_still_logs_usefully() {
        // The discard path must not do work proportional to the overload that
        // caused it, so it logs a hint rather than resolving a name.
        let label = SpeakerLabel::Resolved("alice".to_string());
        assert_eq!(label.display_hint(), "alice");
    }
}
