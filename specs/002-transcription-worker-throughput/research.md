# Phase 0 Research: Transcription Worker Throughput and Load Shedding

**Feature**: [spec.md](./spec.md) | **Date**: 2026-09-09

Every finding below was verified against this repository, the vendored
`whisper.cpp`, or measured on this machine — not assumed. Two findings contradict
assumptions in the spec; both are called out rather than smoothed over.

## R1: Is it safe to run several decodes concurrently from one model?

**Decision**: Yes. One `WhisperContext`, N `WhisperState`s, one job per state at a
time. No new dependency, no model duplication.

**Evidence**:

- `WhisperState` is `Send + Sync` (`whisper-rs-0.15.1/src/whisper_state.rs:19-21`)
  and `WhisperInnerContext` is `Send + Sync`
  (`whisper_ctx.rs:434-435`), so an `Arc<WhisperContext>` can back several states
  on different threads.
- whisper-rs's `WhisperState::full` calls **`whisper_full_with_state`**
  (`whisper_state.rs:296-303`), not `whisper_full`. That distinction is the whole
  question: `whisper_full` mutates `ctx->state` (the context's own default state,
  e.g. `ctx->state->result_all.clear()` at `whisper.cpp:7736`), whereas
  `whisper_full_with_state` writes only through the `state` argument. Everything
  it touches on `ctx` — `ctx->model`, `ctx->itype`, `hparams` — is read-only
  during inference.
- **Upstream does exactly this.** `whisper_full_parallel` creates one state per
  thread and runs `whisper_full_with_state` on `std::thread`s sharing a single
  `ctx` (`whisper.cpp`, `whisper_full_parallel`, lines ~7751-7810). The design
  proposed here is the one whisper.cpp itself ships.

**Consequence**: the model weights (147 MB for `base`) are loaded once and shared.
Only per-state buffers multiply — see R3.

**Alternatives considered**: N independent `WhisperContext`s, one per worker.
Rejected — it multiplies model memory by N for no benefit, which on GPU is the
scarce resource. Also considered `whisper_full_parallel` directly; rejected
because it splits *one* audio buffer across threads, which is not what we need —
we need *different* utterances decoded at once.

## R2: Does reusing decoder state leak one utterance into another?

**Decision**: No, provided `no_context` is left at its default. FR-003 is
satisfied by existing behavior, and must be pinned by a regression test.

**Evidence**: the two buffers that accumulate across calls both live on the state
(`whisper.cpp:915-923`) and both are cleared at the top of every decode:

| Buffer | Cleared where | Condition |
|---|---|---|
| `state->result_all` (segments) | `whisper.cpp:6813` | unconditionally, every call |
| `state->prompt_past` (text context) | `whisper.cpp:6911-6912` | `if (params.no_context)` |

`whisper_full_default_params` sets `no_context = true` (`whisper.cpp:5933`), and
whisper-rs's `FullParams::new` is a thin wrapper over
`whisper_full_default_params` (`whisper_params.rs:54-64`). `src/transcription.rs`
never calls `set_no_context`, so the default holds.

**Consequence worth stating plainly**: FR-003 is one `set_no_context(false)` away
from being violated, silently, with the symptom being one speaker's words
appearing in another speaker's caption. It is a property to *protect*, not to
build. A test that asserts a state produces identical output for the same audio
regardless of what it decoded previously is the cheapest way to keep it true.

**Alternatives considered**: dropping and recreating state between utterances to
guarantee isolation — which is what the code does today. Rejected: R3 shows it
buys almost nothing, and R2 shows there is nothing to isolate.

## R3: How much does per-utterance state setup actually cost?

**Decision**: Far less than the spec assumes. FR-002 is worth doing, but it is
not where the win is — and SC-003 needs to be read with that in mind.

**Measured** on this host (RTX 3090 Ti, 32 cores, `ggml-base.bin`, release build,
1 s utterance — deliberately short, because that is where setup cost matters
most; median of 5):

| Backend | `create_state` | decode | setup as share of total |
|---|---|---|---|
| CPU | 7.6 ms | 835 ms | **0.9%** |
| GPU | 3.0 ms | 38.6 ms | **7.3%** |

Decode time itself was unchanged between a fresh state and a reused one
(GPU: 38.6 ms vs 36.5 ms — inside run-to-run noise), so the saving from FR-002 is
exactly the `create_state` call and nothing more.

