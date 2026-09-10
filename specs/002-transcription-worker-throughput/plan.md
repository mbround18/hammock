# Implementation Plan: Transcription Worker Throughput and Load Shedding

**Branch**: `002-transcription-worker-throughput` | **Date**: 2026-09-09 | **Spec**: [spec.md](./spec.md)

**Input**: Feature specification from `/specs/002-transcription-worker-throughput/spec.md`

## Summary

Stop the transcription stage from serializing every speaker, and stop it from
stalling the audio receive path when it falls behind.

Three pieces of work, in dependency order:

1. **Never block the receive path** — make submission a synchronous `try_send`
   that drops and counts on a full queue, so a full queue can no longer halt
   audio reception for the whole channel. This is a correctness fix, not a
   performance one, and it is the reason US2 is P1.
2. **Worker pool** — one context, N pre-created decoder states, a semaphore
   bounding concurrency, and a dispatcher that never awaits the tick path. Each
   worker owns its state for its lifetime, which is where FR-002 falls out for
   free.
3. **Observability and sizing** — queue depth, in-flight count, transcription
   duration and discard counters on `/k8s/metrics`; a validated
   `TRANSCRIPTION_CONCURRENCY` with a per-backend default reported at startup.

**Two findings from Phase 0 change how this should be read**, and both are
carried into the sections below rather than left in research.md:

- **The state-reuse win is small.** Measured, per-utterance state setup is 0.9%
  of decode time on CPU and 7.3% on GPU (research R3). The idling the spec
  describes comes from serialization, not allocation. FR-001 and FR-004 carry
  this feature; FR-002 is a cheap side effect of doing them properly.
- **A second thing blocks the receive path**, which no FR names: a Discord HTTP
  call on the tick path (research R5). Constitution Principle I forbids it
  explicitly. Proposed as a scope addition below.

## Technical Context

**Language/Version**: Rust 2024 edition, toolchain 1.91

**Primary Dependencies**: `whisper-rs` 0.15.1 over the vendored `whisper-rs-sys`
0.14.1; `tokio` (`sync`, `rt-multi-thread`, `signal` — all already enabled);
`songbird` for the voice tick; `actix-web` for telemetry. **No new dependency is
required** — see research R7 for why the worker pool is built from
`tokio::sync::Semaphore` rather than an mpmc channel crate.

**Storage**: Filesystem only. Unchanged by this feature.

**Testing**: `cargo test --locked` (a mandatory CI gate), plus feature 001's
fixture harness (`tests/transcription_fixtures.rs`, `tests/support/mod.rs`) for
SC-006. This feature adds a **synthetic multi-speaker load generator** for
SC-001/SC-002 — see research R8 for why a generator rather than a recording.

**Target Platform**: Linux x86_64 containers, CPU and CUDA variants (feature
001). Not a hard prerequisite, but the GPU concurrency default cannot be
validated without it.

**Project Type**: Single Rust binary (Discord bot) shipped as a container image.

**Performance Goals**: 2x sustained throughput with multiple speakers (SC-002);
caption delay flat over a ten-minute four-speaker conversation (SC-001);
per-utterance time improved by the elimination of state setup, **bounded at
~1% (CPU) / ~7% (GPU)** by measurement (SC-003, research R3).

**Constraints**: The 20 ms voice tick must never await transcription, a queue, or
a network call (Constitution Principle I). Transcript output must not change
(SC-006). GPU concurrency is bounded by ~233 MB of decoder state per worker for
`base`, above a shared 147 MB model (research R4).

**Scale/Scope**: One new module (worker pool), one rewritten submission path,
four new metrics, one new environment variable, one new test harness. No change
to segmentation or decoding parameters — those are features 003 and 004.

## Constitution Check

*GATE: evaluated before Phase 0, re-evaluated after Phase 1 design.*

