# Pre-feature segmentation baseline

**Tasks**: T001, T002 | **Serves**: SC-004, SC-005, and the SC-001/SC-002 comparison

What the bot did before speech-aware segmentation, recorded so the "after" has
something to be measured against.

## Segmentation behavior (T001)

| Property | Value | Where |
|---|---|---|
| Boundary rule | fixed sample count, no speech awareness | `src/voice/mod.rs` `consume_samples`, `while entry.samples.len() >= self.chunk_samples` |
| Chunk length | `CAPTION_CHUNK_SECS` = 3.0 s → 48 000 samples at 16 kHz | `src/config.rs` |
| Silence flush | **3.0 s** (`silence_flush: state.chunk_duration`) | `src/main.rs:543` |
| Caption timestamp | `Utc::now()` at dispatch — when the chunk was cut | `src/voice/mod.rs` `dispatch_chunk` |
| Minimum speech duration | none | — |
| Maximum utterance | none (chunking bounds it incidentally) | — |
| Flush on `/leave` | **none — audio is dropped** | `src/main.rs:657` |
| Flush on speaker disconnect | yes | `src/voice/mod.rs:214` |

### Transcription requests per minute of audio

With a fixed 3 s chunk, continuous speech produces **20 requests per minute of
audio**, plus one flush per utterance tail.

This is the SC-005 denominator. The criterion asks for a ≥50% reduction, so
speech-aware segmentation passes at **≤10 requests per minute** of continuous
speech — which a 5 s median utterance length reaches by definition.

Worth stating why this matters beyond tidiness: whisper.cpp processes a **fixed
30-second window per request regardless of input length**. A 3-second chunk
therefore pays for 30 seconds of compute and uses a tenth of it. Longer
utterances do not merely add context, they stop wasting most of every request.

## Non-speech transcripts (T002)

Measured with the fixture harness from feature 001, `ggml-base.bin`, on both CPU
and GPU (identical results):

| Fixture | Transcript today | Caption written today? |
|---|---|---|
| `silence-5s` | `""` | no |
| `noise-fan-10s` | `""` | no |
| `noise-keyboard-10s` | `""` | no |
| `music-10s` | `"(soft music)"` | **yes** |

The bot filters exactly one literal, `[blank_audio]`
(`src/transcription/mod.rs`, `transcribe_and_write`). `(soft music)` is not that
literal, so it is written into a caption file as though a participant had said
the words "soft music".

**This is the SC-004 "before", and it currently fails.** Three of four fixtures
already produce nothing; the fourth is the live false-caption bug this feature
fixes. It was first recorded as a known finding in feature 001's
VERIFICATION.md, deliberately left alone there because that feature was not
allowed to change transcript output.

## What is not in this baseline

**Word error rate, severed-word counts, median utterance length for real speech,
and attribution accuracy — the SC-001, SC-002, SC-005 and SC-006 baselines — are
absent**, because measuring them requires conversational speech fixtures with
reference transcripts and the corpus has none.

That is not an oversight in this document. It is the reason
[research.md](./research.md) R0 exists, and the reason T041 is the
highest-value task in [tasks.md](./tasks.md). Without those baselines this
feature's headline claim has no "before" to be compared against.
