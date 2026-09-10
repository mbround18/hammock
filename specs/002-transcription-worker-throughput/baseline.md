# Per-utterance baseline (pre-feature)

**Task**: T002 | **Serves**: SC-003 ("per-utterance transcription time is reduced
by the elimination of repeated decoder-state setup")

Measured before the worker pool landed, so SC-003 is judged against a number
rather than an impression. Method and full context in
[research.md](./research.md) R3.

## Measurement

Host: RTX 3090 Ti, 32 cores, `ggml-base.bin`, `--release`. A 1-second utterance —
deliberately short, because a short utterance is where fixed per-call cost
matters most. Median of 5 runs.

| Backend | `create_state` | `full()` decode | Setup as share of total |
|---|---|---|---|
| CPU | 7.6 ms | 835 ms | **0.9%** |
| GPU | 3.0 ms | 38.6 ms | **7.3%** |

Decode time itself did not differ between a fresh state and a reused one
(GPU: 38.6 ms fresh vs 36.5 ms reused — inside run-to-run noise). So the saving
available to FR-002 is exactly the `create_state` call, and nothing more.

## What this means for SC-003

**The ceiling is 0.9% on CPU and 7.3% on GPU**, and lower in practice: the bot's
default chunk is 3 seconds (`CAPTION_CHUNK_SECS=3.0`), not 1, which dilutes fixed
cost further.

SC-003 is therefore satisfied by any measurable reduction in median short-utterance
time up to that ceiling. It should be reported as the small number it is.

**Where the feature's value actually is.** The spec's Input describes the
accelerator "idling between chunks", and SC-003 frames repeated state setup as a
principal cost. It is not. The idle time comes from **serialization** — the
worker decodes exactly one chunk at a time — which is FR-001, not FR-002. Along
with FR-004 (never blocking the receive path), that is where this feature earns
its keep.

FR-002 remains worth doing because it is nearly free once a worker pool exists:
each worker owns a state for its lifetime, so reuse falls out of the design
rather than being built.

## How to re-measure

Time `ctx.create_state()` against `state.full()` separately, on a short
utterance, median of several runs, in a release build. Compare like with like:
a debug build inverts the ratio, because the Rust-side overhead that debug
inflates is not where either cost lives.