| Principle | Assessment |
|---|---|
| **I. Real-Time Voice Fidelity** | **This is the principle the feature exists to honor.** The receive path currently awaits a bounded queue (and a network call — R5) on the 20 ms tick. FR-004 and FR-005 replace waiting with explicit, counted, logged shedding, which is exactly what the principle prescribes: "drop the work, count the drop, and log it — rather than apply backpressure to the receive path." Re-checked post-design: submission becomes a synchronous `try_send`, so non-blocking is structural, not timing-dependent. |
| **II. Transcript Accuracy Is Measured, Not Asserted** | **Engaged, and partly obstructed.** SC-006 requires transcripts unchanged, and the mechanism exists (001's harness). But the corpus is non-speech only until 001's T005 lands, so "unchanged" can be demonstrated for silence and noise and not yet for speech. Recorded as a known limitation, not waved through. FR-003 makes this sharper than usual: state reuse is one parameter away from leaking text between speakers (R2), so a regression test pinning that is mandatory here rather than optional. |
| **III. Hardware-Optional Parity** | **Served.** Concurrency defaults are derived per backend (FR-011): available parallelism on CPU, a small fixed value on GPU. The CPU path is not merely supported but is the one where serialization hurts most — decode is ~20x slower there (R3), so concurrency matters more, not less. |
| **IV. Configuration Over Recompilation** | **Served.** `TRANSCRIPTION_CONCURRENCY` is an environment variable with a documented per-backend default, an entry in `.env.sample`, and startup validation that reports the offending value and the accepted range (FR-011, FR-012) rather than failing later at use. |
| **V. Observable by Default** | **Served, and this feature is mostly this.** FR-006 through FR-010 put discards, queue depth, in-flight count and duration on the metrics surface, each also logged with guild/channel/speaker. The principle's test — "an error path that neither increments a counter nor emits a log does not exist as far as an operator is concerned" — is what SC-005 and SC-007 encode. |

**Gate result: PASS.** No principle is violated and Complexity Tracking is empty.

One item needs an explicit decision rather than a deviation: the tick-path
network call (R5) is a **pre-existing** Principle I violation that this feature
does not create but would leave in place if scoped literally. See Scope below.

## Scope decision: the tick-path network call

`dispatch_chunk` awaits `resolve_user_name`, which does `ctx.http.get_user` — a
network round trip — on the voice tick path (`src/voice/mod.rs:287`,
`src/utils/discord.rs:8`). Principle I names network calls explicitly.

**Recommendation: fix it in this feature.** It is small — carry the `UserId` on
the job and resolve the display name in the worker, where awaiting is free — and
closing a feature titled "don't block the receive path" with the receive path
still blocking on Discord's API would be a strange result. It is called out here,
before tasks are written, so it is a decision rather than scope creep discovered
later.

**If rejected**, FR-004 is still satisfiable as literally written, and this
becomes a separate defect worth filing. The tasks phase should treat it as one
task, easily dropped.

## Project Structure

### Documentation (this feature)

```text
specs/002-transcription-worker-throughput/
├── spec.md              # Feature specification
├── plan.md              # This file
├── research.md          # Phase 0 output
├── data-model.md        # Phase 1 output
├── quickstart.md        # Phase 1 output
├── contracts/           # Phase 1 output
│   ├── telemetry.md     # /k8s/metrics additions
│   └── configuration.md # TRANSCRIPTION_CONCURRENCY contract
├── checklists/
│   └── requirements.md  # Spec quality checklist
└── tasks.md             # Created by /speckit-tasks, not this command
```

### Source Code (repository root)

```text
src/
├── config.rs            # + TRANSCRIPTION_CONCURRENCY parsing and validation
├── main.rs              # report effective concurrency; shutdown drain (FR-013)
├── transcription/       # transcription.rs grows into a module
│   ├── mod.rs           # TranscriptionHandle, spawn_worker_pool, decoding
│   └── pool.rs          # NEW — dispatcher, semaphore, state pool
├── voice/mod.rs         # submission no longer awaits; UserId carried on the job
└── telemetry/
    ├── metrics.rs       # + discards, queue depth, in-flight, duration
    └── server.rs        # + the new fields and their Swagger schema

tests/
├── support/mod.rs       # + multi-speaker load generator helpers
├── transcription_fixtures.rs   # (from 001, reused for SC-006)
└── worker_throughput.rs # NEW — concurrency, shedding, and latency-under-load
```

**Structure Decision**: `src/transcription.rs` becomes `src/transcription/`. It
is currently ~330 lines doing three jobs — submission, worker lifecycle, and
decoding — and this feature adds a pool to it. Splitting the pool into its own
file keeps the concurrency logic reviewable in isolation, which matters because
it is the part where a mistake costs audio. Everything else is modified in place.

## Phase 1 artifacts

- [data-model.md](./data-model.md) — the job, the worker, the pool, the queue,
  and the validation rules that keep FR-003 true.
- [contracts/telemetry.md](./contracts/telemetry.md) — the four metrics
  additions, extending feature 001's `/k8s/metrics` contract additively.
- [contracts/configuration.md](./contracts/configuration.md) —
  `TRANSCRIPTION_CONCURRENCY`: accepted range, per-backend default, and the
  startup validation message.
- [quickstart.md](./quickstart.md) — how to prove each criterion, and which two
  cannot be proven yet.

## Success criteria, restated honestly

The spec's criteria are kept as written. These notes attach measured context so
the tasks phase does not chase a number that is not there:

| Criterion | Note |
|---|---|
| SC-001, SC-002 | Measured with the synthetic multi-speaker load generator (research R8), not a recording. No suitable recording exists and none is coming until 001's T005 lands. |
| SC-003 | Ceiling is **~1% on CPU, ~7% on GPU**, measured (research R3). Should be recorded as "median short-utterance time reduced by the measured state-setup cost", not as a headline improvement. |
| SC-006 | Demonstrable today only for non-speech fixtures. Blocked on 001's T005 for speech. |
| SC-004, SC-005, SC-007 | Fully verifiable now, with no fixture dependency. |

## Complexity Tracking

No constitutional violations. This table is intentionally empty.

The one judgement call — growing `transcription.rs` into a module — is a
readability choice, not a deviation, and adds no crate, service, or dependency.

## Post-Design Constitution Re-Check

Re-evaluated after the Phase 1 artifacts were written:

- **Principle I** — strengthened by the design, not merely preserved.
  `submit` returns `Result` synchronously with no `async`, so "does not block the
  receive path" becomes a property of the type signature rather than something to
  verify by inspection.
- **Principle II** — unchanged and still partly blocked. The FR-003 regression
  test in [data-model.md](./data-model.md) is the part that is fully actionable
  now.
- **Principle IV** — the configuration contract validates at startup and names
  both the bad value and the accepted range, per the principle's wording.
- **Principle V** — [contracts/telemetry.md](./contracts/telemetry.md) makes
  "is the system keeping up" (SC-007) answerable from metrics alone: queue depth
  and in-flight against the concurrency limit, with a discard counter that is
  non-zero exactly when it is not keeping up.
- **No new violations introduced.**
