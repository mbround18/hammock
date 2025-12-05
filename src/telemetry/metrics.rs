use std::{
    collections::VecDeque,
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use chrono::{TimeZone, Utc};
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct LineWindowSnapshot {
    pub last_1h: usize,
    pub last_30m: usize,
    pub last_15m: usize,
    pub last_5m: usize,
    pub last_1m: usize,
    pub last_30s: usize,
}

#[derive(Debug, Serialize)]
pub struct MetricsSnapshot {
    pub uptime_seconds: u64,
    pub total_transcribed_lines: u64,
    pub total_sessions_started: u64,
    pub total_sessions_completed: u64,
    pub last_transcription_at: Option<String>,
    pub line_windows: LineWindowSnapshot,
}

pub struct AppMetrics {
    start_time: Instant,
    total_transcribed_lines: AtomicU64,
    total_sessions_started: AtomicU64,
    total_sessions_completed: AtomicU64,
    last_transcription_epoch: AtomicU64,
    window_1h: LineWindow,
    window_30m: LineWindow,
    window_15m: LineWindow,
    window_5m: LineWindow,
    window_1m: LineWindow,
    window_30s: LineWindow,
}

impl AppMetrics {
    pub fn new() -> Self {
        Self {
            start_time: Instant::now(),
            total_transcribed_lines: AtomicU64::new(0),
            total_sessions_started: AtomicU64::new(0),
            total_sessions_completed: AtomicU64::new(0),
            last_transcription_epoch: AtomicU64::new(0),
            window_1h: LineWindow::new(Duration::from_secs(60 * 60)),
            window_30m: LineWindow::new(Duration::from_secs(30 * 60)),
            window_15m: LineWindow::new(Duration::from_secs(15 * 60)),
            window_5m: LineWindow::new(Duration::from_secs(5 * 60)),
            window_1m: LineWindow::new(Duration::from_secs(60)),
            window_30s: LineWindow::new(Duration::from_secs(30)),
        }
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
            last_transcription_at: self.last_transcription_iso8601(),
            line_windows: self.line_window_snapshot(now),
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