**Consequence**: eliminating repeated state setup can improve per-utterance time
by at most ~1% on CPU and ~7% on GPU, and less than that in practice because the
bot's default chunk is 3 seconds rather than 1, which dilutes fixed cost further.

**This contradicts the spec's framing.** The Input describes the accelerator
"idling between chunks" and SC-003 asks for a reduction "by the elimination of
repeated decoder-state setup" as though it were a principal source of latency. It
is not. The idle time is caused by **serialization** — one chunk at a time — not
by allocation. FR-001 (concurrency) and FR-004/FR-005 (never block the receive
path) carry essentially all of this feature's value.

FR-002 is still worth doing: it is nearly free once a worker pool exists, since
each worker naturally owns a state for its lifetime. But SC-003's target should
be stated as the small bounded number it is, not left implying a large win. See
the note under Success Criteria in [plan.md](./plan.md).

## R4: How much memory does each concurrent decoder cost?

**Decision**: ~233 MB per state for `base` on CUDA, on top of a shared 147 MB
model. This is what bounds GPU concurrency and what the "configured higher than
the accelerator can hold" edge case is really about.

**Measured** from `whisper_init_state` output on CUDA, `ggml-base.bin`:

| Buffer | Size |
|---|---|
| kv self | 6.29 MB |
| kv cross | 18.87 MB |
| kv pad | 3.15 MB |
| compute (conv) | 17.24 MB |
| compute (encode) | 85.88 MB |
| compute (cross) | 4.66 MB |
| compute (decode) | 97.29 MB |
| **Total per state** | **233.38 MB** |

Model weights, shared across all states: 147.37 MB.

So GPU footprint is `147 MB + N x 233 MB` for `base`. These buffers scale with
model dimensions, so a `large` model costs several times this per state — which
is why the GPU default must be small and why an operator raising it on a big
model is the case that runs out of memory.

**Consequence**: concurrency must be validated against something, and the honest
check is "did state N allocate". Creating all N states eagerly at startup turns a
mid-conversation out-of-memory failure into a startup failure with a clear
message, which is what the spec's edge case asks for.

## R5: What actually blocks the audio receive path today?

**Decision**: Two things, not one. The spec names the first; the second is worse
and is not in any FR.

**Evidence**:

