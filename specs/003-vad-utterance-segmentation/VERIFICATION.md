# Verification record

**Task T044.** What was verified, how, and — at unusual length for this feature —
what was not.

**Headline: four of seven success criteria were not measured, including SC-001,
the claim this feature exists to make.** That is not a discovered surprise; it
was called out in [research.md](./research.md) R0 before implementation began,
and T041 (unblocking the speech fixtures) was written into
[tasks.md](./tasks.md) as the highest-value task in the list. It was not done.

## Success criteria

| | Criterion | Status | Evidence |
|---|---|---|---|
| SC-001 | WER improves ≥20% | **NOT MEASURED** | No speech fixtures. See Outstanding. |
| SC-002 | Zero words severed across boundaries | **NOT MEASURED** | No speech fixtures. The *mechanism* is verified — a sub-threshold pause no longer splits an utterance, and its samples are retained — but that is not the same as counting severed words in a real transcript. |
| SC-003 | Caption delay under 1.5 s | **PASS, as a property** | `close_delay_equals_the_silence_threshold` asserts the delay from last speech to utterance close is the 500 ms threshold, leaving ~1 s of the budget for transcription. Not measured end-to-end on a live call. |
| SC-004 | Silence and noise produce zero captions | **PASS** | All four non-speech fixtures rejected. `music-10s` previously wrote a caption reading "soft music"; it no longer does. |
| SC-005 | Median utterance ≥5 s, requests down ≥50% | **NOT MEASURED** | Needs conversational speech. `utterance_length_ms` reports the number; there is no real speech behind it. |
| SC-006 | Attribution not degraded | **NOT MEASURED** | Needs speech fixtures with known speakers. |
| SC-007 | No audio lost at departure | **PASS** | `/leave` now flushes before tearing down the call; shutdown flushes before draining. Unit-tested via `flush_emits_a_partial_utterance`. |

### SC-004 measurement

Fixture transcripts run through the production rejection rule:

| Fixture | Transcript | Before | After |
|---|---|---|---|
| `silence-5s` | `""` | no caption | no caption |
| `noise-fan-10s` | `""` | no caption | no caption |
| `noise-keyboard-10s` | `""` | no caption | no caption |
| `music-10s` | `"(soft music)"` | **caption written** | no caption |

The bot previously filtered exactly one literal, `[blank_audio]`, so
`(soft music)` was written into caption files as though a participant had said
the words "soft music". That was recorded as a known finding in feature 001's
VERIFICATION.md and deliberately left alone there, because that feature was not
allowed to change transcript output. It is fixed here.

### Configuration validation, end to end

Against the real release binary:

| Input | Result |
|---|---|
| `UTTERANCE_SILENCE_MS=abc` | `'abc' is not a positive integer. Accepted range is 100 to 5000.` |
| `UTTERANCE_SILENCE_MS=50` | `is below the minimum of 100.` |
| `UTTERANCE_MAX_SECS=45` | `exceeds the maximum of 30 (the recognizer processes a 30-second window and would truncate the excess).` |
| `UTTERANCE_MIN_SPEECH_MS=5000` + `UTTERANCE_MAX_SECS=1` | `no utterance could ever be long enough to transcribe, so the bot would run and caption nothing.` |
| `UTTERANCE_SILENCE_MS=150` | starts, warns about mid-sentence fragmentation |
| `CAPTION_CHUNK_SECS=3.0` | starts, warns that it no longer affects segmentation |

The cross-field case is the one no per-variable check catches: two individually
valid values combining into a bot that runs and captions nothing.

## Test coverage added

15 segmenter tests, 3 non-speech rejection tests, and the config tests, all
passing. `cargo fmt --check`, `cargo clippy --all-targets --locked -- -D warnings`
(with and without `--features cuda`), `cargo test --locked` and `uv lock --check`
all clean. 57 tests total across the repository.

The ones that matter:

- **`a_pause_shorter_than_the_threshold_does_not_split`** and
  **`quiet_samples_inside_an_utterance_are_kept`** — together these are FR-003,
  the rule the whole feature rests on. A 200 ms pause is an ordinary breath; the
  previous implementation would cut there, and dropping the quiet samples would
  splice two half-words together.
- **`a_single_lost_packet_does_not_end_an_utterance`** — network jitter is not
  someone finishing their sentence.
- **`a_split_carries_the_remainder_rather_than_dropping_it`** — asserts total
  samples in equals total samples out. Splitting at the maximum length is the
  easiest place in this design to lose audio silently.
- **`two_speakers_segment_independently`** — FR-006 is structural (one
  `Segmenter` per SSRC), and this asserts it stays that way.
- **`non_speech_transcripts_are_rejected_and_real_speech_is_not`** — the second
  half matters more than the first. `(music) thanks for watching` is real speech
  and must produce a caption; eating it would be a worse bug than the one fixed.

## Defects fixed

Two live bugs, both found by reading rather than by testing, both fixed here:

1. **Non-speech produced a false caption** (research R4). Described above.
2. **`/leave` dropped buffered audio** (research R5). `manager.remove(guild_id)`
   tore the call down with nothing flushing the segmenter first, losing up to a
   full chunk per speaker. `flush_stream` existed and was correct; it was simply
   never called on that path. Speaker disconnect was already handled; channel
   departure and shutdown were not. This feature would have made it worse —
   longer utterances mean more buffered audio to lose — so the flush had to land
   in the same change.

## Deviations from the plan

**One, forced.** T034 said to *move* `is_sound_event_annotation` from
`tests/support/mod.rs` into `src/`. It is now in both places. `hammock` is a
binary crate with no library target, so the fixture harness in `tests/` cannot
reach production code — the same constraint feature 002 hit. The production copy
in `src/transcription/mod.rs` is the source of truth and decides what reaches
caption files; the harness copy only decides what the harness reports. Both carry
a comment saying so.

**`CAPTION_CHUNK_SECS` was retained with a warning** rather than removed (T042).
Removing it would break `.env` files that set it; accepting it silently would
leave an operator who had tuned it believing it still worked. It now warns and
points at the two variables that replaced it.

## Outstanding

| Item | Why | Consequence |
|---|---|---|
| **T041 — speech fixtures** | Requires sourcing public-domain clips and transcribing them **by listening**. Cannot be automated, and must not be: deriving references from a recognizer makes the measurement circular. | SC-001, SC-002, SC-005 and SC-006 are unmeasured. |
| **SC-003 end-to-end** | Measured as a segmenter property, not with a stopwatch on a live Discord call. | The threshold arithmetic is right; the full path has not been timed. |
| **Live-call behavior** | No Discord call was made. The segmenter is exhaustively unit-tested against a synthetic tick stream; the wiring into `AudioAggregator` is not. | Open-mic participants, multi-speaker interleaving and mute-toggling are covered by reasoning and unit tests, not observation. |

## What a green test run does and does not prove

It proves the segmenter obeys its rules, that non-speech no longer produces
captions, that configuration is validated, and that audio is no longer dropped
on `/leave`.

**It does not prove that transcripts got better.** That is SC-001, it is the
reason this feature was written, and it remains unmeasured. Constitution
Principle II — "Transcript Accuracy Is Measured, Not Asserted" — is not satisfied
by this change. Do not report it as met.
