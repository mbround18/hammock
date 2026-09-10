# Tasks: Transcription Worker Throughput and Load Shedding

**Input**: Design documents from `/specs/002-transcription-worker-throughput/`

**Prerequisites**: [plan.md](./plan.md), [spec.md](./spec.md), [research.md](./research.md), [data-model.md](./data-model.md), [contracts/telemetry.md](./contracts/telemetry.md), [contracts/configuration.md](./contracts/configuration.md), [quickstart.md](./quickstart.md)

**Status**: Implemented. 41 of 42 tasks complete; T027 is partial (SC-002 measured, SC-001 not). See [VERIFICATION.md](./VERIFICATION.md) for what was verified and what was not, including two layout deviations forced by `hammock` being a binary crate with no library target.

**Tests**: TDD was not requested, so there are no failing-test-first tasks. Three test artifacts ARE deliverables and appear as implementation tasks: the multi-speaker load generator (the only instrument that can measure SC-001/SC-002), the FR-003 leak test (without which state reuse can silently put one speaker's words in another's caption), and the overload harness (which is how the correctness fix is proven at all).

**Organization**: Grouped by user story. US1 and US2 are both P1.

**US2 is sequenced before US1, deliberately.** US2 is a correctness fix — today a full queue stalls audio reception for the entire channel, losing audio for speakers whose captions were fine. US1 is a throughput improvement. Shipping throughput first would mean the window where the receive path can stall stays open longer, and US1's gains are hard to observe honestly while overload still corrupts the measurement. US2 also stands alone: it is valuable with concurrency still at 1.

## Format: `[ID] [P?] [Story] Description`

- **[P]**: Can run in parallel (different files, no dependencies)
- **[Story]**: US1, US2, US3 — maps to the spec's user stories

## Path Conventions

Single Rust project. Sources at `src/`, tests at `tests/`. `src/transcription.rs` becomes `src/transcription/` in T001; every later path assumes that.

---

## Phase 1: Setup

**Purpose**: Make room for the pool before writing it, and pin the numbers this feature will be judged against.

- [X] T001 Split `src/transcription.rs` into `src/transcription/mod.rs` as a pure move with no behavior change, verified by `cargo test --locked` passing before and after — the pool lands in a sibling file in T019 and reviewing it against a simultaneous reshuffle would hide mistakes in the part where a bug costs audio
- [X] T002 [P] Record the measured state-setup baseline for SC-003 in `specs/002-transcription-worker-throughput/baseline.md`: CPU `create_state` 7.6 ms vs 835 ms decode (0.9%), GPU 3.0 ms vs 38.6 ms (7.3%), per research.md R3 — SC-003's ceiling is these numbers, and recording them stops the tasks below chasing a larger win that does not exist

---

## Phase 2: Foundational (Blocking Prerequisites)

**Purpose**: The metrics surface both P1 stories report through, and the load generator both are measured with.

**⚠️ CRITICAL**: US1 and US2 both write to these files. Landing them once here avoids two stories editing `metrics.rs` in parallel.

### Metrics surface

- [X] T003 Add `total_utterances_discarded`, `transcription_queue_depth`, `transcription_in_flight` and `transcription_concurrency_limit` to `AppMetrics` and `MetricsSnapshot` in `src/telemetry/metrics.rs` — the two gauges as `AtomicUsize`, the limit as a set-once value beside the existing `compute_backend`
- [X] T004 Add the `transcription_duration_ms` distribution to `src/telemetry/metrics.rs` per [contracts/telemetry.md](./contracts/telemetry.md): lifetime `count`/`total_ms`, plus `p50_ms`/`p95_ms`/`max_ms` over a bounded recent window built like the existing `LineWindow` — a p95 polluted by a slow decode an hour ago answers nothing about now
- [X] T005 Expose all five new fields inside the nested `metrics` object on `/k8s/metrics` in `src/telemetry/server.rs`, leaving `compute_backend` where feature 001 put it as a top-level sibling
- [X] T006 Replace the loose `"type": "object"` description of `metrics` in the Swagger document in `src/telemetry/server.rs` with real properties for every field it now returns — the contract names this a documentation obligation, not an optional extra

