use std::{
    collections::VecDeque,
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use chrono::{TimeZone, Utc};
use serde::Serialize;

use super::ComputeBackend;

#[derive(Debug, Serialize)]
pub struct LineWindowSnapshot {
    pub last_1h: usize,
    pub last_30m: usize,
    pub last_15m: usize,
    pub last_5m: usize,
    pub last_1m: usize,
    pub last_30s: usize,
}

/// Distribution of transcription wall time.
///
/// `count` and `total_ms` are lifetime totals, so a mean is derivable without
/// publishing one. The percentiles are over a bounded *recent* window instead:
/// a p95 polluted by a slow first decode an hour ago answers nothing about
/// whether the system is keeping up now, which is the question SC-007 asks.
/// The two are deliberately different, and the field names say so.
#[derive(Debug, Serialize)]
pub struct DurationSnapshot {
    pub count: u64,
    pub total_ms: u64,
    pub p50_ms: u64,
    pub p95_ms: u64,
    pub max_ms: u64,
}

#[derive(Debug, Serialize)]
pub struct MetricsSnapshot {
    pub uptime_seconds: u64,
    pub total_transcribed_lines: u64,
    pub total_sessions_started: u64,
    pub total_sessions_completed: u64,
    /// Transcription attempts that failed after startup — a device lost mid-run,
    /// a decode error, a worker that panicked. Counted so a stalled queue is
    /// distinguishable from a quiet channel (Constitution Principle V).
    pub total_transcription_errors: u64,
    /// Utterances dropped because the transcription queue was full (FR-006).
    /// Non-zero means work was lost, not merely delayed.
    pub total_utterances_discarded: u64,
    /// Jobs enqueued and not yet picked up by the dispatcher (FR-009).
    pub transcription_queue_depth: usize,
    /// Jobs currently decoding (FR-009). Never exceeds the concurrency limit.
    pub transcription_in_flight: usize,
    /// The resolved worker count (FR-012). Constant for the process lifetime.
    pub transcription_concurrency_limit: usize,
    /// Decode wall time (feature 002, FR-010).
    pub transcription_duration_ms: DurationSnapshot,
    /// Segments transcribed but producing no caption, because the transcript
    /// was empty or consisted solely of the recognizer's non-speech
    /// annotations (feature 003, FR-012).
    ///
    /// Distinct from `total_utterances_discarded`, and the distinction matters:
    /// a discard means the queue was full and work was **lost**; a rejection
    /// means the audio contained no speech and nothing was lost. One says the
    /// bot is failing, the other says it is working.
    pub total_segments_rejected_as_non_speech: u64,
    /// Distribution of produced utterance durations (feature 003, FR-011).
    ///
    /// How long the *audio* was — not how long it took to transcribe, which is
    /// `transcription_duration_ms`.
    pub utterance_length_ms: DurationSnapshot,
    pub last_transcription_at: Option<String>,
    pub line_windows: LineWindowSnapshot,
    /// The backend transcription is running on (FR-007).
    ///
    /// Skipped here on purpose: the telemetry contract places `compute_backend`
    /// as a *sibling* of `metrics` on `/k8s/metrics`, not inside it, so
    /// `server.rs` lifts this to the top level. Serializing it in both places
    /// would publish the same fact twice under two different paths.
    ///
    /// `None` only before the transcription worker has been constructed. In
    /// practice never observable: `main.rs` builds the worker before the HTTP
    /// server binds.
    #[serde(skip)]
    pub compute_backend: Option<ComputeBackend>,
}

pub struct AppMetrics {
    start_time: Instant,
    total_transcribed_lines: AtomicU64,
    total_sessions_started: AtomicU64,
    total_sessions_completed: AtomicU64,
    total_transcription_errors: AtomicU64,
    total_utterances_discarded: AtomicU64,
    /// Queue depth and in-flight count are gauges, not counters: they go down
    /// as well as up, and their *current* value is the whole signal.
    transcription_queue_depth: AtomicUsize,
    transcription_in_flight: AtomicUsize,
    /// Set once when the worker pool is built, like `compute_backend`. A limit
    /// that appeared to change is a limit an operator cannot reason about.
    transcription_concurrency_limit: OnceLock<usize>,
    transcription_durations: DurationWindow,
    total_segments_rejected_as_non_speech: AtomicU64,
    utterance_lengths: DurationWindow,
    last_transcription_epoch: AtomicU64,
    window_1h: LineWindow,
    window_30m: LineWindow,
    window_15m: LineWindow,
    window_5m: LineWindow,
    window_1m: LineWindow,
    window_30s: LineWindow,
    /// Set once at worker construction and never again — the backend is
    /// immutable for the process lifetime, so this is a fact to record rather
    /// than a counter to increment.
    compute_backend: OnceLock<ComputeBackend>,
}

impl AppMetrics {
    pub fn new() -> Self {
        Self {
            start_time: Instant::now(),
            total_transcribed_lines: AtomicU64::new(0),
            total_sessions_started: AtomicU64::new(0),
            total_sessions_completed: AtomicU64::new(0),
            total_transcription_errors: AtomicU64::new(0),
            total_utterances_discarded: AtomicU64::new(0),
            transcription_queue_depth: AtomicUsize::new(0),
            transcription_in_flight: AtomicUsize::new(0),
            transcription_concurrency_limit: OnceLock::new(),
            transcription_durations: DurationWindow::new(),
            total_segments_rejected_as_non_speech: AtomicU64::new(0),
            utterance_lengths: DurationWindow::new(),
            last_transcription_epoch: AtomicU64::new(0),
            window_1h: LineWindow::new(Duration::from_secs(60 * 60)),
            window_30m: LineWindow::new(Duration::from_secs(30 * 60)),
            window_15m: LineWindow::new(Duration::from_secs(15 * 60)),
            window_5m: LineWindow::new(Duration::from_secs(5 * 60)),
            window_1m: LineWindow::new(Duration::from_secs(60)),
            window_30s: LineWindow::new(Duration::from_secs(30)),
            compute_backend: OnceLock::new(),
        }
    }

    /// Record the resolved compute backend. Only the first call takes effect;
    /// later calls are ignored, because a backend that appeared to change would
    /// be a lie about what the process actually bound at startup.
    pub fn set_compute_backend(&self, backend: ComputeBackend) {
        let _ = self.compute_backend.set(backend);
    }

    pub fn record_transcription_line(&self) {
        let now = Instant::now();
        self.total_transcribed_lines.fetch_add(1, Ordering::Relaxed);
        self.window_1h.record(now);
        self.window_30m.record(now);
        self.window_15m.record(now);
        self.window_5m.record(now);
        self.window_1m.record(now);
        self.window_30s.record(now);

        let epoch = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        self.last_transcription_epoch
            .store(epoch, Ordering::Relaxed);
    }

    pub fn record_transcription_error(&self) {
        self.total_transcription_errors
            .fetch_add(1, Ordering::Relaxed);
    }

    /// One utterance dropped because the queue was full.
    ///
    /// Callers MUST log at the same site with guild, channel and speaker.
    /// SC-005 requires 100% of discards in both a counter and a log line, so a
    /// discard that incremented this without logging would pass the metric and
    /// fail the criterion.
    pub fn record_utterance_discarded(&self) {
        self.total_utterances_discarded
            .fetch_add(1, Ordering::Relaxed);
    }

    /// A job was accepted onto the queue.
    pub fn record_queued(&self) {
        self.transcription_queue_depth
            .fetch_add(1, Ordering::Relaxed);
    }

    /// A job left the queue and entered a worker.
    ///
    /// Must be called from exactly one place — the dispatcher's receive — or
    /// the depth gauge drifts and becomes worse than no metric at all.
    pub fn record_dequeued(&self) {
        // Saturating rather than wrapping: a depth that underflowed to
        // usize::MAX would read as catastrophic overload and send an operator
        // chasing a bug that is in this counter.
        let _ = self.transcription_queue_depth.fetch_update(
            Ordering::Relaxed,
            Ordering::Relaxed,
            |depth| Some(depth.saturating_sub(1)),
        );
    }

    pub fn record_transcription_started(&self) {
        self.transcription_in_flight.fetch_add(1, Ordering::Relaxed);
    }

    /// A decode finished, successfully or not. `elapsed` is decode wall time.
    pub fn record_transcription_finished(&self, elapsed: Duration) {
        let _ = self.transcription_in_flight.fetch_update(
            Ordering::Relaxed,
            Ordering::Relaxed,
            |in_flight| Some(in_flight.saturating_sub(1)),
        );
        self.transcription_durations.record(elapsed);
    }

    /// One segment transcribed and found to contain no speech, so no caption
    /// was written (FR-012).
    ///
    /// Counted separately from a queue-full discard on purpose. Merging them
    /// would tell an operator that work was being lost when it was not.
    pub fn record_non_speech_rejection(&self) {
        self.total_segments_rejected_as_non_speech
            .fetch_add(1, Ordering::Relaxed);
    }

    /// The duration of one produced utterance (FR-011).
    ///
    /// The shape of this distribution is how segmentation misconfiguration is
    /// detected: a median collapsing toward the minimum means the silence
    /// threshold is too low, one pinned at the maximum means boundaries are
    /// coming from the length cap rather than from speech.
    pub fn record_utterance_length(&self, length: Duration) {
        self.utterance_lengths.record(length);
    }

    /// Record the resolved worker count. Only the first call takes effect.
    pub fn set_concurrency_limit(&self, limit: usize) {
        let _ = self.transcription_concurrency_limit.set(limit);
    }

    pub fn record_session_started(&self) {
        self.total_sessions_started.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_session_completed(&self) {
        self.total_sessions_completed
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> MetricsSnapshot {
        let now = Instant::now();
        MetricsSnapshot {
            uptime_seconds: self.start_time.elapsed().as_secs(),
            total_transcribed_lines: self.total_transcribed_lines.load(Ordering::Relaxed),
            total_sessions_started: self.total_sessions_started.load(Ordering::Relaxed),
            total_sessions_completed: self.total_sessions_completed.load(Ordering::Relaxed),
            total_transcription_errors: self.total_transcription_errors.load(Ordering::Relaxed),
            total_utterances_discarded: self.total_utterances_discarded.load(Ordering::Relaxed),
            transcription_queue_depth: self.transcription_queue_depth.load(Ordering::Relaxed),
            transcription_in_flight: self.transcription_in_flight.load(Ordering::Relaxed),
            transcription_concurrency_limit: self
                .transcription_concurrency_limit
                .get()
                .copied()
                .unwrap_or(0),
            transcription_duration_ms: self.transcription_durations.snapshot(),
            total_segments_rejected_as_non_speech: self
                .total_segments_rejected_as_non_speech
                .load(Ordering::Relaxed),
            utterance_length_ms: self.utterance_lengths.snapshot(),
            last_transcription_at: self.last_transcription_iso8601(),
            line_windows: self.line_window_snapshot(now),
            compute_backend: self.compute_backend.get().cloned(),
        }
    }

    fn line_window_snapshot(&self, now: Instant) -> LineWindowSnapshot {
        LineWindowSnapshot {
            last_1h: self.window_1h.count(now),
            last_30m: self.window_30m.count(now),
            last_15m: self.window_15m.count(now),
            last_5m: self.window_5m.count(now),
            last_1m: self.window_1m.count(now),
            last_30s: self.window_30s.count(now),
        }
    }

    fn last_transcription_iso8601(&self) -> Option<String> {
        let epoch = self.last_transcription_epoch.load(Ordering::Relaxed);
        if epoch == 0 {
            return None;
        }
        Utc.timestamp_opt(epoch as i64, 0)
            .single()
            .map(|dt| dt.to_rfc3339())
    }
}

impl Default for AppMetrics {
    fn default() -> Self {
        Self::new()
    }
}

/// Lifetime totals plus a bounded ring of recent samples for percentiles.
///
/// Bounded on purpose. Keeping every sample would grow without limit in a
/// process designed to run unattended for weeks, and percentiles over all time
/// answer a question nobody asks — an operator looking at p95 wants to know
/// about now, not about a cold cache at startup.
struct DurationWindow {
    count: AtomicU64,
    total_ms: AtomicU64,
    recent: Mutex<VecDeque<u64>>,
}

impl DurationWindow {
    /// Enough samples for a meaningful p95, small enough to be free. At the
    /// bot's chunk cadence this is roughly the last few minutes of a busy
    /// channel.
    const CAPACITY: usize = 256;

    fn new() -> Self {
        Self {
            count: AtomicU64::new(0),
            total_ms: AtomicU64::new(0),
            recent: Mutex::new(VecDeque::with_capacity(Self::CAPACITY)),
        }
    }

    fn record(&self, elapsed: Duration) {
        let millis = elapsed.as_millis().min(u64::MAX as u128) as u64;
        self.count.fetch_add(1, Ordering::Relaxed);
        self.total_ms.fetch_add(millis, Ordering::Relaxed);

        let mut recent = self.recent.lock().unwrap_or_else(|err| err.into_inner());
        if recent.len() == Self::CAPACITY {
            recent.pop_front();
        }
        recent.push_back(millis);
    }

    fn snapshot(&self) -> DurationSnapshot {
        let recent = self.recent.lock().unwrap_or_else(|err| err.into_inner());
        let mut sorted: Vec<u64> = recent.iter().copied().collect();
        drop(recent);
        sorted.sort_unstable();

        DurationSnapshot {
            count: self.count.load(Ordering::Relaxed),
            total_ms: self.total_ms.load(Ordering::Relaxed),
            p50_ms: percentile(&sorted, 50),
            p95_ms: percentile(&sorted, 95),
            max_ms: sorted.last().copied().unwrap_or(0),
        }
    }
}

/// Nearest-rank percentile over an ascending slice. Zero for an empty slice —
/// no samples means no latency to report, not a latency of zero, but the field
/// has to hold something and 0 alongside `count: 0` is unambiguous.
fn percentile(sorted: &[u64], p: usize) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let rank = (p * sorted.len()).div_ceil(100).max(1);
    sorted[rank - 1]
}

struct LineWindow {
    horizon: Duration,
    points: Mutex<VecDeque<Instant>>,
}

impl LineWindow {
    fn new(horizon: Duration) -> Self {
        Self {
            horizon,
            points: Mutex::new(VecDeque::new()),
        }
    }

    fn record(&self, now: Instant) {
        let mut points = self.points.lock().unwrap();
        points.push_back(now);
        self.prune(&mut points, now);
    }

    fn count(&self, now: Instant) -> usize {
        let mut points = self.points.lock().unwrap();
        self.prune(&mut points, now);
        points.len()
    }

    fn prune(&self, points: &mut VecDeque<Instant>, now: Instant) {
        while let Some(front) = points.front() {
            if now.duration_since(*front) > self.horizon {
                points.pop_front();
            } else {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles_use_nearest_rank() {
        let sorted: Vec<u64> = (1..=100).collect();
        assert_eq!(percentile(&sorted, 50), 50);
        assert_eq!(percentile(&sorted, 95), 95);
        assert_eq!(percentile(&[], 50), 0);
        assert_eq!(percentile(&[7], 95), 7);
    }

    #[test]
    fn duration_window_keeps_lifetime_totals_but_bounded_percentiles() {
        let window = DurationWindow::new();
        for ms in 1..=(DurationWindow::CAPACITY as u64 + 50) {
            window.record(Duration::from_millis(ms));
        }
        let snap = window.snapshot();

        // Totals are lifetime: every sample counted.
        assert_eq!(snap.count, DurationWindow::CAPACITY as u64 + 50);
        // Percentiles are windowed: the earliest samples have aged out, so the
        // max reflects recent traffic rather than all history.
        assert_eq!(snap.max_ms, DurationWindow::CAPACITY as u64 + 50);
        assert!(snap.p50_ms > 50, "early small samples should have aged out");
    }

    #[test]
    fn gauges_never_underflow() {
        let metrics = AppMetrics::new();
        // A decrement with nothing outstanding must not wrap to usize::MAX,
        // which would read as catastrophic overload.
        metrics.record_dequeued();
        metrics.record_transcription_finished(Duration::from_millis(1));
        let snap = metrics.snapshot();
        assert_eq!(snap.transcription_queue_depth, 0);
        assert_eq!(snap.transcription_in_flight, 0);
    }

    #[test]
    fn queue_depth_tracks_enqueue_and_dequeue() {
        let metrics = AppMetrics::new();
        metrics.record_queued();
        metrics.record_queued();
        assert_eq!(metrics.snapshot().transcription_queue_depth, 2);
        metrics.record_dequeued();
        assert_eq!(metrics.snapshot().transcription_queue_depth, 1);
    }

    #[test]
    fn concurrency_limit_is_set_once() {
        let metrics = AppMetrics::new();
        metrics.set_concurrency_limit(4);
        metrics.set_concurrency_limit(9);
        assert_eq!(metrics.snapshot().transcription_concurrency_limit, 4);
    }
}
