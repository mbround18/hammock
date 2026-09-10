# Quickstart: Validating Speech-Aware Segmentation

**Feature**: [spec.md](./spec.md) | **Plan**: [plan.md](./plan.md)

How to prove this feature works — and, for four of its seven criteria, why you
cannot yet.

**Read this first.** SC-001, SC-002, SC-005 and SC-006 all require conversational
speech with reference transcripts. The fixture corpus contains none, and feature
001's T005 is blocked. Scenarios 1, 2 and 4 run today and cover the rest;
Scenario 5 is written out ready for the day fixtures land. See research R0.

## Prerequisites

| Scenario | Requires |
|---|---|
| 1 (segmenter behavior) | Nothing — no model, no network, no Discord |
| 2 (non-speech rejection) | A Whisper model on disk |
| 3 (flush on departure) | Nothing |
| 4 (metrics and configuration) | A Whisper model on disk |
| 5 (accuracy) | **Speech fixtures that do not exist yet** |

---

## Scenario 1 — Boundaries follow speech, not a sample counter

**Covers**: FR-001 through FR-006, FR-014, SC-003

```sh
cargo test --locked voice::segmenter
```

The segmenter is driven by a synthetic tick stream — a sequence of
(speaking, samples) pairs — so every rule is exercised deterministically in
milliseconds with no model and no Discord (research R7).

**Expected**, one assertion per rule:

- A pause **shorter** than `UTTERANCE_SILENCE_MS` does not close the utterance,
  and the quiet samples stay in it (FR-003). *This is the rule that stops words
  being severed.*
- A pause **at or beyond** the threshold closes it (FR-002).
- Continuous speech past `UTTERANCE_MAX_SECS` splits at the most recent quiet
  tick, or at the hard limit if there has been none (FR-004).
- A burst shorter than `UTTERANCE_MIN_SPEECH_MS` is dropped, and counts as
  neither an utterance nor a non-speech rejection (FR-005).
- Two SSRCs interleaved produce independent boundaries; one speaker's pause never
  closes another's utterance (FR-006).
- A single missing tick — packet loss — does **not** end an utterance early
  (spec edge case).
- The emitted `started_at` is the arrival time of the **first** sample, not of
  the close (FR-014).

**SC-003 is measured here**, not with a stopwatch: the delay between the last
speech tick and the utterance closing is the silence threshold by construction,
so the assertion is that the default leaves room inside the 1.5 s budget.

---

## Scenario 2 — Silence and noise produce no captions

**Covers**: FR-008, FR-009, SC-004 — **runs today, and currently fails**

```sh
cargo test --release --test transcription_fixtures -- --nocapture
```

**Expected**: zero caption lines from all four non-speech fixtures.

**Current behavior**: `music-10s` transcribes as `(soft music)`, and the bot
filters only the literal `[blank_audio]`, so that text is written into a caption
file today as though someone had said it (research R4). Three of the four
fixtures already produce nothing.

This is the one accuracy criterion in this feature that needs no new fixtures:
silence, fan noise, keyboard noise and music are all already committed.

Verify the counter moves too — a rejection nobody counted is invisible:

```sh
curl -s localhost:8080/k8s/metrics | jq '.metrics.total_segments_rejected_as_non_speech'
```

---

## Scenario 3 — No audio is lost when the bot leaves

**Covers**: FR-007, SC-007 — **runs today, and currently fails**

```sh
cargo test --locked voice::segmenter::tests::flush
```

Then by hand: join a channel, speak, and issue `/leave` **while still speaking**.

**Expected**: the partial utterance is transcribed and appears in the caption
file.

**Current behavior**: `/leave` calls `manager.remove(guild_id)` and tears the
call down with nothing flushing the buffers (`src/main.rs:657`), so the tail is
dropped silently. `flush_stream` exists and is correct — it is simply never
called on this path (research R5). Speaker disconnect is already handled; channel
departure and shutdown are not.

This feature makes the bug worse before it fixes it: longer utterances mean more
buffered audio to lose.

---

## Scenario 4 — Configuration and metrics

**Covers**: FR-010, FR-011, FR-012

```sh
UTTERANCE_SILENCE_MS=abc  cargo run --release   # refuses, names value and range
UTTERANCE_MAX_SECS=45     cargo run --release   # refuses, explains the 30s window
UTTERANCE_MIN_SPEECH_MS=5000 UTTERANCE_MAX_SECS=1 cargo run --release  # refuses: nothing would transcribe
UTTERANCE_SILENCE_MS=150  cargo run --release   # starts, but warns about fragmentation
cargo run --release                              # defaults, reported at startup
```

**Expected**: every refusal names the offending value **and** the accepted range.
The cross-field case is the one a per-variable check would miss — two
individually valid values combining into a bot that transcribes nothing.

Then, while running:

```sh
curl -s localhost:8080/k8s/metrics | jq '.metrics.utterance_length_ms'
```

**Expected**: `p50_ms` in the thousands for conversational speech. The reading
table in [contracts/telemetry.md](./contracts/telemetry.md) says what each shape
means — a `p50` collapsing toward `min_speech` means the threshold is too low, a
`p50` pinned at the maximum means boundaries are coming from the length cap
rather than from speech.

---

## Scenario 5 — Transcript accuracy

**Covers**: SC-001, SC-002, SC-005, SC-006 — **BLOCKED**

```sh
cargo test --release --test transcription_fixtures -- --nocapture
```

**This scenario cannot be run.** It needs conversational speech fixtures with
reference transcripts, and the corpus has none. Feature 001's T005 is blocked
because reference transcripts must be produced by listening, which is the one
step that cannot be automated — and must not be, since deriving them from a
recognizer would make the measurement circular.

When fixtures land, this scenario is:

1. Record a baseline with fixed-interval segmentation (git stash, or the
   pre-feature commit).
2. Run the same fixtures through speech-aware segmentation.
3. Compare, holding decoding parameters and model constant:
   - **SC-001**: WER improves by ≥20% relative to the baseline.
   - **SC-002**: zero words severed across caption boundaries.
   - **SC-005**: median `utterance_length_ms` ≥ 5 s, and transcription requests
     per minute of audio down ≥50%.
   - **SC-006**: speaker attribution accuracy not worse than baseline.

Until then, **SC-001 must not be reported as met.** A green test run proves the
segmenter follows its rules; it says nothing about whether transcripts improved,
which is the entire point of this feature.

---

## What CI covers, and what it does not

CI runs Scenarios 1, 3 and 4 in full, and Scenario 2 as far as the model-free
assertions go — the fixture harness skips without a model, and that skip is not
a pass.

Stated plainly: **a green CI run proves the segmenter obeys its own rules and no
longer drops audio on departure. It does not prove that transcripts got better.**
That claim requires Scenario 5, and Scenario 5 requires fixtures that do not
exist.
