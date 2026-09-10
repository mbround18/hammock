//! The transcription worker pool.
//!
//! Before this existed, the worker decoded exactly one chunk at a time: it
//! received a job, awaited a `spawn_blocking` to completion, and only then
//! looked at the queue again. Several speakers therefore serialized, and the
//! queue behind them grew for as long as the conversation lasted.
//!
//! The shape here is one `WhisperContext` shared by N `WhisperState`s, with a
//! semaphore bounding how many decode at once. That is the design whisper.cpp
//! itself ships — `whisper_full_parallel` runs `whisper_full_with_state` on
//! several threads against one context — and it is safe for the same reason:
//! `whisper_full_with_state` writes only through its `state` argument, and
//! everything it reads from the context is immutable during inference.
//!
//! See `specs/002-transcription-worker-throughput/research.md` R1 and R7.

use std::sync::{Arc, Mutex};

use anyhow::Context as _;
use tokio::sync::{Semaphore, mpsc};
use whisper_rs::{WhisperContext, WhisperState};

use crate::telemetry::{AppMetrics, BackendKind, ComputeBackend};

/// Hard ceiling on `TRANSCRIPTION_CONCURRENCY`, matching
/// `contracts/configuration.md`.
pub const MAX_CONCURRENCY: usize = 32;

/// Where the effective concurrency limit came from.
///
/// Reported at startup because "4 workers" alone does not tell an operator
/// whether their configuration was read (FR-012).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConcurrencySource {
    Configured,
    DefaultCpu { cores: usize },
    DefaultGpu,
}

impl ConcurrencySource {
    pub fn describe(self) -> String {
        match self {
            Self::Configured => "from TRANSCRIPTION_CONCURRENCY".to_string(),
            Self::DefaultCpu { cores } => {
                format!("default for cpu backend, {cores} cores available")
            }
            Self::DefaultGpu => "default for gpu backend".to_string(),
        }
    }
}

/// The resolved worker count and where it came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConcurrencyLimit {
    pub value: usize,
    pub source: ConcurrencySource,
}

impl ConcurrencyLimit {
    /// Settle the worker count from configuration, falling back to a default
    /// chosen for the backend actually in use.
    pub fn resolve(configured: Option<usize>, backend: &ComputeBackend) -> Self {
        match configured {
            Some(value) => Self {
                value,
                source: ConcurrencySource::Configured,
            },
            None => Self::default_for(backend),
        }
    }

    fn default_for(backend: &ComputeBackend) -> Self {
        match backend.kind() {
            // Memory, not core count, is what binds on a GPU: each decoder
            // state costs roughly 233 MB for `base` and several times that for
            // a large model, above a model shared by all of them. Two is enough
            // to overlap one decode with the next job's setup — which is where
            // the serialization stall actually shows — while staying safe on a
            // small card carrying a big model.
            BackendKind::Gpu => Self {
                value: 2,
                source: ConcurrencySource::DefaultGpu,
            },
            BackendKind::Cpu => {
                let cores = std::thread::available_parallelism()
                    .map(|n| n.get())
                    .unwrap_or(1);
                // Divided by four, and this is the part that is easy to get
                // wrong: whisper.cpp already parallelises a *single* decode
                // with `n_threads = min(4, hardware_concurrency)`. One worker
                // per core would oversubscribe the machine roughly fourfold and
                // run slower than doing less. The upper clamp exists because
                // throughput flattens well before four workers on realistic
                // channel sizes, while each one still costs memory.
                Self {
                    value: (cores / 4).clamp(1, 4),
                    source: ConcurrencySource::DefaultCpu { cores },
                }
            }
        }
    }
}

/// A fixed set of reusable resources, and the permits that bound how many are
/// checked out at once.
///
/// Generic over the resource purely so the concurrency mechanics can be tested
/// without a Whisper model — this is the part of the feature where a mistake
/// costs audio, and it should not be untestable on a CI runner with no model.
/// The only instantiation in production is [`StatePool`].
///
/// The permit count and the resource count are the same number by construction.
/// They are two encodings of one bound, and if they could diverge a job would
/// hold a permit and then wait on an empty pool — a second queue, invisible in
/// every metric.
pub struct ResourcePool<T> {
    permits: Semaphore,
    resources: Mutex<Vec<T>>,
}

/// The production pool: one reusable decoder state per unit of concurrency.
pub type StatePool = ResourcePool<WhisperState>;

impl<T> ResourcePool<T> {
    pub fn from_resources(resources: Vec<T>) -> Self {
        Self {
            permits: Semaphore::new(resources.len()),
            resources: Mutex::new(resources),
        }
    }

    /// Take a resource, waiting for a permit if all are busy.
    ///
    /// The returned guard puts it back on drop — including on the error and
    /// panic paths. Returning it only on success would shrink the pool by one
    /// per failure until nothing decoded at all, and nothing about that failure
    /// mode is visible until it is total.
    pub async fn acquire(self: &Arc<Self>) -> Option<ResourceGuard<T>> {
        let permit = self.permits.acquire().await.ok()?;
        permit.forget();

        let resource = self
            .resources
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .pop()?;

        Some(ResourceGuard {
            pool: Arc::clone(self),
            resource: Some(resource),
        })
    }

