# Tasks: Speech-Aware Utterance Segmentation

**Input**: Design documents from `/specs/003-vad-utterance-segmentation/`

**Prerequisites**: [plan.md](./plan.md), [spec.md](./spec.md), [research.md](./research.md), [data-model.md](./data-model.md), [contracts/telemetry.md](./contracts/telemetry.md), [contracts/configuration.md](./contracts/configuration.md), [quickstart.md](./quickstart.md)

**⚠️ Read before starting**: Four success criteria (SC-001, SC-002, SC-005, SC-006) **cannot be measured** — they need conversational speech fixtures with reference transcripts, and feature 001's T005 is still blocked. This feature deliberately changes transcript output, which Constitution Principle II says must be measured rather than asserted. T041 unblocks it and is the highest-value task in this list. Everything else can proceed without it; only the accuracy claim cannot.

**Status**: Implemented. 42 of 44 tasks complete. **T041 and T043 are not done**: SC-001, SC-002, SC-005 and SC-006 remain unmeasured for want of speech fixtures. See [VERIFICATION.md](./VERIFICATION.md).

**Tests**: TDD was not requested. But the segmenter is extracted into its own module *specifically* so it can be unit-tested (research R7), and those tests are the only verification most functional requirements here will ever get — so they appear as implementation tasks, not optional extras.

**Organization**: Grouped by user story. US1 is P1; US2 and US3 are both P2.

## Format: `[ID] [P?] [Story] Description`

- **[P]**: Can run in parallel (different files, no dependencies)
- **[Story]**: US1, US2, US3 — maps to the spec's user stories

## Path Conventions

Single Rust project. `hammock` is a **binary crate with no library target**, so tests that need internal types live in `#[cfg(test)]` modules inside `src/`, not in `tests/` — the constraint feature 002 discovered. `tests/` holds only what needs a Whisper model.

---

## Phase 1: Setup

**Purpose**: Record what the current behavior is, since three criteria are comparisons against it.

- [X] T001 Record the pre-feature segmentation baseline in `specs/003-vad-utterance-segmentation/baseline.md`: fixed 3 s cuts at `CAPTION_CHUNK_SECS`, a 3 s silence flush from `src/main.rs:543`, `started_at` set at dispatch rather than at first sample, no minimum speech duration, and the resulting transcription-requests-per-minute figure — SC-005 is a comparison against that last number
- [X] T002 [P] Record in `specs/003-vad-utterance-segmentation/baseline.md` the current non-speech transcripts measured by the existing harness (`silence-5s`, `noise-fan-10s`, `noise-keyboard-10s` produce `""`; `music-10s` produces `"(soft music)"`), which is the SC-004 "before" and the evidence that the false-caption bug is real rather than theoretical

---

## Phase 2: Foundational (Blocking Prerequisites)

**Purpose**: The configuration, the metrics, and the module every story is implemented inside.

**⚠️ CRITICAL**: All three stories are implemented in `src/voice/segmenter.rs`. It has to exist first.

### Configuration

- [X] T003 Add `UTTERANCE_SILENCE_MS`, `UTTERANCE_MAX_SECS` and `UTTERANCE_MIN_SPEECH_MS` to `BotConfig` in `src/config.rs` with the defaults from [contracts/configuration.md](./contracts/configuration.md) (500 ms, 20 s, 300 ms)
- [X] T004 Validate all three at startup in `src/config.rs`, each error naming the offending value **and** the accepted range, following the `parse_concurrency` pattern feature 002 established
- [X] T005 Add the cross-field validation rule in `src/config.rs`: `UTTERANCE_MIN_SPEECH_MS` must be below `UTTERANCE_MAX_SECS`, or nothing would ever be transcribed — two individually valid values combining into a silently dead bot is exactly the "failing later at use" Principle IV forbids, and a per-variable check cannot catch it
- [X] T006 Warn at startup in `src/config.rs` when `UTTERANCE_SILENCE_MS` is below 200 ms, naming the consequence (utterances fragment mid-sentence, word error rate rises) rather than silently accepting a setting that undoes this feature
- [X] T007 Report the effective segmentation settings at startup in `src/main.rs`, beside the backend and concurrency lines from features 001 and 002

### Metrics

- [X] T008 Add `utterance_length_ms` to `AppMetrics` and `MetricsSnapshot` in `src/telemetry/metrics.rs` (FR-011), reusing the `DurationWindow` shape built for feature 002 — lifetime `count`/`total_ms` plus windowed percentiles
- [X] T009 Add `total_segments_rejected_as_non_speech` to `src/telemetry/metrics.rs` (FR-012), kept strictly distinct from feature 002's `total_utterances_discarded` — one means work was lost and the bot is failing, the other means the bot is working correctly, and merging them would be actively misleading
- [X] T010 Expose both on `/k8s/metrics` and add them to the `Metrics` Swagger schema in `src/telemetry/server.rs`, including the sentence distinguishing the two counters per [contracts/telemetry.md](./contracts/telemetry.md)

