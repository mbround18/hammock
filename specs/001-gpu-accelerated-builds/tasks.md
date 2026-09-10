# Tasks: GPU-Accelerated Transcription Builds

**Input**: Design documents from `/specs/001-gpu-accelerated-builds/`

**Prerequisites**: [plan.md](./plan.md), [spec.md](./spec.md), [research.md](./research.md), [data-model.md](./data-model.md), [contracts/telemetry.md](./contracts/telemetry.md), [quickstart.md](./quickstart.md)

**Tests**: TDD was not requested, so there are no failing-test-first tasks. The fixture set and its harness ARE deliverables of this feature (spec Assumptions), so they appear as implementation tasks — without them, Constitution Principle II cannot be satisfied by this or any later feature.

**Organization**: Grouped by user story. US1 and US2 are both P1 in the spec; US2 is sequenced second because it is largely verification of what US1 must not break.

## Format: `[ID] [P?] [Story] Description`

- **[P]**: Can run in parallel (different files, no dependencies)
- **[Story]**: US1, US2, US3 — maps to the spec's user stories

## Path Conventions

Single Rust project. Sources at `src/`, fixtures and harness at `tests/`, image definition at `Dockerfile`, CI at `.github/workflows/paws.yml`.

---

## Phase 1: Setup

**Purpose**: Capture the "before" state, since two success criteria are comparisons against it.

- [X] T001 Record the current CPU image baseline (size, layer count, digest) for the SC-006 5% comparison in `specs/001-gpu-accelerated-builds/baseline.md`; the merged `main` build is `ghcr.io/mbround18/hammock:sha-4a2bdae` at 3343 MB across 12 layers
- [X] T002 [P] Create `tests/fixtures/audio/` with a `README.md` recording, per fixture, its provenance, licence, speaker count, and duration
- [X] T003 [P] Add `tests/fixtures/` to `.dockerignore` so fixture audio never enters the image build context

---

## Phase 2: Foundational (Blocking Prerequisites)

**Purpose**: The fixture corpus every accuracy claim depends on, and the backend-resolution core all three stories report against.

**⚠️ CRITICAL**: No story can be *verified* without Phase 2, and US1/US3 cannot be implemented without the backend type.

### Fixture corpus and harness

- [X] T004 Write `tests/fixtures/generate.sh` producing the non-speech fixtures (pure silence, keyboard/fan noise, background music) as 16 kHz mono WAV, committed alongside their generated output
- [ ] T005 Add 3-5 public-domain or CC0 conversational speech clips (10-30 s each) to `tests/fixtures/audio/`, each with a sibling `.txt` reference transcript verified by listening
  - **BLOCKED — needs a human.** The reference transcript must be produced by *listening*; deriving it from a recognizer would make the WER measurement circular and worthless. Everything around this task is done: the corpus directory, provenance README, format contract, Opus round trip, WER scorer and harness all exist and are exercised by the non-speech fixtures. Drop-in procedure is documented step by step in `tests/fixtures/audio/README.md`. Consequence recorded in [VERIFICATION.md](./VERIFICATION.md): SC-003 is verified against non-speech audio only.
- [X] T006 Implement the Opus round-trip helper in `tests/support/mod.rs`: 16 kHz mono PCM → Opus at 48 kHz → decode back to 16 kHz mono, matching what songbird delivers per research.md R5 — a fixture fed as clean PCM measures a pipeline this bot never runs
- [X] T007 Implement the fixture transcription harness in `tests/transcription_fixtures.rs` that walks `tests/fixtures/audio/`, round-trips each clip through T006, transcribes it, and emits a JSON report of transcript text per fixture
- [X] T008 Add word-error-rate computation against the reference transcripts to `tests/support/mod.rs`, so the harness reports a number rather than requiring eyeball comparison
- [X] T009 Make the harness skip with a clear message (not fail) when the Whisper model named by `WHISPER_MODEL_PATH`/`WHISPER_MODEL_NAME` is absent, so `cargo test` stays green on a machine with no model downloaded

### Backend resolution core

- [X] T010 [P] Define `ComputeBackend` and `FallbackReason` per [data-model.md](./data-model.md) in a new `src/telemetry/backend.rs`, with the validation rules encoded (device fields absent when `kind = cpu`; no `fallback_reason` when the resolved backend matches the request)
- [X] T011 Capture ggml device discovery from the existing log forwarder in `src/transcription.rs:158-180` into shared state, so device count and device name are readable rather than only re-emitted as `debug!` — this is the seam that separates `no_device` from `init_failed` (research.md R4)
- [X] T012 Resolve `ComputeBackend` once at worker construction in `src/transcription.rs:52-80`, replacing the current bare `effective_use_gpu` boolean, and return it from `spawn_worker` alongside the handle
- [X] T013 Log the resolved backend at startup in `src/main.rs`, naming the device when `kind = gpu` (FR-006)
- [X] T014 Carry the resolved `ComputeBackend` into `MetricsSnapshot` in `src/telemetry/metrics.rs` as a set-once value beside the existing atomics, not as another counter
- [X] T015 Add `compute_backend` to the `/k8s/metrics` response in `src/telemetry/server.rs:111-118` exactly as specified in [contracts/telemetry.md](./contracts/telemetry.md) (FR-007)
- [X] T016 Update the Swagger document in `src/telemetry/server.rs:135+` to describe the new `compute_backend` field — the contract names this a documentation obligation, not an optional extra

