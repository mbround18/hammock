# Feature Specification: Transcription Decoding Quality

**Feature Branch**: `004-transcription-decoding-quality`

**Created**: 2026-09-08

**Status**: Draft

**Input**: User description: "Decoding runs at its least accurate settings: smallest model, single-candidate greedy sampling, no fallback or confidence thresholds, no context carried between segments, and language re-detected on every fragment. Raise transcript accuracy now that there is GPU headroom to pay for it."

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Transcripts are accurate enough to read instead of the audio (Priority: P1)

A participant reads back the caption log for a conversation they missed and can
follow what was said, including who said it, without needing to listen to a
recording.

**Why this priority**: This is the purpose of the product. The current
configuration — the smallest available model with single-candidate greedy decoding —
is the least accurate setting the system can run in, chosen when only CPU was
available. With a working accelerator there is headroom to spend on accuracy, and
spending it is the point of having done the GPU work at all.

**Independent Test**: Transcribe the fixture set with the new configuration and
compare word error rate against the reference transcripts and against the current
baseline. Delivers value on its own.

**Acceptance Scenarios**:

1. **Given** a fixture recording of conversational speech, **When** it is
   transcribed, **Then** word error rate is materially lower than the current
   baseline on identical audio.
2. **Given** a participant uses domain vocabulary — game names, server jargon,
   member names — **When** it is transcribed, **Then** those terms are recognized
   more often than without vocabulary hinting.
3. **Given** an accelerated deployment, **When** accuracy settings are raised,
   **Then** the system still keeps up with real-time conversation.

---

### User Story 2 - The bot does not invent speech (Priority: P1)

Participants read a transcript and can trust that every line corresponds to
something somebody actually said. The transcript does not contain plausible-sounding
sentences that were never spoken.

**Why this priority**: Equal to Story 1 because fabricated text is worse than absent
text. The recognizer is known to emit invented output — repeated phrases, stock
sentences — when given low-confidence or non-speech audio, and the current
configuration sets none of the available confidence thresholds or fallback
behaviors. A reader cannot distinguish a fabricated line from a real one.

**Independent Test**: Transcribe fixtures containing silence, noise, music, and
non-speech vocalizations, and count caption lines produced. Additionally, count
degenerate repetition in conversational fixtures.

**Acceptance Scenarios**:

1. **Given** a low-confidence segment, **When** it is decoded, **Then** the system
   retries with progressively different decoding settings before accepting or
   rejecting the result.
2. **Given** a decoded result that remains low-confidence after retries, **When** it
   is evaluated, **Then** it is discarded rather than written as a caption.
3. **Given** a segment that produces degenerate repeated output, **When** it is
   evaluated, **Then** it is rejected.
4. **Given** any rejection above, **When** an operator inspects metrics, **Then** the
   rejection is counted by reason.

---

### User Story 3 - Conversations stay coherent across turns (Priority: P2)

A participant refers back to something said moments earlier using a name or term
introduced then. The transcript spells it consistently, rather than guessing anew
each time.

**Why this priority**: Below P1 because the transcript is usable without it, but it
is what separates a serviceable transcript from a good one. Each segment is
currently decoded with no knowledge of what preceded it, so proper nouns and jargon
are re-guessed from scratch on every utterance.

**Independent Test**: Transcribe a fixture in which a distinctive proper noun recurs
across several utterances, and measure consistency of its rendering.

**Acceptance Scenarios**:

1. **Given** a term was recognized in a participant's previous utterance, **When**
   their next utterance is decoded, **Then** the previous text is available as
   context to the decoder.
2. **Given** an operator supplies a vocabulary of expected terms, **When** speech is
   decoded, **Then** those terms are biased toward.
3. **Given** context from a previous utterance would exceed the decoder's context
   budget, **When** it is supplied, **Then** it is truncated rather than causing an
   error.
4. **Given** a participant has been silent for an extended period, **When** they
   speak again, **Then** stale context is not carried forward.

---

### User Story 4 - Operators can trade accuracy against cost (Priority: P3)

An operator on modest hardware chooses a faster, less accurate configuration; an
operator with a large GPU chooses a slower, more accurate one. Neither has to edit
code or rebuild.

**Why this priority**: Lowest because a good default serves most deployments. It
matters because the range of hardware running this bot is wide, and a default tuned
for a GPU would make the CPU path unusable if it could not be turned down.

**Independent Test**: Run the bot at each documented quality preset and confirm the
documented accuracy and speed characteristics hold.

**Acceptance Scenarios**:

1. **Given** no quality configuration is supplied, **When** the bot starts, **Then**
   it selects a default appropriate to the active compute backend and reports it.
2. **Given** a model that is not present locally, **When** the bot starts, **Then**
   it reports which model is missing and where it is expected, rather than failing
   obscurely.
3. **Given** a configuration too demanding for the hardware, **When** the system
   cannot keep up, **Then** the shortfall is visible in metrics.

---

### Edge Cases

- What happens when a participant speaks a language other than the configured one?
  The behavior must be defined and documented rather than silently producing
  nonsense.