### Segmenter module

- [X] T011 Create `src/voice/segmenter.rs` with the `Utterance`, `Segmenter` and `SegmentationConfig` types from [data-model.md](./data-model.md), and register the module in `src/voice/mod.rs`
- [X] T012 Define the single entry point `Segmenter::on_tick(speaking: bool, samples: &[i16], now: Instant) -> Option<Utterance>` in `src/voice/segmenter.rs` — pure, no I/O, no awaiting, because it runs on the 20 ms voice tick where Constitution Principle I forbids all three

**Checkpoint**: Configuration and metrics exist; the segmenter has a shape and an entry point. All three stories can now be implemented against it.

---

## Phase 3: User Story 1 — Captions contain whole words and whole sentences (Priority: P1) 🎯 MVP

**Goal**: Utterance boundaries follow speech, so words stop being severed mid-syllable.

**Independent Test**: *As specified*, replay a speech fixture and compare WER against a reference — **blocked, see T041**. What is testable today is the mechanism: the segmenter's boundary rules, driven by a synthetic tick stream.

- [X] T013 [US1] Implement open-on-speech in `src/voice/segmenter.rs`: the first tick carrying speech opens an utterance and records `started_at` as that moment (FR-001, FR-014)
- [X] T014 [US1] Implement extend-on-speech in `src/voice/segmenter.rs`: subsequent speech ticks append samples and advance `last_speech`
- [X] T015 [US1] Implement close-on-silence in `src/voice/segmenter.rs`: emit the utterance once `now - last_speech >= silence_threshold` (FR-002)
- [X] T016 [US1] Implement hold-through-short-pause in `src/voice/segmenter.rs`: a quiet tick below the threshold keeps the utterance open **and still appends its samples** (FR-003) — this is the single rule that stops words being severed, because a pause inside a sentence is part of the sentence
- [X] T017 [US1] Give each SSRC its own `Segmenter` instance in `src/voice/mod.rs` so one speaker's pauses cannot affect another's boundaries (FR-006) — per-participant state makes this structural rather than something to remember
- [X] T018 [US1] Replace the fixed-size chunking loop in `src/voice/mod.rs` (`consume_samples`, the `while entry.samples.len() >= self.chunk_samples` block) with a call to `Segmenter::on_tick`, and delete `AudioBuffer` and the `chunk_samples` field
- [X] T019 [US1] Drive the segmenter from both halves of the tick in `src/voice/mod.rs` `on_voice_tick`: `tick.speaking` entries as speech, `tick.silent` entries as quiet — the silent set is currently consulted only for an elapsed-time flush, and it is the actual boundary signal (research R1)
- [X] T020 [US1] Fix the caption timestamp in `src/voice/mod.rs` and `src/transcription/mod.rs` so `started_at` is the utterance's first sample rather than `Utc::now()` at dispatch (FR-014) — already wrong by up to a chunk length, and longer utterances make it worse
- [X] T021 [US1] Carry speaker identity from utterance open through to the job unchanged in `src/voice/segmenter.rs` and `src/voice/mod.rs` (FR-013), so attribution survives an utterance that spans what used to be several chunks
- [X] T022 [US1] Add boundary tests to `src/voice/segmenter.rs` driven by a synthetic tick stream (research R7): a pause below the threshold does not split, a pause at or beyond it does, quiet samples inside an utterance are retained, and `started_at` is the first sample's arrival
- [X] T023 [US1] Add a packet-loss test to `src/voice/segmenter.rs`: a single missing tick must not end an utterance early (spec Edge Cases) — network jitter is not someone finishing their sentence
- [X] T024 [P] [US1] Add a two-speaker independence test to `src/voice/segmenter.rs`: interleaved SSRCs produce independent boundaries (FR-006)

**Checkpoint**: Boundaries follow speech. The mechanism is verified; the transcript improvement is not, and cannot be until T041.

---

## Phase 4: User Story 2 — Captions appear promptly (Priority: P2)

**Goal**: A caption appears about a second after someone stops, not after a three-second timer.

**Independent Test**: Speak a short phrase, stop, and measure the interval to the caption appearing. Also measurable as a segmenter property: the close delay is the silence threshold by construction.

