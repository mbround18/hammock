# Feature Specification: Speech-Aware Utterance Segmentation

**Feature Branch**: `003-vad-utterance-segmentation`

**Created**: 2026-09-08

**Status**: Draft

**Input**: User description: "Audio is cut into fixed three-second chunks regardless of speech content, so words are split mid-syllable and each chunk is transcribed with no context. Trailing audio waits three seconds of silence before flushing. Segment boundaries should follow speech, not a sample counter."

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Captions contain whole words and whole sentences (Priority: P1)

A participant speaks a normal sentence. The caption records that sentence as one
coherent line, rather than as two or three fragments with words severed at the
boundaries between them.

**Why this priority**: This is the largest single source of transcript error today.
The audio is currently cut at a fixed sample count with no regard for whether
someone is mid-word, and speech recognition quality collapses on fragments that
begin or end mid-syllable. Every downstream consumer of the transcript — the reader,
the summarizer — inherits the damage.

**Independent Test**: Replay a fixture recording of continuous speech and compare
the produced captions against a reference transcript, measuring word error rate and
counting boundary-severed words. Delivers value on its own with no other change.

**Acceptance Scenarios**:

1. **Given** a participant speaks a sentence lasting longer than the previous fixed
   chunk length, **When** it is transcribed, **Then** the sentence is not split at an
   arbitrary point inside a word.
2. **Given** a participant pauses briefly mid-sentence, **When** they continue,
   **Then** the pause does not by itself end the caption line if speech resumes
   promptly.
3. **Given** a participant finishes speaking, **When** the silence that follows is
   long enough to indicate they are done, **Then** the utterance is transcribed and
   the caption appears.

---

### User Story 2 - Captions appear promptly after someone stops talking (Priority: P2)

A participant says a short phrase and stops. The caption appears within about a
second, not after a multi-second wait for a fixed timer to expire.

**Why this priority**: Below P1 because a delayed correct caption is more useful than
a prompt wrong one, but the delay is significant and the fix falls out of the same
change. Today, a partial utterance waits for a full silence timeout after the last
audio before it is flushed.

**Independent Test**: Speak a short phrase, stop, and measure the interval between
the end of speech and the caption appearing.

**Acceptance Scenarios**:

1. **Given** a participant speaks a short phrase and stops, **When** the silence
   threshold elapses, **Then** the utterance is submitted for transcription without
   waiting for any additional fixed interval.
2. **Given** a participant is speaking continuously beyond the maximum utterance
   length, **When** the maximum is reached, **Then** the utterance is split at the
   quietest available point rather than being held indefinitely.
3. **Given** a participant leaves the channel mid-utterance, **When** they
   disconnect, **Then** the buffered audio is still transcribed and attributed.

---

### User Story 3 - Silence and background noise do not produce captions (Priority: P2)

A participant is connected with an open microphone in a noisy room but is not
speaking. No captions are produced for them — not empty ones, and not invented
sentences.

**Why this priority**: Equal in practical impact to Story 2. Speech recognition
models are known to emit fabricated text when given silence or noise, and the
current implementation filters only one specific literal marker. False captions are
worse than missing ones because they are indistinguishable from real speech to
anyone reading the transcript.

**Independent Test**: Replay a fixture containing silence, keyboard noise, and
background music with no speech, and confirm no caption lines are produced.

**Acceptance Scenarios**:

1. **Given** a participant's stream contains only silence, **When** it is processed,
   **Then** no caption is produced.
2. **Given** a segment is shorter than the minimum speech duration, **When** it is
   considered for transcription, **Then** it is not submitted.
3. **Given** a segment contains only non-speech audio, **When** it is evaluated,
   **Then** it is rejected before or after transcription without producing a caption.

---

### Edge Cases

- What happens when two participants speak simultaneously? Each participant's stream
  is segmented independently; one speaker's pauses must not affect another's
  boundaries.
- What happens when a participant speaks continuously for several minutes without
  pausing? A maximum utterance length must bound memory and latency, splitting at the
  least disruptive point available.
- What happens when a participant's audio arrives in bursts due to network jitter?
  Gaps caused by packet loss must not be mistaken for the speaker having finished.
- What happens to buffered audio when the bot is told to leave the channel? It must
  be flushed and transcribed before the pipeline is torn down.
