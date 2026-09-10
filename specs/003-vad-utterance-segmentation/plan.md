# Implementation Plan: Speech-Aware Utterance Segmentation

**Branch**: `003-vad-utterance-segmentation` | **Date**: 2026-09-09 | **Spec**: [spec.md](./spec.md)

**Input**: Feature specification from `/specs/003-vad-utterance-segmentation/spec.md`

## Summary

Cut audio where speech stops, not where a sample counter reaches a threshold.

Four pieces of work:

1. **Extract the segmenter** — pull boundary logic out of `AudioAggregator` into
   a unit driven by (speech activity, samples, thresholds), so every requirement
   here is testable without Discord, a model, or a network.
2. **Segment on speech** — open an utterance when a participant starts
   transmitting, close it after a silence threshold or at a maximum length, drop
   anything below a minimum.
3. **Stop writing false captions** — reject transcripts that are empty or are
   nothing but the recognizer's own non-speech annotations.
4. **Stop losing buffered audio** — flush on channel departure and shutdown, not
   only on speaker disconnect.

**Read the following before anything else in this plan.**

### This feature cannot be verified as specified

**Five of its seven success criteria require conversational speech fixtures with
reference transcripts, and none exist.** Feature 001's T005 is still blocked.
SC-001, SC-002, SC-005 and SC-006 are unmeasurable without it; SC-003, SC-004 and
SC-007 are measurable now.

Features 001 and 002 each lost one criterion to this. This feature loses most of
them — and unlike those two, this one **deliberately changes transcript output**,
which is exactly the case Constitution Principle II says must be measured rather
than asserted.

**Recommendation: unblock T005 before implementing this feature.** It is a small
task — source three to five public-domain clips, transcribe them by listening —
and it is the difference between shipping a measured improvement and shipping a
plausible one.

Implementation can proceed regardless, because two of the four work items above
fix **live defects** that need no fixtures to demonstrate:

- Non-speech audio produces a caption reading `(soft music)` today (research R4).
- Audio buffered when the bot leaves a channel is dropped silently (research R5).

But SC-001 must not be reported as met on the strength of a green test run.

## Technical Context

**Language/Version**: Rust 2024 edition, toolchain 1.91

**Primary Dependencies**: `songbird` 0.6 for the voice tick and its per-SSRC
speaking/silent split — the segmentation signal this feature is built on;
`whisper-rs` 0.15.1 for transcription. **No new dependency, and deliberately no
VAD model** — see research R2.

**Storage**: Filesystem only. Unchanged.

**Testing**: `cargo test --locked` (a mandatory CI gate). The segmenter is
extracted specifically so it can be unit-tested against a synthetic tick stream
(research R7). `hammock` is a binary crate with no library target, so tests live
in `#[cfg(test)]` modules inside `src/` — the layout constraint feature 002
discovered.

**Target Platform**: Linux x86_64 containers, CPU and CUDA variants.

**Performance Goals**: Median caption delay after end of speech under 1.5 s
(SC-003); median utterance length at least 5 s, cutting transcription requests
per minute of audio by at least 50% (SC-005).

**Constraints**: The 20 ms voice tick must not await anything (Constitution
Principle I) — segmentation runs on that path, so it must be pure bookkeeping
with no I/O and no allocation proportional to history. Speaker attribution must
survive longer utterances (FR-013). Decoding parameters and model selection are
held constant; those belong to feature 004.

**Scale/Scope**: One new module, one rewritten buffering path, three new
configuration variables, two new metrics, one caption-filter change, one flush
fix.

## Constitution Check

*GATE: evaluated before Phase 0, re-evaluated after Phase 1 design.*

| Principle | Assessment |
|---|---|
| **I. Real-Time Voice Fidelity** | **Directly engaged — segmentation runs on the tick path.** Every decision here happens inside `on_voice_tick`, so it must be arithmetic and bookkeeping only: no I/O, no awaiting, and no buffer that grows without bound. FR-004's maximum utterance length is what bounds memory, and it is a correctness requirement on this principle, not a tuning knob. Feature 002 made submission non-blocking, so a closed utterance still cannot stall the tick. |
| **II. Transcript Accuracy Is Measured, Not Asserted** | **Engaged more than any feature so far, and currently obstructed.** This feature exists to change transcript output. The principle requires evaluation against a committed fixture set before and after, reported in the change description. Five of seven criteria cannot be evaluated because the speech corpus does not exist (research R0). **This is the gate that is at risk**, and the plan says so rather than proceeding as if measured. |
| **III. Hardware-Optional Parity** | **Unaffected.** Segmentation is CPU-side bookkeeping identical on both variants. Rejecting the VAD model (research R2) keeps it that way — a second model would have been another artifact for every self-hoster. |
| **IV. Configuration Over Recompilation** | **Served.** Three new tunables, each with a documented default safe for a CPU-only self-hoster, an entry in `.env.sample`, and startup validation naming both the bad value and the accepted range. The spec's own edge case — "silence threshold configured very low" — is why the range is documented rather than merely bounded. |
| **V. Observable by Default** | **Served.** FR-011 exposes the utterance-length distribution and FR-012 counts non-speech rejections. Together they make segmentation misconfiguration visible: a collapsing median length means the silence threshold is too low, and a climbing rejection count means it is too high or the room is too loud. |