- What happens when multiple languages are spoken in one channel? Out of scope for
  this feature, but the limitation must be documented rather than implied.
- What happens when retries with alternative decoding settings exhaust the available
  time budget? The system must bound total effort per utterance and prefer shedding
  the utterance to falling behind real time.
- What happens when the supplied vocabulary is very large? It must be bounded and
  truncated predictably, with the limit documented.
- What happens when a larger model does not fit in available memory? Startup must
  fail with a message naming the model and the shortfall, or fall back and report
  having done so.
- What happens when carried-over context is itself wrong? It must not compound
  indefinitely; context is bounded and reset on silence.

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001**: The default transcription model MUST be upgraded from the smallest
  available model to one that measurably improves accuracy on the fixture set while
  sustaining real-time throughput on an accelerated deployment.
- **FR-002**: The model MUST remain configurable, and a smaller model MUST remain a
  supported choice for constrained deployments.
- **FR-003**: The system MUST use a multi-candidate decoding strategy by default when
  the active compute backend can sustain it.
- **FR-004**: The system MUST apply confidence-based fallback: a segment decoded with
  low confidence MUST be retried with alternative decoding settings before its result
  is accepted.
- **FR-005**: The system MUST reject decoded results that remain below configured
  confidence thresholds after fallback, rather than writing them as captions.
- **FR-006**: The system MUST reject decoded results exhibiting degenerate
  repetition.
- **FR-007**: The system MUST suppress the recognizer's non-speech and blank output
  rather than filtering only specific literal strings after the fact.
- **FR-008**: The system MUST provide the preceding transcript text for the same
  speaker as decoding context, bounded in length and reset after a configurable
  period of that speaker's silence.
- **FR-009**: The system MUST accept an operator-supplied vocabulary of expected
  terms and bias decoding toward it, with a documented maximum size and predictable
  truncation.
- **FR-010**: The spoken language MUST be configurable and, when configured, MUST be
  applied rather than detected per segment.
- **FR-011**: When no language is configured, the system MUST determine it once and
  reuse that determination, rather than re-detecting on every segment.
- **FR-012**: The system MUST count rejected results by reason — low confidence,
  degenerate repetition, non-speech — as separate metrics.
- **FR-013**: The system MUST bound total decoding effort per utterance so that
  fallback retries cannot cause the pipeline to fall behind real time indefinitely.
- **FR-014**: All decoding parameters introduced MUST be configurable with documented
  defaults, validated at startup.
- **FR-015**: The system MUST report its effective model, language, and decoding
  strategy at startup.
- **FR-016**: When a configured model is absent or unloadable, the system MUST report
  which model, where it was expected, and how to obtain it.

### Key Entities

- **Quality profile**: The combined selection of model, decoding strategy, and
  thresholds that determines the accuracy-versus-cost operating point, with defaults
  derived from the active compute backend.
- **Decoding context**: Bounded prior text for a speaker, plus operator-supplied
  vocabulary, provided to bias recognition toward what has already been established.
- **Rejection reason**: The classified cause for discarding a decoded result, counted
  separately so that over-aggressive thresholds are distinguishable from genuine
  non-speech.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: Word error rate on the conversational fixture set improves by at least
  30% relative to the current baseline.
- **SC-002**: Recognition of domain vocabulary terms present in the supplied
  vocabulary improves by at least 50% relative to no vocabulary hinting.
- **SC-003**: Fabricated caption lines on silence, noise, and music fixtures are
  reduced to zero.
- **SC-004**: Degenerate repeated output is absent from all fixture transcripts.
- **SC-005**: A recurring proper noun in the fixture set is rendered consistently in
  at least 90% of its occurrences.
- **SC-006**: On an accelerated deployment at the default quality profile, the system
  sustains real-time transcription for at least four concurrent speakers.
- **SC-007**: On a CPU-only deployment at its default quality profile, transcription
  remains at least as fast as the current implementation.
- **SC-008**: Every rejected result is attributable to a specific counted reason.
- **SC-009**: An operator can identify the active model, language, and decoding
  strategy from startup output alone.

## Assumptions

- This feature is sequenced after features 001, 002, and 003. Higher accuracy
  settings cost compute that only the GPU path and the throughput work make
  affordable, and the accuracy gains from better decoding are difficult to measure
  while segmentation is still cutting words in half.
- A distilled or turbo variant of a large model is the assumed default for the
  accelerated path, on the basis that such variants offer near-large accuracy at a
  fraction of the cost. The specific model is selected during planning by measuring
  candidates against the fixture set, not assumed here.
- The CPU default may remain a smaller model than the GPU default. Defaults that
  differ by compute backend are acceptable and expected, provided both are reported
  at startup.
- Models are obtained and stored as they are today; this feature does not change
  model distribution or the existing download mechanism, only which model is chosen
  by default.
- Single-language channels are the assumed case. Multilingual channels are
  explicitly out of scope and the limitation is documented rather than addressed.
- Fixture recordings with reference transcripts, plus dedicated silence, noise, and
  music fixtures, are prerequisites for every success criterion here. They are
  shared with features 001, 002, and 003; this feature has the most demanding
  requirements of them and may need to extend the set.