- What happens when a participant toggles mute mid-utterance? The utterance must be
  closed and transcribed rather than held waiting for audio that will not arrive.
- What happens when the silence threshold is configured very low? Utterances
  fragment; the configuration must have a documented safe range.

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001**: Utterance boundaries MUST be determined by the presence and absence of
  speech, not by a fixed count of audio samples.
- **FR-002**: The system MUST close an utterance when a speaker has been silent for a
  configurable threshold.
- **FR-003**: The system MUST NOT close an utterance on brief intra-sentence pauses
  shorter than that threshold.
- **FR-004**: The system MUST enforce a configurable maximum utterance length, and
  when it is reached MUST split at the least disruptive point available rather than
  discarding or growing without bound.
- **FR-005**: The system MUST enforce a configurable minimum speech duration below
  which a segment is not submitted for transcription.
- **FR-006**: Each participant's audio MUST be segmented independently of every other
  participant's.
- **FR-007**: Buffered audio MUST be flushed and transcribed when a speaker
  disconnects, when the bot leaves the channel, and on shutdown.
- **FR-008**: The system MUST discard segments determined to contain no speech,
  without producing a caption.
- **FR-009**: The system MUST NOT emit captions consisting solely of the recognizer's
  non-speech markers or of empty text.
- **FR-010**: Silence threshold, maximum utterance length, and minimum speech
  duration MUST each be configurable, MUST have documented defaults, and MUST be
  validated at startup.
- **FR-011**: The system MUST expose the distribution of produced utterance lengths
  as a metric, so that misconfigured segmentation is detectable.
- **FR-012**: The system MUST count segments rejected as containing no speech, so
  that an over-aggressive threshold is detectable.
- **FR-013**: Speaker attribution for an utterance MUST be preserved across the whole
  utterance, including when it spans what were previously multiple chunks.
- **FR-014**: The capture timestamp recorded on a caption MUST correspond to when the
  speech began, not when transcription completed.

### Key Entities

- **Utterance**: A contiguous span of one participant's speech, bounded by silence or
  by the maximum length, and the unit submitted for transcription.
- **Silence threshold**: The duration of quiet that ends an utterance, distinguishing
  a pause within a sentence from the end of a turn.
- **Speech activity signal**: The per-participant, per-interval determination of
  whether the participant is currently speaking, from which boundaries are derived.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: Word error rate on the fixture set improves by at least 20% relative to
  fixed-interval segmentation.
- **SC-002**: Zero words in fixture transcripts are severed across caption
  boundaries, compared to the current implementation's rate.
- **SC-003**: Median delay between the end of speech and the appearance of the
  caption is under 1.5 seconds.
- **SC-004**: A fixture containing only silence and background noise produces zero
  caption lines.
- **SC-005**: Median produced utterance length increases to at least 5 seconds for
  conversational speech, reducing the number of separate transcription requests per
  minute of audio by at least 50%.
- **SC-006**: Speaker attribution accuracy on the fixture set is not degraded
  relative to the current implementation.
- **SC-007**: No audio is lost at channel departure or shutdown: every utterance
  present in a fixture appears in the output.

## Assumptions

- Per-participant speech activity is already available at fine granularity from the
  voice receive path at 20-millisecond resolution, and is the primary segmentation
  signal. A model-based voice activity detector is available in the transcription
  library and may be layered on top as a secondary filter during planning; whether it
  is needed is decided against measurements rather than assumed.
- Default silence threshold of roughly 500 milliseconds and default maximum utterance
  length of roughly 20 seconds are the assumed starting points, chosen because they
  are typical of conversational turn-taking and because the recognizer processes a
  30-second window regardless. Both are tuned against the fixture set during planning.
- Reducing the number of transcription requests per minute of audio is a deliberate
  secondary benefit: the recognizer pads every request to a fixed window, so longer
  segments materially reduce wasted compute. That benefit is claimed here but the
  throughput work itself belongs to feature 002.
- Overlapping segments with transcript stitching are out of scope. If measurements
  show boundary context loss remains significant, that becomes its own feature.
- This feature does not change decoding parameters or model selection; those belong
  to feature 004. The fixture comparison for this feature holds them constant.
- Fixture recordings with reference transcripts are required for every success
  criterion here. They are shared with features 001, 002, and 004.