- [X] T025 [US2] Remove the 3 s silence flush in `src/main.rs:543` (`silence_flush: state.chunk_duration`) and the `silence_flush` field from `src/voice/mod.rs` — this single line is the delay the spec complains about, and the silence threshold replaces it
- [X] T026 [US2] Implement the maximum-length split in `src/voice/segmenter.rs`: emit and reopen when samples reach `max_utterance` (FR-004), splitting at the most recent tick without speech activity and falling back to the hard limit when there has been none — searching decoded audio for a quiet sample would be signal processing on the tick path, which Principle I rules out
- [X] T027 [US2] Implement the minimum-speech drop in `src/voice/segmenter.rs`: an utterance whose speech ticks total less than `min_speech` is discarded, counting as neither an utterance nor a non-speech rejection (FR-005) — nothing was rejected, nothing happened
- [X] T028 [US2] Implement `Segmenter::flush()` in `src/voice/segmenter.rs`, emitting any open utterance regardless of thresholds, for the three cases FR-007 names
- [X] T029 [US2] Flush all buffers before tearing down the call in the `/leave` command in `src/main.rs` — `manager.remove(guild_id)` currently drops everything buffered, and `flush_stream` exists and is correct but is never called on this path (research R5, FR-007, SC-007). **This is a live audio-loss bug**, and this feature makes it worse before better because utterances get longer
- [X] T030 [US2] Flush all buffers on shutdown in `src/main.rs`, before feature 002's `transcription::drain` — draining the transcription queue while audio still sits unsegmented in the aggregator would drain the wrong thing first
- [X] T031 [US2] Confirm the existing `on_disconnect` flush in `src/voice/mod.rs:214` still fires with the segmenter in place, covering the mid-utterance departure case (US2 acceptance scenario 3) and the mute-toggle edge case
- [X] T032 [US2] Add max-length, min-speech and flush tests to `src/voice/segmenter.rs`, including that a flushed partial utterance is emitted even when below the silence threshold
- [X] T033 [US2] Assert in `src/voice/segmenter.rs` that the close delay equals the silence threshold, and that the default leaves room inside SC-003's 1.5 s budget — SC-003 is a property of the segmenter, not something needing a stopwatch on a live call

**Checkpoint**: Captions arrive promptly, long turns are bounded, and no audio is lost at departure.

---

## Phase 5: User Story 3 — Silence and noise produce no captions (Priority: P2)

**Goal**: No captions from silence, noise or music — neither empty ones nor invented ones.

**Independent Test**: Replay the non-speech fixtures and confirm zero caption lines. **Runs today**, and currently fails.

- [X] T034 [US3] Move `is_sound_event_annotation` from `tests/support/mod.rs` into `src/transcription/mod.rs` so production can use it, keeping the tests that already cover it — this is a move, not new logic; it was written and tested for feature 001
- [X] T035 [US3] Replace the single `[blank_audio]` literal check in `src/transcription/mod.rs` `transcribe_and_write` with rejection of any transcript that normalizes to empty **or** consists solely of bracketed or parenthesized annotations (FR-008, FR-009) — `(soft music)` is written into caption files today as though someone said it (research R4)
- [X] T036 [US3] Increment `total_segments_rejected_as_non_speech` on every rejection in `src/transcription/mod.rs` (FR-012), and log at debug with guild, channel and speaker — a rejection nobody counted is invisible, and an over-aggressive threshold would then look like a quiet channel
- [X] T037 [US3] Add a test to `src/transcription/mod.rs` asserting that `""`, `"[blank_audio]"`, `"(soft music)"` and `"[APPLAUSE] (laughter)"` are all rejected while `"hello there"` and `"(music) thanks for watching"` are not — the last case matters most: an annotation next to real words is real speech, and eating it would be worse than the bug being fixed
- [X] T038 [US3] Run the existing fixture harness in `tests/transcription_fixtures.rs` via `cargo test --release --test transcription_fixtures` and confirm all four non-speech fixtures now produce zero captions (SC-004)

**Checkpoint**: All three stories functional. SC-004 verified against real fixtures.

---

## Phase 6: Polish & Cross-Cutting Concerns