### Job shape and measurement harness

- [X] T007 [P] Add `queued_at: Instant` to `TranscriptionJob` in `src/transcription/mod.rs`, set at successful enqueue, so queue wait is separable from decode time in T004's numbers
- [X] T008 [P] Implement the multi-speaker load generator in `tests/support/mod.rs`: N simulated speakers submitting existing fixture audio concurrently through the real queue, recording per-utterance end-of-audio to caption-written latency — research.md R8 explains why this rather than a recording, and it is the only instrument that can measure SC-001 or SC-002
- [X] T009 Create `tests/worker_throughput.rs` with the same skip-without-a-model behavior as `tests/transcription_fixtures.rs`, so `cargo test` stays green on a CI runner with no model — a hard failure would make a mandatory gate unpassable rather than meaningful

**Checkpoint**: Metrics can carry the new numbers and the harness can generate load. Both P1 stories can now begin.

---

## Phase 3: User Story 2 — Overload sheds work instead of losing audio (Priority: P1) 🎯 MVP

**Goal**: A full queue can never stall the audio receive path. Excess work is dropped explicitly, counted, and logged.

**Independent Test**: Constrain throughput, drive sustained speech, and confirm submission never blocks, discards are counted and logged with guild/channel/speaker, and the queue drains without a restart.

- [X] T010 [US2] Change `TranscriptionHandle::submit` in `src/transcription/mod.rs` from `async fn` using `tx.send().await` to a synchronous `fn` using `try_send` — this is the change that satisfies FR-004, because a synchronous call cannot block a caller under any queue condition by construction rather than by timing
- [X] T011 [US2] Handle `TrySendError::Full` in `src/transcription/mod.rs` by dropping the newest job, incrementing `total_utterances_discarded`, and returning without awaiting (FR-005, FR-006) — confirming the spec's assumed drop-newest policy for the reasons in research.md R6
- [X] T012 [US2] Emit a warning on every discard in `src/transcription/mod.rs` carrying `guild`, `channel` and `speaker` (FR-007), incremented at the same place as the counter — SC-005 requires 100% of discards in both, so a discard that counted without logging would pass the metric and fail the criterion
- [X] T013 [US2] Handle `TrySendError::Closed` in `src/transcription/mod.rs` distinctly from `Full`, logging at error level — a closed channel means shutdown, not overload, and folding it into the discard counter would make that counter mean two different things
- [X] T014 [US2] Track queue depth in `src/transcription/mod.rs`: increment on successful `try_send`, decrement at the single point where the dispatcher receives (FR-009) — a counter decremented in more than one place drifts and becomes worse than no metric
- [X] T015 [US2] Update the call site in `src/voice/mod.rs` so `dispatch_chunk` no longer awaits submission, and remove the now-dead error branch that assumed a fallible async send
- [X] T016 [US2] Move the Discord name lookup off the voice tick path in `src/voice/mod.rs` and `src/transcription/mod.rs` by carrying `SpeakerLabel::Deferred(UserId)` on the job and resolving it in the worker, per [data-model.md](./data-model.md) — **this is the scope addition flagged in [plan.md](./plan.md)**: `resolve_user_name` awaits `ctx.http.get_user`, a network round trip, on the 20 ms tick, which Constitution Principle I names explicitly. It is a pre-existing violation this feature does not create. **Drop this one task if the addition is rejected**; nothing else in this phase depends on it
- [X] T017 [US2] Implement quickstart Scenario 3 in `tests/worker_throughput.rs`: constrain throughput, oversubscribe it, and assert submission returns in microseconds regardless of queue state, that discard count equals discard log lines exactly, that non-discarded work still produces captions, and that the queue drains to `in_flight` 0 without a restart (FR-004, FR-005, SC-004, SC-005)

**Checkpoint**: The receive path can no longer be stalled by transcription. Shippable on its own, with concurrency still at 1.

---

## Phase 4: User Story 1 — A busy channel keeps up (Priority: P1)

**Goal**: Several speakers are transcribed concurrently instead of serially, on reused decoder state.

