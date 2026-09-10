# Phase 0 Research: Speech-Aware Utterance Segmentation

**Feature**: [spec.md](./spec.md) | **Date**: 2026-09-09

Every finding was verified against this repository, the vendored `whisper.cpp`,
songbird 0.6.0, or measured here. Two findings change what this feature can
honestly claim, and both are stated rather than buried.

## R0: This feature cannot be verified without speech fixtures

**Decision**: State it first, because it governs everything else.

**Five of seven success criteria require conversational speech with reference
transcripts**, and the fixture corpus contains none. Feature 001's T005 — real
recorded speech with transcripts produced by listening — is still blocked, and
the spec's own Assumptions say "Fixture recordings with reference transcripts
are required for every success criterion here."

| Criterion | Needs speech fixtures? |
|---|---|
| SC-001 (WER improves ≥20%) | **Yes** — WER is undefined without a reference |
| SC-002 (zero severed words) | **Yes** |
| SC-003 (caption delay <1.5 s) | No — timing, measurable with any audio |
| SC-004 (silence/noise produce zero captions) | **No** — the existing non-speech fixtures are exactly this |
| SC-005 (median utterance ≥5 s) | **Yes** — "for conversational speech" |
| SC-006 (attribution not degraded) | **Yes** |
| SC-007 (no audio lost at departure) | No — presence of output, not its content |

This is a harder block than features 001 and 002 faced. There it degraded one
criterion each; here it is most of them, and unlike those features this one
**deliberately changes transcript output**, which is precisely what Constitution
Principle II says must be measured rather than asserted.

**Recommendation**: unblock T005 before implementing. The work is small — source
three to five public-domain clips and transcribe them by listening — and without
it this feature ships with its central claim (better transcripts) untested.

Implementation can proceed in the meantime: FR-007's audio-loss bug (R5) and
FR-009's false-caption bug (R4) are both real defects fixable and testable today.
But **SC-001 must not be reported as met on the strength of a green test run**.

## R1: What speech-activity signal already exists?

**Decision**: songbird's per-tick speaking/silent split is the primary boundary
signal. No model-based VAD is required for segmentation.

**Evidence**: `VoiceTick` carries `speaking: HashMap<u32, VoiceData>` and
`silent: HashSet<u32>` (`songbird-0.6.0/src/events/context/data/voice.rs:14-19`),
per 20 ms tick, per SSRC. An SSRC appears in `speaking` only when a voice packet
arrived for that tick.

That is a stronger signal than it first appears: **Discord clients run their own
voice-activity detection or push-to-talk and stop transmitting when the user is
not speaking.** So "a packet arrived" already means "the sending client believes
this person is talking", decided at the microphone with far better information
than we have downstream. It is free, exact, per-participant, and already
delivered at the 20 ms resolution FR-001 wants.

The current code receives this signal and throws it away: `on_voice_tick`
accumulates samples into a buffer and cuts at a fixed `chunk_samples` count
(`src/voice/mod.rs:222-234, 261-270`), consulting `tick.silent` only to check an
elapsed-time flush.

