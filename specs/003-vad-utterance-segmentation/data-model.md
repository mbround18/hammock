# Phase 1 Data Model: Speech-Aware Utterance Segmentation

**Feature**: [spec.md](./spec.md) | **Plan**: [plan.md](./plan.md)

Nothing here is persisted. These are in-memory values on the voice tick path,
which constrains them: Constitution Principle I means no I/O, no awaiting, and no
buffer that grows without bound.

## Entity: Utterance

A contiguous span of one participant's speech — the unit submitted for
transcription, replacing the fixed-size chunk.

| Field | Type | Description |
|---|---|---|
| `samples` | `Vec<i16>` | 16 kHz mono PCM accumulated so far. Bounded by `max_utterance` (FR-004); that bound is what keeps this from being the unbounded queue Principle I forbids. |
| `speaker` | `SpeakerIdentity` | Fixed when the utterance opens, carried to the job unchanged (FR-013). |
| `started_at` | `DateTime<Utc>` | **When the first sample arrived**, not when the utterance closed (FR-014). |
| `last_speech` | `Instant` | Tick of the most recent speech activity. The silence threshold measures from here. |
| `speech_ticks` | `usize` | Ticks carrying speech, for the minimum-duration test (FR-005). Counted rather than derived from `samples.len()` so that a gap from packet loss is not mistaken for speech. |

**Why `started_at` is the first sample.** Today it is `Utc::now()` at dispatch —
the moment the chunk was cut. That was wrong by up to a chunk length already, and
this feature makes utterances longer, so it would get worse. A caption
timestamped when transcription happened to begin is not a record of when anyone
spoke.

**Validation**:

- An utterance is submitted only if `speech_ticks * TICK >= min_speech` (FR-005).
- `samples.len()` never exceeds `max_utterance * sample_rate`; reaching it forces
  a close (FR-004).
- An utterance with zero samples is never submitted, and does not count as a
  non-speech rejection — nothing was rejected, nothing happened.

## Entity: Segmenter

Per-participant state deciding where utterances begin and end. One instance per
SSRC, which is what makes FR-006 structural: two participants share no state, so
one speaker's pauses cannot affect another's boundaries.

| Field | Type | Description |
|---|---|---|
| `current` | `Option<Utterance>` | Open utterance, if any. |
| `config` | `SegmentationConfig` | The three thresholds. |

### The one operation that matters

```text
on_tick(speaking: bool, samples: &[i16], now: Instant) -> Option<Utterance>
```

Called once per participant per 20 ms tick. Returns an utterance when one closes.
Pure: no I/O, no awaiting, no allocation beyond extending the sample buffer —
which is what lets it run on the tick path and, separately, what lets it be
tested exhaustively without Discord (research R7).

### State transitions

| From | Event | To | Requirement |
|---|---|---|---|
| Idle | tick with speech | Open, `started_at` = now, `last_speech` = now | FR-001, FR-014 |
| Idle | tick without speech | Idle | — |
| Open | tick with speech | Open, extend samples, `last_speech` = now | FR-001 |
| Open | tick without speech, `now - last_speech < silence_threshold` | Open, **samples still extended** | FR-003 |
| Open | tick without speech, `now - last_speech >= silence_threshold` | Idle, **emit** | FR-002 |
| Open | `samples` reach `max_utterance` | Idle, **emit**, reopen if speech continues | FR-004 |
| Open | speaker disconnects, bot leaves, or shutdown | Idle, **emit** | FR-007 |
| Open | emitted but `speech_ticks * TICK < min_speech` | Idle, **discard** | FR-005 |

**The two rows that carry this feature.**

Row 4 — *silence shorter than the threshold keeps the utterance open, and keeps
appending samples* — is FR-003, and it is the reason words stop being severed.
The silence is part of the utterance, because a pause inside a sentence is part
of the sentence.

Row 6 — the maximum-length split — is where "least disruptive point" lives. The
honest reading, given the constraint that this runs on the tick path with no
lookahead, is **split at the most recent tick without speech activity**, falling
back to the hard limit when there has been none. Searching decoded audio for the
quietest sample would be signal processing on the 20 ms path, which Principle I
rules out.

**Bounding memory (Principle I)**: `max_utterance` at 20 s and 16 kHz mono is
320 000 samples, 640 KB per speaking participant. That is the number that makes
FR-004 a correctness requirement rather than a tuning preference.

## Entity: SegmentationConfig

| Field | Type | Default | Requirement |
|---|---|---|---|
| `silence_threshold` | `Duration` | 500 ms | FR-002, FR-003 |
| `max_utterance` | `Duration` | 20 s | FR-004 |
| `min_speech` | `Duration` | 300 ms | FR-005 |

Ranges, validation and the reasoning behind each default are in
[contracts/configuration.md](./contracts/configuration.md). All three are
resolved once at startup and immutable thereafter.

**These are reasoned starting points, not tuned values** (research R6). Nothing
has been tuned against real speech because there is no real speech in the fixture
corpus. Calling them tuned would be the assertion Principle II forbids.

## Entity: NonSpeechRejection

Not a stored value — the decision to write no caption for a transcript that
contains no speech.

A transcript is rejected when its normalized form is:

- empty, or
- composed solely of bracketed or parenthesized annotations — `[blank_audio]`,
  `(soft music)`, `[APPLAUSE]`.

The classifier already exists and is already tested:
`tests/support::is_sound_event_annotation`, written for feature 001. This feature
moves it into `src/` so production can use it, rather than reimplementing it.

**Why annotations count as non-speech.** They are the recognizer correctly
reporting that nothing was said, in its own notation. That is categorically
different from a hallucinated sentence, and both are different from speech. Today
only the literal `[blank_audio]` is filtered, so `(soft music)` reaches a caption
file as though someone had spoken it (research R4) — a live false-caption bug.

**Every rejection increments a counter** (FR-012). A rejection that is not
counted is invisible, and an over-aggressive threshold would then look like a
quiet channel — which is the failure mode Principle V exists to prevent.

## Relationship to existing types

- `AudioBuffer` (`src/voice/mod.rs`) is **replaced** by `Segmenter`. The
  `chunk_samples` field and the `while samples.len() >= chunk_samples` loop go
  with it; `CAPTION_CHUNK_SECS` loses its segmentation role.
- `TranscriptionJob` (`src/transcription/mod.rs`) is unchanged in shape. Its
  `started_at` now means what its name says. Its `pcm` gets longer and more
  variable, which is fine — the job already carries a `Vec<i16>` of arbitrary
  length.
- `AppMetrics` gains an utterance-length distribution and a rejection counter.
  The distribution reuses the `DurationWindow` shape built for feature 002:
  lifetime totals plus windowed percentiles.
- `SpeakerLabel` (feature 002) is unaffected. Attribution is fixed when the
  utterance opens and carried through unchanged, which is FR-013.