**Independent Test**: Run the load generator with four simulated speakers and confirm caption delay at the end of a ten-minute run is no worse than at the start, and that throughput is at least 2x the same run at concurrency 1.

- [X] T018 [US1] Resolve the concurrency limit in `src/transcription/mod.rs` from the `ComputeBackend` feature 001 already resolves — CPU `clamp(available_parallelism / 4, 1, 4)`, GPU `2` — per [contracts/configuration.md](./contracts/configuration.md). The division by four is not arbitrary: whisper.cpp already runs `n_threads = min(4, hardware_concurrency)` inside a single decode, so one worker per core oversubscribes roughly fourfold and runs slower than doing less
- [X] T019 [US1] Create `src/transcription/pool.rs` holding the dispatcher, a `tokio::sync::Semaphore` with `concurrency` permits, and a `Mutex<Vec<WhisperState>>` state pool — research.md R7 explains why this shape rather than an mpmc channel crate; it needs no new dependency
- [X] T020 [US1] Create all `concurrency` `WhisperState`s eagerly at pool construction in `src/transcription/pool.rs` and fail startup with a message naming memory as the constraint if allocation fails — each state costs ~233 MB for `base` on CUDA above a shared 147 MB model (research.md R4), and eager creation is what turns a mid-conversation out-of-memory into a startup error, as the spec's edge case requires
- [X] T021 [US1] Keep the permit count and the state count equal in `src/transcription/pool.rs` — they are two encodings of one bound, and if they can diverge a job holds a permit then waits on an empty pool, which is a second invisible queue
- [X] T022 [US1] Run each job on `spawn_blocking` from the dispatcher in `src/transcription/pool.rs`, replacing the current single-job-at-a-time loop that awaits each `spawn_blocking` before receiving the next (FR-001)
- [X] T023 [US1] Return the decoder state to the pool from a `Drop` guard in `src/transcription/pool.rs` so it is returned on error and panic paths too (FR-008) — returning only on success shrinks the pool by one per failure until nothing decodes, which is exactly the "pool must not shrink permanently" edge case and is invisible until it is total
- [X] T024 [US1] Reuse each worker's `WhisperState` across jobs rather than calling `ctx.create_state()` per utterance in `src/transcription/pool.rs` (FR-002) — worth doing because it is nearly free once the pool exists, but see T002: the measured ceiling is 0.9% on CPU and 7.3% on GPU, so it is not where this feature's value comes from
- [X] T025 [US1] Record in-flight count and decode duration around each job in `src/transcription/pool.rs`, feeding T003's gauge and T004's distribution (FR-009, FR-010)
- [X] T026 [US1] Add the FR-003 leak test to `tests/worker_throughput.rs`: decode fixture A then fixture B on one reused state and assert B's transcript is byte-identical to B decoded on a fresh state — `no_context` defaults to `true` so this passes today (research.md R2), which is the point: this test is the entire enforcement of FR-003, and one `set_no_context(false)` would otherwise leak one speaker's words into another's caption silently
- [~] T027 [US1] Implement quickstart Scenario 2 in `tests/worker_throughput.rs`: four simulated speakers for a sustained run, asserting mean caption delay in the final minute is no worse than the first (SC-001) and throughput is ≥2x the concurrency-1 run (SC-002), expressed as a **ratio** and never as an absolute threshold — absolute numbers are machine-dependent and would make CI fail on slow runners for no reason
  - **PARTIAL.** SC-002 is done and passing as a ratio: `concurrent_workers_raise_throughput` measures 2.79x on CPU and 2.27x on GPU. **SC-001 is not measured** — caption delay over a ten-minute four-speaker conversation needs a sustained run against a live bot, which the in-process generator does not reproduce. The mechanism that made delay grow is gone and boundedness is proven, but the criterion is not signed off. Recorded in [VERIFICATION.md](./VERIFICATION.md).
