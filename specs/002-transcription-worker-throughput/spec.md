# Feature Specification: Transcription Worker Throughput and Load Shedding

**Feature Branch**: `002-transcription-worker-throughput`

**Created**: 2026-09-08

**Status**: Draft

**Input**: User description: "The transcription worker processes exactly one chunk at a time and allocates a fresh decoder state per chunk, so the accelerator idles between chunks and multiple speakers serialize. When the queue fills, submission blocks inside the voice event handler, stalling the receive path and losing audio for everyone."

## User Scenarios & Testing *(mandatory)*

### User Story 1 - A busy channel keeps up (Priority: P1)

Several participants are talking in a channel, sometimes over each other. Captions
for each of them continue to appear promptly, rather than one speaker's captions
arriving on time while everyone else falls progressively further behind.

**Why this priority**: This is the observable failure today. Because chunks are
processed strictly one at a time, a channel with several active speakers builds a
queue that never drains during the conversation, so caption lag grows without bound
until the channel goes quiet.

**Independent Test**: Replay a fixture recording containing several overlapping
speakers, and measure the delay between the end of each utterance and the appearance
of its caption. Delivers value on its own: the same hardware serves more speakers.

**Acceptance Scenarios**:

1. **Given** a channel with four simultaneously active speakers, **When** the
   conversation runs for ten minutes, **Then** caption delay at the end of the
   conversation is no worse than at the start.
2. **Given** several utterances complete at the same instant, **When** they are
   transcribed, **Then** they are processed concurrently rather than strictly in
   sequence.
3. **Given** the system is under no load, **When** a single participant speaks,
   **Then** caption delay is no worse than it is today.

---

### User Story 2 - Overload sheds work instead of losing audio (Priority: P1)

The transcription stage cannot keep up — the host is undersized, the model is too
large, or a burst of speech arrives at once. The bot drops the excess explicitly and
keeps receiving audio, rather than stalling the audio receive path and corrupting
every speaker's stream.

**Why this priority**: Equal priority because it is a correctness issue, not a
performance one. Today, submission waits on a bounded queue from inside the voice
event handler, so a full queue halts audio reception for the whole channel. The
symptom — audio loss for everyone, including speakers whose captions were fine — is
far worse than the cause.

**Independent Test**: Artificially constrain transcription throughput, drive
sustained speech through the bot, and confirm that the audio receive path continues
to run on schedule and that the discarded work is counted and logged.

**Acceptance Scenarios**:

1. **Given** the transcription queue is full, **When** another utterance is ready to
   submit, **Then** the audio receive path is not blocked.
2. **Given** work has been discarded due to overload, **When** an operator inspects
   metrics, **Then** the number of discarded utterances is available as a counter.
3. **Given** work has been discarded, **When** an operator inspects logs, **Then**
   the discard is recorded with the guild, channel, and speaker involved.
4. **Given** the overload subsides, **When** the queue drains, **Then** the system
   returns to normal operation without a restart.

---

### User Story 3 - Operator can size the worker pool (Priority: P3)

An operator running on unusual hardware — a very large GPU, a small shared CPU box —
adjusts how much transcription work runs concurrently to match what their hardware
can actually sustain.

**Why this priority**: Lower than P1 because a sensible automatic default serves
nearly everyone. It matters for the tail of deployments where the default is
badly wrong in either direction.

**Independent Test**: Set the concurrency configuration to a known value, start the
bot, and confirm from startup output that the configured number of workers was
created and that concurrent in-flight work does not exceed it.

**Acceptance Scenarios**:

1. **Given** no concurrency configuration is supplied, **When** the bot starts,
   **Then** it selects a default appropriate to the active compute backend and
   reports it.
2. **Given** a concurrency value is configured, **When** the bot starts, **Then**
   that many workers are used.
3. **Given** an invalid concurrency value is configured, **When** the bot starts,
   **Then** it reports the invalid value and the accepted range rather than failing
   later or silently substituting a value.

---

### Edge Cases

- What happens when a worker fails while transcribing? The failure must be counted
  and logged, the worker must recover for subsequent work, and the pool must not
  shrink permanently.