- [X] T039 [P] Document all three tunables in `.env.sample`: effect of raising and lowering each, safe ranges, and `utterance_length_ms` on `/k8s/metrics` as the way to tell whether the values are right
- [X] T040 [P] Document segmentation in `docs/README.md`, including the metrics reading table from [contracts/telemetry.md](./contracts/telemetry.md) and the distinction between the two counters
- [ ] T041 **Unblock feature 001's T005**: add 3-5 public-domain or CC0 conversational speech clips to `tests/fixtures/audio/` with reference transcripts produced **by listening**, following the procedure in `tests/fixtures/audio/README.md`. **This is the highest-value task in this list.** Without it SC-001, SC-002, SC-005 and SC-006 are unmeasurable, and this feature's entire claim — that transcripts got better — rests on nothing. Requires a human: deriving references from a recognizer would make the measurement circular
- [X] T042 Decide and implement the fate of `CAPTION_CHUNK_SECS` in `src/config.rs` and `.env.sample`: it currently sets both chunk size and, via `src/main.rs:543`, the silence timeout, and this feature takes both jobs away. Either remove it or retain it with a startup warning that it no longer affects segmentation — retaining it silently is the one unacceptable option, because an operator who tuned it would see it stop working with no explanation
- [ ] T043 Measure SC-001, SC-002, SC-005 and SC-006 against the speech fixtures from T041, holding decoding parameters and model constant so any change is attributable to segmentation alone, and record the before/after comparison in `specs/003-vad-utterance-segmentation/VERIFICATION.md` per Constitution Principle II. **Blocked on T041**
- [X] T044 Write `specs/003-vad-utterance-segmentation/VERIFICATION.md` recording what was verified and what was not, following `specs/002-transcription-worker-throughput/VERIFICATION.md` — if T041 did not happen, it must say plainly that a green test run proves the segmenter obeys its rules and says nothing about whether transcripts improved

---

## Dependencies & Execution Order

### Phase Dependencies

- **Setup (Phase 1)**: no dependencies
- **Foundational (Phase 2)**: depends on Setup — **blocks all three stories**
- **US1 (Phase 3)**: depends on Phase 2
- **US2 (Phase 4)**: depends on US1 — the max-length split and flush operate on the utterance US1 introduces
- **US3 (Phase 5)**: depends only on Phase 2. **Fully independent of US1 and US2** — it filters transcripts, not audio
- **Polish (Phase 6)**: depends on all stories, except T041 which depends on nothing and blocks T043

### US3 can be done first, and possibly should be

US3 touches only `src/transcription/mod.rs` and fixes a live false-caption bug using a classifier that already exists and is already tested. It shares no files with US1 or US2. If the segmentation work stalls, US3 still ships — and it is the only story here whose success criterion (SC-004) can be verified today.

### Within Phase 2

- T003 → T004 → T005 → T006 are sequential (all edit `src/config.rs`)
- T007 depends on T003
- T008 → T009 → T010 are sequential (metrics then endpoint; the endpoint cannot expose what the snapshot does not carry)
- T011 → T012 sequential
- The three tracks — config, metrics, segmenter module — are parallel with each other

### Within US1

- T013 → T014 → T015 → T016 are sequential (all build the same state machine in `segmenter.rs`)
- T017 → T018 → T019 are sequential (all rewire `src/voice/mod.rs`)
- T020, T021 depend on T018
- T022, T023 depend on T016; T024 is parallel with them

### Within US2

- T025 depends on T019 (the old flush cannot go until the new boundary signal is in)
- T026, T027, T028 are sequential (all in `segmenter.rs`)
- T029, T030 depend on T028; T031 is verification
- T032, T033 depend on T028

### Within US3

- T034 → T035 → T036 sequential (all in `src/transcription/mod.rs`)
- T037 depends on T035
- T038 depends on T036

### Parallel Opportunities

- T002 in Setup
- The three Phase 2 tracks in full
- T024 within US1
- T039, T040 in Polish
- **T041 can start immediately** and in parallel with everything — it is a sourcing errand, not code

---

## Parallel Example: Phase 2

```bash
# Track A — configuration
Task: "Add the three segmentation tunables to src/config.rs"

# Track B — metrics
Task: "Add utterance_length_ms and the rejection counter to src/telemetry/metrics.rs"

# Track C — the module every story is built in
Task: "Create src/voice/segmenter.rs with Utterance, Segmenter, SegmentationConfig"
```

---

## Implementation Strategy

### MVP scope

**Phase 1 + Phase 2 + Phase 3 (US1).** That delivers speech-shaped boundaries,
which is the feature. US2 and US3 are both P2 and both build on or beside it.

### But consider shipping US3 first

US3 is small, independent, verifiable today, and fixes a bug that is writing
false text into caption files right now. It does not need the segmenter. If you
want something demonstrably correct in hand before the larger change, it is US3.

### Sequencing note

**T041 is the task that determines whether this feature can be believed.** Four
of seven success criteria — including SC-001, the headline claim that word error
rate improves by 20% — are unmeasurable without it. Everything else in this list
can be completed, tested and shipped, and the result will still be a feature
whose central benefit has never been observed.

It is also the easiest task to skip, because nothing fails without it. That is
precisely why it is called out here rather than left in the verification step.

### What a green CI run does and does not prove

It proves the segmenter obeys its rules, that no audio is lost at departure, and
that non-speech no longer produces captions. It does **not** prove transcripts
improved. Do not report SC-001 as met on the strength of it.