- [X] T028 [US1] Assert in `tests/worker_throughput.rs` that single-speaker latency at concurrency 1 is unchanged from before this feature (US1 acceptance scenario 3) — concurrency must not make the uncontended case slower
- [X] T029 [US1] Implement quickstart Scenario 4 in `tests/worker_throughput.rs`: inject a decode failure and assert the error counter increments, later utterances still transcribe, and `in_flight` still reaches the limit afterwards (FR-008) — the last assertion is the one that catches a leaked state

**Checkpoint**: Both P1 stories complete. Multiple speakers are served concurrently and overload sheds rather than stalls.

---

## Phase 5: User Story 3 — Operator can size the worker pool (Priority: P3)

**Goal**: The concurrency limit is configurable, validated, and reported.

**Independent Test**: Set the value to a known number, start the bot, and confirm startup output and `/k8s/metrics` both report it and that in-flight work never exceeds it.

- [X] T030 [US3] Parse `TRANSCRIPTION_CONCURRENCY` in `src/config.rs`, overriding T018's per-backend default when set
- [X] T031 [US3] Validate it at startup in `src/config.rs` per [contracts/configuration.md](./contracts/configuration.md): reject `0`, non-integers, negatives and values above 32, each error naming the offending value **and** the accepted range (FR-011). Unlike feature 001's GPU settings this refuses to start rather than degrading — an absent GPU is an environment problem the operator may not control, a nonsense integer is one they can fix in seconds, and FR-011 rules out silently substituting a value
- [X] T032 [US3] Report the effective limit and its source at startup in `src/main.rs` — `configured` / `default_cpu` / `default_gpu` (FR-012) — because "4 workers" alone does not tell an operator whether their configuration was read
- [X] T033 [US3] Publish the same value as `transcription_concurrency_limit` on `/k8s/metrics` via `src/telemetry/metrics.rs`, since startup output scrolls away — the reasoning feature 001 applied to the compute backend
- [X] T034 [P] [US3] Document `TRANSCRIPTION_CONCURRENCY` in `.env.sample`: effect, per-backend defaults, accepted range, and which metrics show whether the current value is right
- [X] T035 [P] [US3] Document sizing the worker pool in `docs/README.md` — the metrics that say whether the system is keeping up, and when to raise or lower the limit
- [X] T036 [US3] Implement quickstart Scenario 1 in `tests/worker_throughput.rs`: assert each invalid value is refused with its range named, that a valid value is honored, and that `in_flight` never exceeds the limit

**Checkpoint**: All three stories independently functional.

---

## Phase 6: Polish & Cross-Cutting Concerns

- [X] T037 Add bounded shutdown draining in `src/main.rs` and `src/transcription/pool.rs` (FR-013): on SIGTERM/Ctrl-C allow in-flight decodes a bounded period to finish, then discard what remains and report the count. No signal handling exists today — `client.start().await` is the last statement in `main` — so this is new plumbing, and `compose.yml` already declares `stop_grace_period: 30s`, which is the budget to respect
- [X] T038 [P] Cap queue growth under sustained overload and confirm discards stay bounded and counted in `tests/worker_throughput.rs` (spec Edge Cases) — the queue must never grow without limit as an alternative to dropping
- [X] T039 [P] Confirm in `tests/worker_throughput.rs` that one very long utterance does not prevent shorter utterances from other speakers being transcribed concurrently (spec Edge Cases)
- [X] T040 Run `cargo test --release --test transcription_fixtures` before and after this feature and diff the two `target/fixture-report-*.json` reports, confirming transcript text is unchanged (SC-006, quickstart Scenario 6), recording the comparison in `specs/002-transcription-worker-throughput/VERIFICATION.md` per Constitution Principle II — **note the limitation**: the corpus is non-speech only until feature 001's T005 lands, so this shows the pool did not change how silence and noise are transcribed and cannot yet show speech is unaffected
- [X] T041 Run quickstart Scenario 5 on a GPU host via `cargo test --release --features cuda --test worker_throughput` against `tests/worker_throughput.rs` and confirm the default of 2, that raising it increases throughput, and that an over-large value fails at startup naming memory (FR-011 on the GPU path)
- [X] T042 Write `specs/002-transcription-worker-throughput/VERIFICATION.md` recording the measured throughput ratio, the hardware, the concurrency used, and which criteria remain outstanding, following the shape of `specs/001-gpu-accelerated-builds/VERIFICATION.md` — SC-001 and SC-002 are measured with a synthetic generator rather than a recording, and SC-006 is partial; a green CI run proves shedding and pool survival, not the throughput target