- What happens when overload is sustained for a long period? Discards must remain
  bounded and counted; the queue must not grow without limit as an alternative to
  dropping.
- What happens when the bot is asked to shut down with work still queued? In-flight
  work should be allowed a bounded time to finish; work still queued after that is
  discarded and counted.
- What happens if concurrency is configured higher than the accelerator can hold in
  memory? Initialization must fail with a clear message naming the constraint, or
  reduce concurrency and report having done so — not fail opaquely at first use.
- What happens when one speaker produces a very long utterance? It must not prevent
  other speakers' shorter utterances from being transcribed concurrently.

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001**: The system MUST transcribe multiple utterances concurrently, up to a
  bounded limit.
- **FR-002**: The system MUST reuse decoder state across utterances rather than
  allocating and releasing it for every utterance.
- **FR-003**: Reused decoder state MUST NOT allow content from one utterance to leak
  into the transcript of an unrelated utterance or speaker.
- **FR-004**: Submitting an utterance for transcription MUST NOT block the audio
  receive path under any queue condition.
- **FR-005**: When the transcription queue is full, the system MUST discard work
  according to a defined, documented policy rather than waiting.
- **FR-006**: Every discarded utterance MUST increment a counter exposed in metrics.
- **FR-007**: Every discarded utterance MUST be logged with the guild, channel, and
  speaker it belonged to.
- **FR-008**: A transcription failure in one worker MUST NOT terminate the worker
  pool or prevent subsequent utterances from being processed.
- **FR-009**: The system MUST expose the current queue depth and the number of
  in-flight transcriptions as metrics.
- **FR-010**: The system MUST expose transcription duration as a metric, so that
  throughput regressions are detectable.
- **FR-011**: The concurrency limit MUST be configurable, MUST have a default
  appropriate to the active compute backend, and MUST be validated at startup.
- **FR-012**: The system MUST report its effective concurrency limit at startup.
- **FR-013**: On shutdown, the system MUST allow in-flight transcriptions a bounded
  period to complete before exiting, and MUST report anything discarded.

### Key Entities

- **Transcription job**: One speaker's segment of audio awaiting transcription,
  carrying its speaker attribution, originating channel, and capture time.
- **Worker**: A unit of transcription concurrency owning reusable decoder state and
  processing one job at a time.
- **Queue**: The bounded buffer of jobs between the audio receive path and the worker
  pool, whose depth is the primary overload signal.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: With four concurrently active speakers, the delay between the end of an
  utterance and the appearance of its caption does not grow over a ten-minute
  conversation.
- **SC-002**: Sustained transcription throughput on identical hardware improves by at
  least 2x relative to the current implementation, measured on a fixture recording
  with multiple speakers.
- **SC-003**: Per-utterance transcription time is reduced by the elimination of
  repeated decoder-state setup, measured as a reduction in median transcription
  duration for short utterances on identical hardware.
- **SC-004**: Under deliberately induced overload, the audio receive path continues
  to run on schedule, and no audio is lost for speakers whose work was not
  discarded.
- **SC-005**: 100% of discarded utterances are reflected in both a metric counter and
  a log line identifying the speaker.
- **SC-006**: Transcript content for the fixture set is unchanged relative to the
  current implementation, confirming this feature alters throughput only.
- **SC-007**: An operator can determine, from metrics alone, whether the system is
  currently keeping up with demand.

## Assumptions

- This feature changes throughput and overload behavior only. Segmentation of audio
  into utterances is unchanged here and is addressed separately; decoding parameters
  and model selection are likewise addressed separately.
- Discarding the newest work when the queue is full is the assumed default policy,
  on the grounds that older queued utterances are closer to being useful and that
  dropping the oldest would produce transcripts with confusing gaps. The policy is
  documented and revisited during planning if evidence favors the alternative.
- A concurrency default derived from available parallelism on CPU, and a small fixed
  default on GPU, is assumed adequate; the correct GPU default is bounded by device
  memory per concurrent decoder state.
- Feature 001 (GPU-accelerated builds) is not a hard prerequisite — these changes
  benefit the CPU path too — but the concurrency defaults for the GPU path cannot be
  validated until 001 lands.
- The fixture recordings referenced in the success criteria are shared with feature
  001 and reused here.