**Where it is not sufficient**: a participant with open-mic transmission ("always
transmit") streams continuously, including room noise, so their SSRC is never
silent. Segmentation for them degrades to the maximum-utterance-length rule
(FR-004), and their noise is caught by content rejection instead (R4). Packet
loss is the mirror image — a gap that is not the speaker finishing — and is
addressed by the silence threshold being a duration rather than a single missing
tick (FR-003 handles both cases with one mechanism).

**Alternatives considered**: energy or zero-crossing VAD over the decoded PCM.
Rejected as the *primary* signal — it re-derives, worse, what the sending client
already decided, and it fires on the keyboard and fan noise the fixture corpus
is full of.

## R2: Is the whisper.cpp VAD model needed?

**Decision**: No, and it should not be adopted in this feature.

**Evidence**: whisper-rs 0.15.1 does expose a complete VAD API —
`WhisperVadContext`, `whisper_vad_init_from_file_with_params`,
`segments_from_samples` (`whisper-rs-0.15.1/src/whisper_vad.rs`). It works. But
`whisper_vad_init_from_file_with_params` requires a **separate Silero VAD model
file**, which means another model to download at startup, another artifact in
the image, and another failure mode when it is missing.

**Why it is not needed**: R1 supplies boundaries and R4 supplies content
rejection, between them covering every functional requirement here. Adding a
second model to do a job two existing mechanisms already do would be complexity
the constitution asks us to justify, and there is no measurement supporting it —
which is exactly the bar the spec's own Assumption sets ("whether it is needed is
decided against measurements rather than assumed").

**When to revisit**: if measurement against real speech fixtures shows
open-mic participants dominating the discard counter (FR-012), or WER on
segment boundaries staying poor. Both are visible in the metrics this feature
adds, so the decision can be revisited with evidence rather than re-argued.

## R3: The current segmentation, precisely

**Decision**: Recorded here because three separate defects hide in it, and the
plan addresses each.

| Behavior | Where | Consequence |
|---|---|---|
| Cuts at a fixed sample count | `src/voice/mod.rs:261-270` | Words severed mid-syllable (FR-001). At `CAPTION_CHUNK_SECS=3.0` and 16 kHz that is every 48 000 samples, wherever they fall. |
| Silence flush waits `chunk_duration` | `src/main.rs:543` (`silence_flush: state.chunk_duration`) | **A 3-second wait after speech ends** before the tail is transcribed (US2, SC-003). The delay the spec complains about is this line. |
| `started_at: Utc::now()` at dispatch | `src/voice/mod.rs` `dispatch_chunk` | The caption timestamp is when the chunk was *cut*, not when speech began (FR-014). Off by up to the chunk length, and by more once utterances get longer. |
| No minimum speech duration | — | A 100 ms fragment is transcribed like any other (FR-005). |

`flush_expired` is called only for SSRCs present in `tick.silent`, which is the
right trigger; what is wrong is that it waits three seconds and then flushes
whatever fixed-size remainder is left rather than treating the silence as the
utterance boundary.

## R4: How to stop non-speech producing captions

**Decision**: Reject on transcript content, not on audio content. No new model,
no new dependency.

**Evidence — measured in this repository.** The fixture harness from feature 001
records what the recognizer actually produces for non-speech audio:

| Fixture | Transcript |
|---|---|
| `silence-5s` | `""` |
| `noise-fan-10s` | `""` |
| `noise-keyboard-10s` | `""` |
| `music-10s` | `"(soft music)"` |

Three of four produce nothing. The fourth produces a **sound-event annotation** —
the model correctly reporting that no speech occurred, in its own notation.

The bot currently filters exactly one literal, `[blank_audio]`
(`src/transcription/mod.rs`, `transcribe_and_write`). So `(soft music)` is
written into a caption file today as though someone had said it. **That is a
live false-caption bug**, recorded as a known finding in feature 001's
VERIFICATION.md and precisely what FR-009 targets.

**The fix**: reject a transcript whose normalized form is empty *or* consists
solely of bracketed or parenthesized annotations. The classifier already exists
and is already tested — `tests/support::is_sound_event_annotation`, written for
feature 001 — so this is a move into `src/`, not new logic.

US3's acceptance scenario 3 permits exactly this: a non-speech segment "is
rejected **before or after** transcription without producing a caption."
Rejecting after costs one transcription of audio nobody will read; rejecting
before would need R2's model. Given R1 already suppresses most non-speech by not
opening an utterance at all, the residue is small.

**Consequence**: SC-004 becomes verifiable **today**, against the fixtures that
already exist. It is the one criterion here that does not wait on T005.

**Alternatives considered**: a hardcoded list of known Whisper hallucinations
("Thanks for watching!", "Subtitles by..."). Rejected — it is a denylist against
an open set, it is language-specific, and it would silently eat real speech that
happened to match.

## R5: Audio is lost when the bot leaves a channel

**Decision**: A defect found while reading for FR-007, not a new requirement.

**Evidence**: `/leave` calls `manager.remove(guild_id)` (`src/main.rs:657`) and
tears the call down. Nothing flushes `AudioAggregator::buffers` first. Anything
buffered — up to a full chunk per speaker, and after this feature up to a full
utterance — is dropped silently.

`flush_stream` exists and is correct (`src/voice/mod.rs:337-352`); it is simply
never called on the leave path. `on_disconnect` does call it (line 214), so the
speaker-disconnect case is already handled; the channel-departure and shutdown
cases are not.

**Consequence**: FR-007 and SC-007 are fixing a real, current audio-loss bug, and
this feature makes it worse before it makes it better — longer utterances mean
more buffered audio to lose. The flush must land in the same change as the
segmentation.

## R6: Choosing the thresholds

**Decision**: 500 ms silence, 20 s maximum, 300 ms minimum. These are starting
points to be tuned against fixtures, not measured results — labelled as such.

| Parameter | Default | Reasoning |
|---|---|---|
| Silence threshold | 500 ms | Long enough to survive an intra-sentence breath (FR-003), short enough to keep SC-003's 1.5 s budget reachable. Twenty-five ticks at 20 ms. |
| Maximum utterance | 20 s | whisper.cpp processes a fixed 30 s window per request regardless of input length, so anything beyond 30 s is not merely wasteful but truncated. 20 s leaves headroom for the padding and for the split not landing exactly on the limit. |
| Minimum speech | 300 ms | Below roughly this, a segment is a click or a syllable fragment, and transcribing it invites a hallucinated word (FR-005). |

**On the 30-second window**: this is also what makes SC-005's second clause true.
Every request costs a 30 s window whatever it carries, so raising median
utterance length from ~3 s to ~5 s does not just improve context — it cuts the
number of padded requests per minute of audio by more than half. That benefit is
real and belongs to this feature; the concurrency to exploit it belongs to 002.

**Honest caveat**: these three numbers are the feature's whole behavior, and none
of them has been tuned against real speech, because there is none to tune
against (R0). They are defensible starting points. Calling them tuned would be
the kind of assertion Principle II exists to prevent.

## R7: Measuring segmentation without a live Discord call

**Decision**: Drive the segmenter directly with a synthetic tick stream. Keep it
free of songbird and of Discord.

**Rationale**: segmentation is a pure function of (speech-activity per tick,
samples per tick, thresholds) → utterance boundaries. Extracting it from
`AudioAggregator` into its own unit makes every FR here testable in
milliseconds, deterministically, with no model and no network:

- a pause shorter than the threshold does not split (FR-003)
- a pause longer than it does (FR-002)
- continuous speech splits at the maximum (FR-004)
- a short blip is dropped (FR-005)
- two SSRCs interleaved segment independently (FR-006)
- a gap from packet loss does not end an utterance early (edge case)
- the timestamp is the first sample's arrival, not the cut (FR-014)

**Consequence**: SC-003 becomes measurable as a property of the segmenter — the
delay from last speech tick to utterance close is the silence threshold, by
construction — rather than needing a stopwatch on a live call.

This is the same lesson feature 002 learned the hard way: `hammock` is a binary
crate with no library target, so anything that needs testing must be reachable
from a `#[cfg(test)]` module inside `src/`, not from `tests/`.

## Resolved unknowns

All resolved. Three items carried forward as **stated risks**:

1. Five of seven success criteria cannot be measured until 001's T005 lands (R0).
2. The three thresholds are reasoned defaults, not tuned values (R6).
3. Open-mic participants are handled by the maximum-length rule and content
   rejection rather than by real segmentation, and how common they are is
   unknown until FR-012's counter runs in production (R1).