---

## Dependencies & Execution Order

### Phase Dependencies

- **Setup (Phase 1)**: no dependencies
- **Foundational (Phase 2)**: depends on Setup — **blocks both P1 stories**
- **US2 (Phase 3)**: depends on Phase 2. Independent of US1
- **US1 (Phase 4)**: depends on Phase 2. Independent of US2, but see the note below
- **US3 (Phase 5)**: depends on US1 — there is no pool to size until T019 exists
- **Polish (Phase 6)**: depends on US1 and US2

### US1 and US2 are independent but should not be parallelised

Both edit `src/transcription/mod.rs`, and US1's dispatcher (T022) is the same code that decrements US2's queue-depth counter (T014). Running them concurrently means resolving that overlap in a merge rather than in a design. Sequential, US2 first, for the reasons in the Organization note.

### Within Phase 2

- T003 → T004 → T005 → T006 are strictly sequential — T003 and T004 both edit `metrics.rs`, T005 and T006 both edit `server.rs`, and the endpoint cannot expose what the snapshot does not carry
- T007, T008 are parallel with the metrics chain — different files
- T009 depends on T008

### Within US2

- T010 → T011 → T012 → T013 → T014 are sequential (all edit `src/transcription/mod.rs`)
- T015 depends on T010 — the call site cannot stop awaiting until `submit` stops being async
- T016 is independent of the chain and droppable
- T017 depends on everything above it

### Within US1

- T018 → T019 → T020 → T021 → T022 → T023 → T024 → T025 are sequential (all in `pool.rs`, each building on the last)
- T026, T027, T028, T029 depend on T025 and are parallel with each other only if written as separate test functions

### Within US3

- T030 → T031 → T032 → T033 sequential
- T034, T035 are parallel — two different files, no shared state
- T036 depends on T031

### Parallel Opportunities

- T002 in Setup
- T007, T008 against the T003-T006 metrics chain in Foundational
- T034, T035 within US3
- T038, T039 in Polish

---

## Parallel Example: Phase 2

```bash
# Track A — metrics surface (strictly sequential within itself)
Task: "Add discard/depth/in-flight/limit fields to src/telemetry/metrics.rs"

# Track B — job shape and harness (no metrics dependency)
Task: "Add queued_at to TranscriptionJob in src/transcription/mod.rs"
Task: "Implement the multi-speaker load generator in tests/support/mod.rs"
```

---

## Implementation Strategy

### MVP scope

**Phase 1 + Phase 2 + Phase 3 (US2).** That delivers the correctness fix on its
own: the audio receive path can no longer be stalled by a full transcription
queue, and every dropped utterance is counted and attributable.

It is worth shipping there if the pool work slips. With concurrency still at 1
the bot will drop more than it eventually should — but dropping one speaker's
utterance explicitly is strictly better than corrupting every speaker's audio
silently, which is what happens today.

### Then US1

The throughput work is where the spec's headline benefit lives, and it is the
larger change. It is second only because correctness precedes performance, not
because it is optional — both are P1.

### Sequencing note

The measured ceiling on FR-002 (T002, T024) is ~1% on CPU and ~7% on GPU. If
implementation pressure forces a cut, cut **state reuse**, not concurrency:
reuse is the part that looks central in the spec's framing and is nearly
irrelevant in the measurements. T019-T023 are what make a busy channel keep up.

### Hardware dependency

T041 needs an NVIDIA GPU host. T027's throughput ratio is machine-dependent and
should be measured on the deployment target where possible, and never asserted
as an absolute number in CI.

### Dependency on feature 001

Not a hard prerequisite. T018's GPU default and T041 cannot be validated without
001's accelerated build, and T040's SC-006 check stays partial until 001's T005
lands — but every task in Phases 1-3 is independent of it, and those are the ones
that fix the audio loss.