**Gate result: PASS on principles I, III, IV, V. Principle II is at risk and the
risk is recorded, not waived.** No principle is violated by the design; one
cannot be *satisfied* until a blocked dependency lands, which is a scheduling
problem rather than a design problem.

Complexity Tracking is empty.

## Project Structure

### Documentation (this feature)

```text
specs/003-vad-utterance-segmentation/
├── spec.md              # Feature specification
├── plan.md              # This file
├── research.md          # Phase 0 output
├── data-model.md        # Phase 1 output
├── quickstart.md        # Phase 1 output
├── contracts/           # Phase 1 output
│   ├── telemetry.md     # utterance-length and rejection metrics
│   └── configuration.md # the three segmentation tunables
├── checklists/
│   └── requirements.md  # Spec quality checklist
└── tasks.md             # Created by /speckit-tasks, not this command
```

### Source Code (repository root)

```text
src/
├── config.rs            # + three segmentation tunables, validated at startup
├── main.rs              # + flush on channel departure and shutdown (FR-007)
├── voice/
│   ├── mod.rs           # AudioAggregator drives the segmenter instead of counting samples
│   └── segmenter.rs     # NEW — utterance boundaries, and where the tests live
├── transcription/
│   └── mod.rs           # reject empty and annotation-only transcripts (FR-009)
└── telemetry/
    ├── metrics.rs       # + utterance length distribution, non-speech rejections
    └── server.rs        # + both on /k8s/metrics and in the Swagger schema
```

**Structure Decision**: One new file. The segmenter is extracted rather than
written in place because it is the whole feature and, left inside
`AudioAggregator`, it would be reachable only through songbird events — untestable
without a live Discord call, which is how the current fixed-chunk logic came to
have no tests at all. Everything else is modified in place.

## Design notes carried from Phase 0

Three findings that shape the tasks, so they are not rediscovered later:

- **The speech signal is already there and is being discarded.** songbird's
  `VoiceTick` separates `speaking` from `silent` per SSRC per 20 ms tick, and
  Discord clients apply their own VAD before transmitting — so "a packet arrived"
  already means "the sending client believes this person is speaking." The
  current code receives this and cuts at a sample count anyway (research R1).
- **No VAD model.** whisper-rs exposes one, but it needs a separate Silero model
  file, and R1 plus content rejection cover every requirement without it. Revisit
  only if FR-012's counter says otherwise (research R2).
- **Non-speech rejection is a transcript-content check**, using the annotation
  classifier already written and tested for feature 001. This makes SC-004
  verifiable today (research R4).

## Phase 1 artifacts

- [data-model.md](./data-model.md) — the utterance, the segmenter state machine,
  and the transitions each functional requirement pins.
- [contracts/telemetry.md](./contracts/telemetry.md) — utterance-length
  distribution and non-speech rejection counter, extending 001's and 002's
  `/k8s/metrics` contracts additively.
- [contracts/configuration.md](./contracts/configuration.md) — the three
  tunables, their ranges, and why each default is where it is.
- [quickstart.md](./quickstart.md) — how to prove each criterion, and which four
  cannot be proven yet.

## Success criteria, restated honestly

| Criterion | Status under this plan |
|---|---|
| SC-001, SC-002, SC-005, SC-006 | **Blocked on feature 001's T005.** Measurable the day speech fixtures land; not before. |
| SC-003 (delay <1.5 s) | Measurable now, as a property of the segmenter: delay from last speech tick to utterance close is the silence threshold by construction. |
| SC-004 (no captions from silence or noise) | **Measurable now** against the existing non-speech fixtures, and it currently fails — `music-10s` produces `(soft music)`. |
| SC-007 (no audio lost at departure) | Measurable now. Currently fails: nothing flushes on `/leave`. |

## Complexity Tracking

No constitutional violations. This table is intentionally empty.

Extracting the segmenter into its own module is a testability decision, not a
deviation: it adds no crate, dependency, or service.

## Post-Design Constitution Re-Check

Re-evaluated after the Phase 1 artifacts were written:

- **Principle I** — the segmenter in [data-model.md](./data-model.md) is a state
  machine over counters. It performs no I/O, and FR-004's maximum length bounds
  its memory, so the tick path stays arithmetic.
- **Principle II** — unchanged and still the risk. The design makes the
  measurement *possible* the moment fixtures exist: holding decoding parameters
  constant means any WER change is attributable to segmentation alone.
- **Principle IV** — [contracts/configuration.md](./contracts/configuration.md)
  validates all three tunables at startup, including the cross-field rule that
  minimum speech must be shorter than maximum utterance.
- **Principle V** — [contracts/telemetry.md](./contracts/telemetry.md) makes both
  misconfiguration directions visible from metrics alone.
- **No new violations introduced.**