    #[cfg(test)]
    fn available(&self) -> usize {
        self.resources
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .len()
    }
}

impl StatePool {
    /// Allocate every decoder state up front.
    ///
    /// Eager on purpose. These allocations are what fail when concurrency is
    /// set higher than the accelerator can hold, and failing here turns that
    /// into a startup error naming the constraint rather than an opaque failure
    /// partway through a conversation.
    pub fn new(ctx: &WhisperContext, concurrency: usize) -> anyhow::Result<Self> {
        let mut states = Vec::with_capacity(concurrency);
        for index in 0..concurrency {
            let state = ctx.create_state().with_context(|| {
                format!(
                    "failed to allocate decoder state {} of {concurrency}. Each concurrent \
                     transcription needs its own decoder state (a few hundred MB for a small \
                     model, more for a large one), so this usually means \
                     TRANSCRIPTION_CONCURRENCY is higher than this device's memory allows. \
                     Lower it, or use a smaller WHISPER_MODEL_NAME.",
                    index + 1
                )
            })?;
            states.push(state);
        }
        Ok(Self::from_resources(states))
    }
}

/// A borrowed resource that returns itself to the pool no matter how the job
/// ends.
pub struct ResourceGuard<T> {
    pool: Arc<ResourcePool<T>>,
    resource: Option<T>,
}

/// The production guard.
pub type StateGuard = ResourceGuard<WhisperState>;

impl<T> ResourceGuard<T> {
    pub fn state_mut(&mut self) -> &mut T {
        self.resource
            .as_mut()
            .expect("resource is only taken in Drop, which consumes the guard")
    }
}

impl<T> Drop for ResourceGuard<T> {
    fn drop(&mut self) {
        if let Some(resource) = self.resource.take() {
            self.pool
                .resources
                .lock()
                .unwrap_or_else(|err| err.into_inner())
                .push(resource);
        }
        // The permit is restored after the resource, never before: a waiter
        // woken by the permit must find a resource waiting for it.
        self.pool.permits.add_permits(1);
    }
}