1. **The bounded queue** (the spec's concern). `TranscriptionHandle::submit`
   awaits `tx.send(job).await` on an `mpsc::channel(32)`
   (`src/transcription.rs:41-46, 57`), and `dispatch_chunk` awaits that
   (`src/voice/mod.rs:308`) from inside `on_voice_tick`
   (`src/voice/mod.rs:222-234`), which songbird awaits per 20 ms tick. A full
   queue therefore stalls audio reception for the entire channel. Exactly as
   described.

2. **A Discord HTTP call.** `dispatch_chunk` also awaits
   `resolve_user_name(&self.ctx, user_id)` (`src/voice/mod.rs:287`), which on a
   cache miss performs `ctx.http.get_user(user_id).await` — a **network round
   trip** — on the tick path (`src/utils/discord.rs:8`).

**Constitution Principle I names both**: a handler on the receive path "MUST
complete without awaiting transcription, disk I/O, **network calls**, or an
unbounded queue." Item 2 is a live violation today, independent of queue depth,
and it will still be one after FR-004 is satisfied as literally written.

**Recommendation**: fix both. Item 2 is small — resolve the name off the tick
path, or attach the `UserId` to the job and resolve it in the worker, where an
await costs nothing. Leaving it would mean closing a feature about not blocking
the receive path with the receive path still blocking. Flagged in
[plan.md](./plan.md) as a deliberate scope addition rather than smuggled in.

**Alternatives considered**: an unbounded queue, which would make `send`
non-blocking without a shedding policy. Rejected outright — Principle I requires
shedding load explicitly over unbounded growth, and the spec's own edge case
forbids "the queue must not grow without limit as an alternative to dropping."

## R6: Which shedding policy, and how is it implemented?

**Decision**: Drop the newest on a full queue — `try_send`, and on
`TrySendError::Full` count it, log it with guild/channel/speaker, and return.
This confirms the spec's assumed default.

**Rationale**: the spec assumed drop-newest on the grounds that older queued
utterances are closer to being useful. Two further points support it:

- `try_send` needs no new machinery. Dropping the *oldest* would require popping
  from the receiving end, which `tokio::sync::mpsc` does not offer to the sender;
  it would mean replacing the channel with a deque plus a notifier.
- Drop-newest keeps captions contiguous up to the point of overload. Drop-oldest
  produces a transcript with a hole in the middle and recent text present, which
  reads as a bug rather than as overload.

**Consequence**: `submit` stops being `async`. That is the change that actually
satisfies FR-004: a synchronous `try_send` cannot block a caller, under any queue
condition, by construction rather than by timing.

**Alternatives considered**: `send_timeout`. Rejected — a bounded wait is still a
wait on the 20 ms tick path, and it converts a clean drop into a jittery one.

## R7: Worker pool shape, with no new dependencies

**Decision**: one dispatcher task owning the `mpsc::Receiver`, a
`tokio::sync::Semaphore` with N permits bounding concurrency, and a pool of N
pre-created `WhisperState`s. Each job acquires a permit, takes a state, runs on
`spawn_blocking`, returns the state.

**Rationale**: `tokio::sync::mpsc` is single-consumer, so N workers cannot each
`recv()` from it. The alternatives were an mpmc channel (`async-channel`, a new
dependency) or sharing the receiver behind a mutex (workable but clumsy, and it
serializes the `recv` await). Dispatcher-plus-semaphore uses only what is already
in the tree, keeps the permit count and the state count trivially equal, and puts
the concurrency bound in one obvious place.

**Consequence for FR-008**: because each job is its own `spawn_blocking` task, a
panic or error in one is caught at its join point and cannot take down the
dispatcher or the pool. The state must be returned to the pool on the failure
path too, or the pool shrinks by one on every error — which is precisely the
"pool must not shrink permanently" edge case.

**Alternatives considered**: `rayon`, or N long-lived OS threads each owning a
state and pulling from a crossbeam queue. Rejected — both add a dependency and a
second scheduling domain alongside tokio, for a workload that is already
`spawn_blocking`-shaped.

## R8: Measuring this feature — what exists and what is missing

**Decision**: reuse feature 001's fixture harness. It supplies the Opus round
trip, WER scoring and reporting. But **the fixtures this feature's criteria need
do not exist yet.**

**Evidence**: `tests/fixtures/audio/` currently holds four non-speech fixtures
(silence, fan noise, keyboard noise, music). Feature 001's T005 — real
conversational speech with reference transcripts — is **blocked and outstanding**
(`specs/001-gpu-accelerated-builds/VERIFICATION.md`), because reference
transcripts must be produced by listening.

**What that costs this feature**:

| Criterion | Needs | Status |
|---|---|---|
| SC-001 (four speakers, 10 min, no growing lag) | a multi-speaker fixture, longer than anything in the corpus | **not satisfiable today** |
| SC-002 (2x sustained throughput) | a multi-speaker fixture | **not satisfiable today** |
| SC-006 (transcripts unchanged) | any fixture; speech makes it meaningful | partly — non-speech only |
| SC-003 (per-utterance time) | any fixture | satisfiable now |
| SC-004, SC-005, SC-007 | induced overload, no fixtures needed | satisfiable now |

**Recommendation**: SC-001 and SC-002 need a **synthetic multi-speaker load
generator**, not a recording — four concurrent submitters replaying fixture audio
through the real queue, measuring end-of-utterance to caption-written latency.
That is reproducible, needs no licensed recordings, and measures the thing the
criteria actually describe (queueing behavior under concurrent speakers), which a
single recording of four people would measure less directly anyway.

Real speech fixtures remain necessary for SC-006 to mean anything, and that stays
blocked on 001's T005.

**Alternatives considered**: deferring this feature until T005 lands. Rejected —
the load generator is the better instrument for SC-001/SC-002 regardless, and
FR-004's correctness bug (the receive path stalling and losing audio for everyone)
should not wait on a fixture-sourcing errand.

## Resolved unknowns

Every NEEDS CLARIFICATION from Technical Context is resolved above. Two items are
carried forward as **stated risks rather than unknowns**:

1. SC-003's achievable magnitude is ~1% (CPU) / ~7% (GPU), not the larger win the
   spec's framing implies (R3).
2. SC-006 cannot be fully honored until 001's T005 lands (R8).