**Checkpoint**: Fixtures exist and are measurable; the bot reports its backend truthfully on the CPU path. US1 and US3 can now begin.

---

## Phase 3: User Story 1 — Operator with a GPU gets GPU transcription (Priority: P1) 🎯 MVP

**Goal**: A published accelerated image that actually uses the GPU, with no build-from-source step.

**Independent Test**: On a GPU host, start the accelerated image, confirm startup output names the device and `nvidia-smi` shows the process holding GPU memory during transcription.

- [X] T017 [US1] Add a `cuda-builder` stage to `Dockerfile` based on `nvidia/cuda:12.9.2-devel-ubuntu24.04` (CUDA 12 not 13 — research.md R2 explains that 13.x drops pre-Turing GPUs), installing rustup and the same native packages the existing `builder` stage installs
- [X] T018 [US1] Build the binary with `--features cuda` in the `cuda-builder` stage, reusing the existing cargo-chef recipe so dependency caching behaves as it does for the CPU stage
- [X] T019 [US1] Add a `cuda-runtime` final stage to `Dockerfile` based on `nvidia/cuda:12.9.2-runtime-ubuntu24.04`, mirroring the CPU `runtime` stage's user creation, venv, volumes, healthcheck and entrypoint, and carrying no build toolchain (FR-005)
- [X] T020 [US1] Parameterise the CUDA version as a single `ARG CUDA_VERSION` used by both CUDA stages, so devel and runtime cannot drift apart
- [X] T021 [US1] Add a second `paws docker` step to `.github/workflows/paws.yml` using `--target cuda-runtime --prepend-target`, which is paws' native two-variant mechanism (research.md R3), keeping the existing CPU step untouched
- [X] T022 [P] [US1] Document `WHISPER_USE_GPU` and `WHISPER_GPU_DEVICE` in `.env.sample`, including which image variant each is meaningful for (FR-013)
- [X] T023 [P] [US1] Add a commented-out GPU device reservation to `compose.yml` alongside the existing service definition (FR-014)
- [X] T024 [P] [US1] Document choosing between variants and granting a container GPU access in `docs/`, including that the image installs no driver (FR-013)
- [X] T025 [US1] Verify quickstart Scenario 2 on a GPU host and record the measured GPU-vs-CPU wall-clock ratio, the GPU model, and the fixture used in the PR description (SC-002, Constitution Principle II)
  - Measured on an RTX 3090 Ti: 0.29 s GPU vs 6.13 s CPU across the whole fixture corpus — **4.7%** of CPU wall-clock, against a 40% budget. Recorded in [VERIFICATION.md](./VERIFICATION.md). Verified with natively built binaries: this host has a driver and `nvcc` but no NVIDIA Container Toolkit, so `docker run --gpus` was unavailable.

**Checkpoint**: The accelerated variant exists, publishes, and demonstrably uses the GPU.

---

## Phase 4: User Story 2 — Operator without a GPU is unaffected (Priority: P1)

**Goal**: The CPU variant costs nothing for the existence of the accelerated one.

**Independent Test**: On a machine with no GPU, run the CPU image with an unchanged existing config and confirm identical behaviour and comparable size to the T001 baseline.

- [X] T026 [US2] Confirm the CPU `runtime` stage remains the Dockerfile's default final stage, so an untargeted `docker build` still produces the CPU image
- [X] T027 [US2] Assert the CPU image carries no CUDA runtime — `ldconfig -p | grep -c cudart` returns 0 — as quickstart Scenario 1 specifies (FR-002)
- [X] T028 [US2] Assert neither variant contains a Rust toolchain, per quickstart Scenario 1 (FR-005)
- [X] T029 [US2] Compare the rebuilt CPU image against the T001 baseline and confirm ≤5% growth (SC-006)
- [X] T030 [US2] Confirm `whisper_use_gpu` still defaults correctly per variant via the existing `cfg!(feature = "cuda")` in `src/config.rs:56` — data-model.md notes FR-010 is already satisfied here, so this task is verification, explicitly not a change
- [X] T031 [US2] Confirm CI builds both variants on every change and that a failure in either fails the job (FR-004)

**Checkpoint**: The CPU path is provably unchanged.

---

## Phase 5: User Story 3 — Misconfiguration is diagnosed, not fatal (Priority: P2)

**Goal**: Each of the four GPU-unavailable causes produces a distinct, actionable message, and the bot keeps working.

