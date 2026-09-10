# Verification record

**Task T042.** What was actually verified for this feature, how, and what was
not.

Stated up front: **SC-001 was not measured, and SC-006 is partial.** Both are
explained below rather than left for a reader to notice.

## Hardware

| | |
|---|---|
| GPU | NVIDIA GeForce RTX 3090 Ti (24 GB), driver 595.84 |
| CPU | 32 threads |
| Model | `ggml-base.bin` |
| Build | `--release`, and `--release --features cuda` for the GPU runs |

## Success criteria

| | Criterion | Status | Evidence |
|---|---|---|---|
| SC-001 | Caption delay does not grow over a ten-minute four-speaker conversation | **Not measured** | See Outstanding. The mechanism that made delay grow — a queue nothing drained concurrently — is gone, and boundedness is proven, but the ten-minute conversation itself was not run. |
| SC-002 | ≥2x sustained throughput on identical hardware | **PASS on both backends** | CPU **2.79x** (7.70 s → 2.76 s), GPU **2.27x** (0.38 s → 0.17 s), 8 utterances, 1 worker vs 4. |
| SC-003 | Per-utterance time reduced by eliminating repeated state setup | **PASS, and small — as predicted** | State is now reused (`transcribe_and_write` takes `&mut WhisperState`). The saving is the measured `create_state` cost: 7.6 ms of 843 ms on CPU (0.9%), 3.0 ms of 41.6 ms on GPU (7.3%). See [baseline.md](./baseline.md). |
| SC-004 | Under induced overload the receive path keeps running and non-discarded work is unaffected | **PASS** | Submission is synchronous by signature. 1000 submissions against a full queue complete in under 100 ms; the old code awaited indefinitely at that point. |
| SC-005 | 100% of discards in both a counter and a log line | **PASS** | Counter and log increment at the same site in `submit`. 500 submissions against a queue of 4 produced exactly 496 counted discards. |
| SC-006 | Transcripts unchanged | **PASS on the corpus that exists** | All four fixtures byte-identical to the pre-feature output. Corpus is non-speech only — see Outstanding. |
| SC-007 | Keeping-up determinable from metrics alone | **PASS** | Queue depth, in-flight, concurrency limit, discards and duration percentiles all on `/k8s/metrics`; the reading table is in `contracts/telemetry.md` and `docs/README.md`. |

### SC-002 measurement

Eight utterances of the same fixture, decoded with one worker and then with
four, each worker owning a reused state against a shared context — the shipped
pool's shape. Reported as a **ratio**, never an absolute: absolute throughput is
machine-dependent, and the test asserts the ratio for that reason.

| Backend | 1 worker | 4 workers | Speedup |
|---|---|---|---|
| CPU | 7.70 s | 2.76 s | **2.79x** |
| GPU | 0.38 s | 0.17 s | **2.27x** |

### Fallback and configuration, end to end

Against the real release binary:

| Case | Result |
|---|---|
| unset, CPU backend | `transcription concurrency: 4 workers (default for cpu backend, 32 cores available)`, `transcription_concurrency_limit: 4` |
| unset, GPU backend | `transcription concurrency: 2 workers (default for gpu backend)`, limit 2, `compute_backend.kind: gpu` |
| `TRANSCRIPTION_CONCURRENCY=6` | `6 workers (from TRANSCRIPTION_CONCURRENCY)`, limit 6 |
| `=0` | refuses to start: "is not valid: transcription would never run. Accepted range is 1 to 32." |
| `=abc` | refuses: "'abc' is not a positive integer. Accepted range is 1 to 32." |
| `=-3` | refuses: "-3 is not a positive integer. Accepted range is 1 to 32." |
| `=64` | refuses: "exceeds the maximum of 32." |

Every refusal names the offending value **and** the range, per FR-011 and
Constitution Principle IV.

## Test coverage added

31 unit tests and 2 model-dependent integration tests, all passing, with
`cargo fmt --check`, `cargo clippy --all-targets --locked -- -D warnings`,
`cargo test --locked` and `uv lock --check` clean.

The ones worth naming, because they pin properties that fail silently:

- **`reused_state_does_not_leak_between_utterances`** — the whole of FR-003's
  enforcement. State reuse is safe only because `no_context` defaults to `true`;
  a single `set_no_context(false)` would start seeding each transcript with the
  previous speaker's words, intermittently and only under load.
- **`a_resource_returns_to_the_pool_after_a_panic`** — a pool that leaks one
  decoder state per failure keeps working while shrinking to nothing. Nothing
  else catches it until it is total.
- **`concurrency_is_actually_bounded`** — 24 jobs against 3 resources, peak
  in-flight asserted at exactly 3.
- **`a_waiter_woken_by_a_permit_always_finds_a_resource`** — the guard releases
  the resource before the permit. Reversed, a woken waiter finds an empty pool
  and stalls forever holding a permit.
- **`sustained_overload_keeps_the_queue_bounded`** — 500 submissions against a
  queue of 4: depth stays at 4, and all 496 drops are counted.
- **`submission_returns_promptly_when_the_queue_is_full`** — the correctness fix
  itself.

## Deviations from the plan

**Two, both forced by the codebase rather than chosen.**

1. **Test layout.** `tasks.md` put the concurrency tests in
   `tests/worker_throughput.rs`. `hammock` is a **binary crate with no library
   target**, so an integration test cannot reach `TranscriptionHandle` or the
   pool. Rather than restructure `main.rs` into a library — a large refactor of
   code unrelated to throughput — the queue, shedding, pool and configuration
   tests live as unit tests inside `src/`, where they touch the real types.
   `tests/worker_throughput.rs` keeps what genuinely needs a model: the FR-003
   leak check and the SC-002 measurement. The layout note is in that file's
   header so the next reader is not puzzled.

2. **The pool is generic over its resource.** `ResourcePool<T>` with
   `StatePool = ResourcePool<WhisperState>`. `StatePool::new` needs a real
   Whisper context, which would have made the concurrency mechanics untestable
   on a CI runner with no model — unacceptable for the part of this feature
   where a mistake costs audio. The generic exists solely so those tests can run
   against `ResourcePool<u32>`; production instantiates it one way.

**The scope addition was taken.** T016 — the Discord HTTP lookup on the 20 ms
voice tick path — is fixed. A cache hit still resolves inline (not a network
call); a miss is carried as `SpeakerLabel::Deferred` and resolved in the
dispatcher, off the receive path. Constitution Principle I names network calls
on that path explicitly, and closing a feature about not blocking the receive
path with the receive path still blocking would have been a strange result.

## Outstanding

| Item | Why | Consequence |
|---|---|---|
| **SC-001** — four speakers, ten minutes, delay must not grow | Needs a sustained multi-speaker run against a live bot. The synthetic generator measures throughput, not end-to-end caption delay through Discord. | The *cause* of growing delay is addressed and proven: concurrency is bounded and real, the queue cannot exceed capacity, and overload sheds instead of accumulating. But the ten-minute conversation was not run, so the criterion is not signed off. |
| **SC-006 for speech** | The fixture corpus is non-speech only; feature 001's T005 (real conversational clips with listened-to reference transcripts) is still blocked. | SC-006 shows the pool did not change how silence, noise and music are transcribed. It cannot yet show speech is unaffected. |
| **Shutdown drain under real load** | `drain` is implemented and wired to SIGTERM/Ctrl-C, but was exercised only with an idle queue. | The bounded-wait logic is straightforward and unit-testable in principle; a loaded drain has not been observed. |

None of these block the feature. The correctness fix — the audio receive path
can no longer be stalled by transcription — is verified and is the reason this
work was P1.
