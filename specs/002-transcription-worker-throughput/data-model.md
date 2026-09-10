# Phase 1 Data Model: Transcription Worker Throughput and Load Shedding

**Feature**: [spec.md](./spec.md) | **Plan**: [plan.md](./plan.md)

This feature persists nothing. The entities below are in-process values: a job in
flight, the pool that decodes it, and the counters that describe both.

## Entity: TranscriptionJob

One speaker's segment of audio awaiting transcription. Exists today
(`src/transcription.rs:25-33`); this feature changes two things about it.

| Field | Type | Change | Note |
|---|---|---|---|
| `channel_id` | `ChannelId` | unchanged | needed for the discard log line (FR-007) |
| `guild_id` | `GuildId` | unchanged | needed for the discard log line (FR-007) |
| `speaker_id` | `Option<UserId>` | unchanged | |
| `speaker_name` | `String` | **becomes deferred** | see below |
| `pcm` | `Vec<i16>` | unchanged | |
| `sample_rate` | `u32` | unchanged | |
| `started_at` | `DateTime<Utc>` | unchanged | the clock SC-001's latency is measured from |
| `queued_at` | `Instant` | **new** | set at successful enqueue; the queue-wait half of FR-010 |

**`speaker_name` becomes deferred.** Today it is resolved before the job is
built, by an awaited Discord HTTP call on the voice tick path (research R5).
Carrying the `UserId` and resolving the display name inside the worker moves that
await somewhere it costs nothing. Model it as an enum so the "not yet resolved"
state is unrepresentable-by-accident:

```text
SpeakerLabel =
  | Resolved(String)      // cache hit on the tick path, or a placeholder label
  | Deferred(UserId)      // resolve in the worker, off the receive path
```

A cache hit is not an await and can stay on the tick path; only the miss defers.
Subject to the scope decision in [plan.md](./plan.md) — if that is rejected, this
field stays a plain `String` and nothing else in this document changes.

**Validation**: `pcm` non-empty. An empty job is dropped at the submission site
without counting as a discard — it is not overload, and counting it would make
the FR-006 counter mean two different things.

## Entity: Worker

A unit of transcription concurrency owning reusable decoder state.

| Field | Type | Description |
|---|---|---|
| `state` | `WhisperState` | Created once at pool construction, reused for every job this worker handles (FR-002). |
| `index` | `usize` | Stable identity for logs, so a repeatedly failing worker is identifiable. |

**Lifecycle**: created eagerly at startup, never recreated. Eager creation is
deliberate — it turns "the accelerator cannot hold this much concurrent state"
into a startup failure with a clear message rather than an opaque failure at
first use, which is what the spec's edge case requires. Per-state cost is ~233 MB
for `base` on CUDA (research R4).

**Invariant (FR-003)**: a worker's state must produce the same transcript for the
same audio regardless of what it decoded before. This holds because
`prompt_past` and `result_all` are both cleared per decode while `no_context`
stays at its default `true` (research R2).

**This invariant is one line of code from being violated.** Calling
`set_no_context(false)` anywhere in the decode path would make one speaker's
words leak into another speaker's caption, silently and intermittently. It must
be pinned by a test: decode fixture A, then B, on one reused state; assert B's
transcript is byte-identical to B decoded on a fresh state. That test is the
whole of FR-003's enforcement.

## Entity: WorkerPool

| Field | Type | Description |
|---|---|---|
| `concurrency` | `usize` | The bound. Equal to both the permit count and the state count — see below. |
| `permits` | `Semaphore` | `concurrency` permits. Acquiring one is what makes FR-001's limit real. |
| `states` | `Mutex<Vec<WhisperState>>` | The `concurrency` pre-created states. Pop to use, push to return. |
| `in_flight` | `AtomicUsize` | Jobs currently decoding (FR-009). |

**Why permits and states are the same number**: they are two encodings of one
bound, and keeping them equal means a worker that holds a permit is guaranteed a
state without blocking. If they could diverge, a job could hold a permit and then
wait on an empty state pool — a second, invisible queue.

**Invariant**: a state is returned to `states` on **every** exit path, including
error and panic. Returning only on success shrinks the pool by one per failure
until nothing decodes at all, which is exactly the "pool must not shrink
permanently" edge case (FR-008). The return belongs in a guard whose `Drop` runs
regardless of outcome, not at the end of the happy path.

**State transitions**: none at the pool level. `concurrency` is resolved once at
startup and is immutable, like feature 001's `ComputeBackend` — and for the same
reason: a limit that appears to change is a limit an operator cannot reason about.

## Entity: Queue

The bounded buffer between the receive path and the pool.

| Property | Value |
|---|---|
| Type | `tokio::sync::mpsc` bounded channel |
| Capacity | existing 32, revisited during tasks |
| Producer | the voice tick handler, via a **synchronous** `try_send` |
| Consumer | the dispatcher task |
| Full policy | **drop the newest** — count, log, return (FR-005) |
| Depth | tracked in an `AtomicUsize`, incremented on successful send, decremented when the dispatcher takes a job (FR-009) |

**Why depth is tracked separately**: `tokio::sync::mpsc` exposes no length. The
counter is therefore the only way to answer SC-007, and it must be decremented at
exactly one place — where the dispatcher receives — or it drifts and becomes
worse than no metric at all.

**The load-shedding contract**, which is Principle I restated in this feature's
terms:

```text
try_send(job):
  Ok                  -> depth += 1
  Err(Full(job))      -> discards += 1
                         warn!(guild, channel, speaker, "transcription queue full; dropped")
                         return                       // never await
  Err(Closed(job))    -> error!(...)                  // shutdown in progress
                         return
```

The `Full` arm returns rather than waiting. That is FR-004: not a fast path, but
no path at all through which the caller can block.

## Entity: ConcurrencyLimit

The resolved worker count, and where it came from.

| Field | Type | Description |
|---|---|---|
| `value` | `usize` | Effective worker count, 1..=32. |
| `source` | enum: `configured` \| `default_cpu` \| `default_gpu` | Reported at startup (FR-012) so an operator can tell "I set this" from "this was chosen for me". |

Defaults and validation live in
[contracts/configuration.md](./contracts/configuration.md). The default depends
on the resolved `ComputeBackend` from feature 001, which is why the pool must be
built after backend resolution and not before.

## Relationship to existing types

- `TranscriptionHandle` keeps its shape but `submit` **stops being `async`** and
  returns a `Result` synchronously. This is the single most important change in
  the feature: it makes "cannot block the receive path" checkable from the
  signature rather than by tracing every caller.
- `AppMetrics` (`src/telemetry/metrics.rs`) gains four measurements. Three are
  counters and fit the existing atomics; duration is a distribution and needs a
  bounded ring, in the shape of the existing `LineWindow`.
- `ComputeBackend` (from feature 001, `src/telemetry/backend.rs`) is an **input**
  here: it selects the concurrency default. Unchanged by this feature.
- `MetricsSnapshot` gains the corresponding fields — see
  [contracts/telemetry.md](./contracts/telemetry.md).