**Independent Test**: Run the accelerated image without device passthrough; the bot starts, warns that passthrough is likely missing, and transcribes on CPU.

- [X] T032 [US3] Emit `not_compiled` with a message naming the accelerated image to pull instead, replacing the current generic warning at `src/transcription.rs:62-66`
- [X] T033 [US3] Emit `no_device` when discovery (T011) reports zero devices, stating that container device passthrough is likely missing
- [X] T034 [US3] Emit `invalid_device` when the configured index is ≥ the discovered device count, reporting both the requested index and the number of devices actually present
- [X] T035 [US3] Emit `init_failed` when `WhisperContext::new_with_params` returns `Err`, including the underlying error rather than swallowing it
- [X] T036 [US3] Ensure every fallback path continues on CPU and never aborts startup (FR-009, SC-005)
- [X] T037 [US3] Ensure an operator explicitly setting `WHISPER_USE_GPU=false` produces no warning and no `fallback_reason` — opting out deliberately is a valid choice, not a failure (FR-011); this is the row most easily got wrong
- [X] T038 [US3] Walk the full quickstart Scenario 3 matrix and confirm all four causes are distinguishable in both the startup log and `/k8s/metrics`

**Checkpoint**: All three stories independently functional.

---

## Phase 6: Polish & Cross-Cutting Concerns

- [X] T039 [P] Add the GPU-memory-shortfall edge case from the spec: report the shortfall and fall back rather than crash
- [X] T040 [P] Handle a device becoming unavailable after startup as a counted, logged transcription error, not a silent queue stall (spec Edge Cases, Constitution Principle V)
- [X] T041 Run quickstart Scenario 5 on a GPU host and confirm transcript equivalence between variants across the fixture set (SC-003)
  - Identical output on both backends for every fixture, WER delta 0.00 against a 0.02 tolerance. **Caveat**: the corpus is non-speech only until T005 lands, so this shows the backends agree about silence, not about speech.
- [X] T042 Record in the PR which success criteria were verified and which remain outstanding for lack of GPU hardware — research.md R6 states plainly that a green CI run proves both variants build, not that the GPU one works

---

## Dependencies & Execution Order

### Phase Dependencies

- **Setup (Phase 1)**: no dependencies
- **Foundational (Phase 2)**: depends on Setup — **blocks all stories**
- **US1 (Phase 3)** and **US3 (Phase 5)**: depend on Phase 2's backend core (T010-T016)
- **US2 (Phase 4)**: depends on US1 existing, since it verifies US1 did not damage the CPU path
- **Polish (Phase 6)**: depends on US1 and US3

### Within Phase 2

- T004-T009 (fixtures) and T010-T016 (backend core) are **independent tracks** and can proceed in parallel
- Inside the backend track the order is strict: T010 → T011 → T012 → T013/T014 → T015 → T016
- T014 must precede T015; the endpoint cannot expose what the snapshot does not carry

### Within US1

- T017 → T018 → T019 → T020 are sequential (all edit `Dockerfile`)
- T021 depends on T019 (the stage must exist before CI targets it)
- T022, T023, T024 are parallel — three different files, no shared state
- T025 depends on everything above plus GPU hardware

### Parallel Opportunities

- T002, T003 in Setup
- The two Phase 2 tracks (fixtures / backend core) in full
- T022, T023, T024 within US1
- T039, T040 in Polish
- **US2 cannot be parallelised with US1** — it is verification of US1's blast radius

---

## Parallel Example: Phase 2

```bash
# Track A — fixtures (no source changes)
Task: "Write tests/fixtures/generate.sh for non-speech fixtures"
Task: "Add conversational clips with reference transcripts"

# Track B — backend core (no fixture dependency)
Task: "Define ComputeBackend and FallbackReason in src/telemetry/backend.rs"
```

---

## Implementation Strategy

### MVP scope

**Phase 1 + Phase 2 + Phase 3 (US1).** That delivers a published accelerated image that verifiably uses the GPU, plus the fixture corpus every later feature needs.

US2 is P1 in the spec and should not be skipped — but it is verification rather than construction, so it follows US1 naturally rather than gating it.

### Sequencing note

Phase 2's fixture track is the highest-value work in this entire feature and the easiest to defer under pressure. It is the prerequisite for **every** success criterion in features 002, 003, and 004, not just this one. Deferring it means the next three features have no way to demonstrate they helped.

### Hardware dependency

T025, T041, and parts of T038 need an NVIDIA GPU host. Without one, the feature can be fully implemented and CI-verified as *building*, but SC-002 and SC-003 ship unverified and T042 must say so explicitly rather than letting a green check imply otherwise.

**As implemented, a GPU host was available** (RTX 3090 Ti), so SC-002 and SC-003 were measured rather than deferred. The host lacked the NVIDIA Container Toolkit, so GPU execution was verified with natively built binaries and the image was verified for everything not requiring a device. See [VERIFICATION.md](./VERIFICATION.md) for exactly what that does and does not establish.
