# Quickstart: Validating Throughput and Load Shedding

**Feature**: [spec.md](./spec.md) | **Plan**: [plan.md](./plan.md)

How to prove this feature works. Each scenario maps to success criteria.
Scenarios 1-4 run anywhere; Scenario 5 needs a GPU.

**Read this first**: two criteria cannot be fully proven yet. SC-001 and SC-002
need multi-speaker audio, and SC-006 needs speech audio. The corpus is non-speech
only until feature 001's T005 lands. Scenarios 2 and 6 say exactly what that
does and does not leave provable — see research R8.

## Prerequisites

| Scenario | Requires |
|---|---|
| 1, 2, 3, 4, 6 | A Whisper model on disk (`models/ggml-base.bin`) |
| 5 | The accelerated build from feature 001, plus a GPU |

The fixture harness skips loudly without a model. That skip is not a pass.

---

## Scenario 1 — Concurrency is real and bounded

**Covers**: FR-001, FR-011, FR-012

```sh
TRANSCRIPTION_CONCURRENCY=3 cargo run --release
```

**Expected**:

- Startup reports `transcription concurrency: 3 workers (from TRANSCRIPTION_CONCURRENCY)`.
- `curl -s localhost:8080/k8s/metrics | jq '.metrics.transcription_concurrency_limit'` is `3`.
- Under load, `.metrics.transcription_in_flight` reaches 3 and **never exceeds it**.

Then the validation table from
[contracts/configuration.md](./contracts/configuration.md):

```sh
TRANSCRIPTION_CONCURRENCY=0    cargo run --release   # refuses, names the range
TRANSCRIPTION_CONCURRENCY=abc  cargo run --release   # refuses, names the range
TRANSCRIPTION_CONCURRENCY=64   cargo run --release   # refuses, names the maximum
cargo run --release                                   # backend default, source reported
```

Each refusal must name the offending value **and** the accepted range, and must
happen before any decoder state is allocated.

---

## Scenario 2 — Four speakers do not fall behind

**Covers**: SC-001, SC-002 — **the load generator, not a recording**

```sh
cargo test --release --test worker_throughput -- --nocapture concurrent_speakers
```

The generator submits utterances from four simulated speakers concurrently
through the real queue and worker pool, and records, per utterance, the delay
from end-of-audio to caption written.

**Expected**:

- Mean caption delay in the final minute is **no worse than** in the first
  (SC-001).
- Utterances per second is **at least 2x** the same generator run against a
  concurrency of 1, which stands in for the current implementation (SC-002).
- Single-speaker delay at concurrency 1 is unchanged from today (US1 scenario 3 —
  concurrency must not make the uncontended case slower).

**Why a generator and not a recording.** No multi-speaker fixture exists, and
none can until 001's T005 lands. But the generator is the better instrument
regardless: these criteria are about queueing behavior under concurrent
speakers, and a generator controls speaker count, overlap and duration exactly,
where a recording bakes them in. The audio itself comes from the existing
fixtures, so this needs no new licensed material.

---

## Scenario 3 — Overload sheds work and never blocks the receive path

**Covers**: FR-004, FR-005, FR-006, FR-007, SC-004, SC-005 — **the correctness fix**

```sh
cargo test --release --test worker_throughput -- --nocapture overload_sheds
```

Constrain throughput (concurrency 1, a queue capacity of 2) and drive far more
work than can be served.

**Expected**:

- **Submission never blocks.** Every `submit` returns within microseconds
  regardless of queue state. This is the assertion the whole feature exists for:
  a blocked submit is lost audio for every speaker in the channel, not just the
  one being dropped.
- `.metrics.total_utterances_discarded` rises.
- Every discard has a matching warning log carrying guild, channel and speaker.
  Counter and log lines must agree **exactly** — SC-005 says 100% in both.
- When submission stops, the queue drains and `in_flight` returns to 0 with no
  restart (FR-005 recovery).
- Work that was *not* discarded produces its caption normally (SC-004).

---

## Scenario 4 — A failing worker does not take down the pool

**Covers**: FR-008, and the "worker fails while transcribing" edge case

```sh
cargo test --release --test worker_throughput -- --nocapture worker_failure_recovers
```

Inject a decode failure into one job.

**Expected**:

- `.metrics.total_transcription_errors` increments.
- Subsequent utterances are still transcribed.
- **`transcription_concurrency_limit` is unchanged and `in_flight` still reaches
  it.** This is the real assertion: a pool that leaks a decoder state per failure
  keeps working while quietly shrinking to nothing, and only this check catches
  it.

---

## Scenario 5 — GPU concurrency behaves and is memory-bounded

**Covers**: FR-011 on the GPU path — **requires a GPU**

```sh
cargo test --release --features cuda --test worker_throughput -- --nocapture
TRANSCRIPTION_CONCURRENCY=64 cargo run --release --features cuda
```

**Expected**:

- The default is 2 and is reported as `default for gpu backend`.
- Raising it increases `in_flight` and throughput until the device saturates.
- A value too large for the device fails **at startup**, while allocating
  decoder states, with a message naming memory as the constraint — not opaquely
  at first use. Each state costs ~233 MB for `base` (research R4), so provoking
  this on a large card needs a large model.

---

## Scenario 6 — Transcripts are unchanged

**Covers**: SC-006

```sh
cargo test --release --test transcription_fixtures -- --nocapture
```

Feature 001's harness, unchanged. Compare its report against one taken before
this feature.

**Expected**: identical transcript text for every fixture. This feature alters
throughput only; any difference here means the worker pool changed what users
read, which is a defect and not a tradeoff.

**Also required — the FR-003 leak test**: decode fixture A then fixture B on one
reused state, and assert B's transcript is byte-identical to B decoded on a fresh
state. Without it, nothing prevents a future `set_no_context(false)` from leaking
one speaker's words into another's caption (research R2, data-model.md).

**Limitation, stated plainly**: the corpus is non-speech only, so this currently
shows the pool did not change how silence and noise are transcribed. It cannot
yet show speech is unaffected. That gap closes when 001's T005 lands.

---

## What CI covers, and what it does not

CI runs Scenarios 1, 3, 4 and 6 — everything that needs no GPU and no model
beyond what the harness skips without.

Scenario 2's absolute throughput numbers are **machine-dependent** and must not
be asserted as fixed thresholds in CI; the ratio between concurrency 1 and
concurrency N is the portable measurement, and that is what the test asserts.

Scenario 5 needs GPU hardware CI does not have, as feature 001 established.

Stated plainly: **a green CI run proves work is shed rather than blocking, and
that the pool survives failure. It does not prove the throughput target.** That
is measured by hand and recorded in the PR, per Constitution Principle II.