/// Receive jobs and hand them to workers, never more than `concurrency` at once.
///
/// This task owns the receiver. It is the single place a job leaves the queue,
/// which is what keeps the depth gauge honest — a counter decremented in two
/// places drifts, and a drifting gauge is worse than no gauge.
pub fn spawn_dispatcher<F>(
    mut rx: mpsc::Receiver<super::TranscriptionJob>,
    pool: Arc<StatePool>,
    metrics: Arc<AppMetrics>,
    run_job: F,
) where
    F: Fn(super::TranscriptionJob, StateGuard, Arc<AppMetrics>) + Send + Sync + 'static,
{
    let run_job = Arc::new(run_job);

    tokio::spawn(async move {
        while let Some(mut job) = rx.recv().await {
            metrics.record_dequeued();

            // The job's speaker name may still be a deferred Discord lookup.
            // Here is where it is resolved: off the 20 ms receive path, before
            // the blocking decode, which is the only point where awaiting a
            // network round trip costs nothing that matters.
            job.resolve_speaker().await;

            let Some(guard) = pool.acquire().await else {
                tracing::error!("decoder state pool is closed; dropping job");
                continue;
            };

            let metrics = Arc::clone(&metrics);
            let run_job = Arc::clone(&run_job);

            // Not awaited. Awaiting here is exactly what made every speaker
            // queue behind every other speaker, and is the bug this feature
            // exists to fix.
            tokio::spawn(async move {
                let join_metrics = Arc::clone(&metrics);
                if let Err(err) =
                    tokio::task::spawn_blocking(move || run_job(job, guard, metrics)).await
                {
                    // A panic in one decode. The guard's Drop has already
                    // returned the state, so the pool is intact and the next
                    // job proceeds (FR-008).
                    join_metrics.record_transcription_error();
                    tracing::error!("transcription task join error: {err}");
                }
            });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telemetry::ComputeBackend;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn gpu_default_is_small_and_memory_shaped() {
        let limit = ConcurrencyLimit::resolve(None, &ComputeBackend::gpu(0, None));
        assert_eq!(limit.value, 2);
        assert_eq!(limit.source, ConcurrencySource::DefaultGpu);
    }

    #[test]
    fn cpu_default_leaves_room_for_whispers_own_threads() {
        // whisper.cpp uses up to 4 threads per decode, so the default divides
        // available parallelism by 4 rather than claiming a worker per core.
        let limit = ConcurrencyLimit::default_for(&ComputeBackend::cpu());
        let cores = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1);
        assert_eq!(limit.value, (cores / 4).clamp(1, 4));
        assert!((1..=4).contains(&limit.value));
    }

    #[test]
    fn configuration_wins_over_the_default() {
        let limit = ConcurrencyLimit::resolve(Some(7), &ComputeBackend::cpu());
        assert_eq!(limit.value, 7);
        assert_eq!(limit.source, ConcurrencySource::Configured);
    }

    #[test]
    fn the_source_is_reportable() {
        assert_eq!(
            ConcurrencySource::Configured.describe(),
            "from TRANSCRIPTION_CONCURRENCY"
        );
        assert_eq!(
            ConcurrencySource::DefaultGpu.describe(),
            "default for gpu backend"
        );
        assert!(
            ConcurrencySource::DefaultCpu { cores: 32 }
                .describe()
                .contains("32 cores")
        );
    }

    // ---------------------------------------------------------------------
    // Pool mechanics. These are the assertions that matter most in this
    // feature: a bound that does not hold, or a resource that is not returned,
    // costs audio and is invisible until it is total.
    // ---------------------------------------------------------------------

    #[tokio::test]
    async fn concurrency_is_actually_bounded() {
        let pool = Arc::new(ResourcePool::from_resources(vec![1u32, 2, 3]));
        let in_flight = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));

        let mut handles = Vec::new();
        for _ in 0..24 {
            let pool = Arc::clone(&pool);
            let in_flight = Arc::clone(&in_flight);
            let peak = Arc::clone(&peak);
            handles.push(tokio::spawn(async move {
                let _guard = pool.acquire().await.expect("pool is open");
                let now = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(now, Ordering::SeqCst);
                tokio::task::yield_now().await;
                in_flight.fetch_sub(1, Ordering::SeqCst);
            }));
        }
        for handle in handles {
            handle.await.unwrap();
        }

        assert_eq!(
            peak.load(Ordering::SeqCst),
            3,
            "24 jobs against a pool of 3 must never have more than 3 in flight"
        );
        assert_eq!(pool.available(), 3, "every resource must come back");
    }

    #[tokio::test]
    async fn a_resource_returns_to_the_pool_after_a_panic() {
        // FR-008 and the "pool must not shrink permanently" edge case. A pool
        // that leaks one resource per failure keeps working while quietly
        // shrinking to nothing, so this is the only assertion that catches it.
        let pool = Arc::new(ResourcePool::from_resources(vec![7u32]));

        let panicking = {
            let pool = Arc::clone(&pool);
            tokio::spawn(async move {
                let _guard = pool.acquire().await.expect("pool is open");
                panic!("decode blew up");
            })
        };
        assert!(panicking.await.is_err(), "the task should have panicked");

        assert_eq!(pool.available(), 1, "the resource must be back in the pool");

        // And the pool must still hand it out.
        let guard = pool.acquire().await.expect("pool still works");
        assert_eq!(*guard.resource.as_ref().unwrap(), 7);
    }

    #[tokio::test]
    async fn a_resource_returns_to_the_pool_after_an_error() {
        let pool = Arc::new(ResourcePool::from_resources(vec![1u32]));
        {
            let mut guard = pool.acquire().await.unwrap();
            *guard.state_mut() = 2;
            // Guard dropped here on what stands in for an error path.
        }
        assert_eq!(pool.available(), 1);
        let guard = pool.acquire().await.unwrap();
        assert_eq!(
            *guard.resource.as_ref().unwrap(),
            2,
            "the same resource comes back, mutations and all — this is what makes reuse reuse"
        );
    }

    #[tokio::test]
    async fn a_waiter_woken_by_a_permit_always_finds_a_resource() {
        // The permit is released after the resource is pushed back, never
        // before. If that order were reversed a woken waiter could find an
        // empty pool and stall forever holding a permit.
        let pool = Arc::new(ResourcePool::from_resources(vec![1u32]));
        let held = pool.acquire().await.unwrap();

        let waiter = {
            let pool = Arc::clone(&pool);
            tokio::spawn(async move { pool.acquire().await.map(|mut g| *g.state_mut()) })
        };
        tokio::task::yield_now().await;
        drop(held);

        assert_eq!(waiter.await.unwrap(), Some(1));
    }

    #[tokio::test]
    async fn one_slow_job_does_not_block_the_others() {
        // The spec's edge case: a very long utterance from one speaker must not
        // prevent other speakers' shorter utterances from being transcribed.
        // Before the pool existed this was impossible — the worker awaited each
        // decode before looking at the queue again.
        let pool = Arc::new(ResourcePool::from_resources(vec![1u32, 2]));
        let finished_short = Arc::new(AtomicUsize::new(0));

        let long_job = {
            let pool = Arc::clone(&pool);
            tokio::spawn(async move {
                let _guard = pool.acquire().await.unwrap();
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            })
        };
        tokio::task::yield_now().await;

        // Short jobs run against the remaining resource while the long one
        // holds its own.
        for _ in 0..5 {
            let _guard = pool.acquire().await.unwrap();
            finished_short.fetch_add(1, Ordering::SeqCst);
        }

        assert_eq!(
            finished_short.load(Ordering::SeqCst),
            5,
            "short jobs must complete while a long one is still running"
        );
        assert!(
            !long_job.is_finished(),
            "the long job should still be running"
        );
        long_job.await.unwrap();
    }
}
