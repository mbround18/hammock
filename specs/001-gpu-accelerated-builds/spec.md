# Feature Specification: GPU-Accelerated Transcription Builds

**Feature Branch**: `001-gpu-accelerated-builds`

**Created**: 2026-09-08

**Status**: Draft

**Input**: User description: "Make GPU acceleration actually reachable in the deployed artifact. The `cuda` build feature exists but the published container is built without it, on a base image with no GPU toolchain, so GPU transcription is unreachable code and `WHISPER_USE_GPU` defaults to off."

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Operator with a GPU gets GPU transcription (Priority: P1)

An operator self-hosting Hammock on a machine with a discrete NVIDIA GPU pulls the
accelerated image, grants the container access to the GPU, and starts the bot. The
bot loads the speech model onto the GPU and transcribes voice using it, without the
operator having to build anything from source.

**Why this priority**: This is the entire feature. Every other improvement to
throughput and accuracy in the roadmap assumes a working GPU path; none of them can
be realized while the deployed artifact is CPU-only.

**Independent Test**: On a GPU host, start the accelerated image, join a voice
channel, speak, and confirm from startup output that the GPU device was selected
and from host-level GPU monitoring that the device does work during transcription.
Delivers value on its own: faster transcription with no other change.

**Acceptance Scenarios**:

1. **Given** a host with a supported GPU and the container granted device access,
   **When** the operator starts the accelerated image with GPU use enabled,
   **Then** startup output states that GPU acceleration is active and names the
   selected device, and transcription runs on the GPU.
2. **Given** the bot is running with GPU acceleration active, **When** a participant
   speaks, **Then** a caption is produced with the same speaker attribution and
   content quality as the CPU build produces for the same audio.
3. **Given** a host with more than one GPU, **When** the operator selects a specific
   device by configuration, **Then** that device is used and named in startup output.

---

### User Story 2 - Operator without a GPU is unaffected (Priority: P1)

An operator running on a CPU-only VPS continues to run Hammock exactly as before.
The existence of an accelerated variant costs them nothing: no larger image, no new
required configuration, no new failure mode.

**Why this priority**: Equal priority to Story 1 because the constitution treats CPU
as a first-class supported configuration, and CPU-only self-hosters are the majority
of the user base. Shipping GPU support that degrades or complicates the CPU path
would be a net loss.

**Independent Test**: On a machine with no GPU, run the CPU image with an unchanged
existing configuration file and confirm identical behavior and comparable image size
to the current release.

**Acceptance Scenarios**:

1. **Given** an existing CPU-only deployment with its current configuration,
   **When** the operator upgrades to the release containing this feature,
   **Then** the bot starts and transcribes with no configuration change required.
2. **Given** the CPU image, **When** the operator inspects its size, **Then** it is
   not materially larger than the current release image.

---

### User Story 3 - Misconfiguration is diagnosed, not fatal (Priority: P2)

An operator enables GPU use but the GPU is unavailable — they pulled the CPU image
by mistake, forgot to pass the device through, or the driver is missing. The bot
tells them exactly which of those happened and keeps working on CPU.

**Why this priority**: Lower than P1 because it is a recovery path rather than the
primary journey, but it is the difference between a five-minute fix and an
unexplained outage. GPU passthrough is the single most commonly misconfigured part
of a containerized GPU deployment.

**Independent Test**: On a GPU host, deliberately start the accelerated image
*without* granting device access and confirm the bot starts, warns with an
actionable message, and transcribes on CPU.

**Acceptance Scenarios**:

1. **Given** GPU use is enabled but the running image has no GPU support compiled
   in, **When** the bot starts, **Then** it warns that the image is a CPU build,
   names the accelerated image to use instead, and continues on CPU.
2. **Given** GPU use is enabled on an accelerated image but no device is visible to
   the container, **When** the bot starts, **Then** it warns that no device was
   found, states that device passthrough is likely missing, and continues on CPU.
3. **Given** any of the above fallbacks has occurred, **When** an operator inspects
   the bot's exposed operational state, **Then** the active compute backend is
   reported there, not only in startup logs that may have scrolled away.

---

### Edge Cases

- What happens when the GPU has insufficient memory for the configured model? The
  bot must report the shortfall and fall back to CPU rather than crash or hang.
- What happens when a second Hammock instance on the same host claims the same
  device? Both must run; contention is a performance concern, not a correctness one.
- What happens when GPU use is explicitly disabled by configuration on an
  accelerated image? CPU must be used, with no warning — this is a valid choice.
- What happens when the device index names a GPU that does not exist? The bot must
  report the invalid index and the number of devices actually present.
- What happens when the GPU becomes unavailable after successful startup (driver
  reset, device removed)? The failure must be surfaced as a counted, logged error
  rather than a silent stall of the transcription queue.

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001**: The project MUST publish a container image variant in which GPU
  acceleration is compiled in and the GPU runtime libraries required to use it are
  present in the final image.
- **FR-002**: The project MUST continue to publish a CPU-only image that requires no
  GPU drivers, runtime libraries, or device access on the host.
- **FR-003**: The two image variants MUST be distinguishable by tag, and the tag
  scheme MUST make it unambiguous which variant an operator has pulled.
- **FR-004**: Both image variants MUST be built by continuous integration on every
  change, so that neither variant can silently stop building.
- **FR-005**: The runtime image MUST NOT contain the compiler toolchain used to build
  it.
- **FR-006**: The system MUST report, at startup, whether transcription is running on
  GPU or CPU, and when on GPU, which device was selected.
- **FR-007**: The system MUST expose the active compute backend through its existing
  operational-state surface, so it is discoverable after startup output is gone.
- **FR-008**: When GPU use is requested but cannot be satisfied, the system MUST warn
  with a message that distinguishes the cause — not compiled in, no device present,
  invalid device selected, or initialization failure — and MUST continue on CPU.
- **FR-009**: The system MUST NOT fail to start solely because a requested GPU is
  unavailable.
- **FR-010**: GPU use MUST default to enabled on the accelerated image and disabled
  on the CPU image, so that neither variant requires configuration to do the
  expected thing.
- **FR-011**: The operator MUST be able to explicitly disable GPU use on an
  accelerated image, and this MUST be honored without a warning.
- **FR-012**: The operator MUST be able to select which GPU device is used when more
  than one is present.
- **FR-013**: Deployment documentation and the sample environment file MUST describe
  how to grant a container access to a GPU and which image variant to choose.
- **FR-014**: The example deployment configuration MUST include a working,
  commented-out GPU device reservation.

### Key Entities

- **Image variant**: A published build of the bot, characterized by whether GPU
  acceleration is compiled in and whether GPU runtime libraries are present.
- **Compute backend**: The processor actually executing transcription for the running
  instance — GPU (with a device identity) or CPU — together with the reason if it is
  not what was requested.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: On a GPU-equipped host, an operator can go from a published image to
  GPU-backed transcription using only documented configuration, with no compilation
  step.
- **SC-002**: Transcribing an identical fixture recording takes at most 40% of the
  wall-clock time on the accelerated variant that it takes on the CPU variant, on
  comparable hardware.
- **SC-003**: Transcript content for the fixture set is unchanged between the CPU and
  accelerated variants, allowing for documented numerical tolerance.
- **SC-004**: An operator who has misconfigured GPU access can identify the cause
  from the bot's own output alone, without consulting external tooling.
- **SC-005**: 100% of GPU-unavailable scenarios result in a running bot producing
  transcripts, rather than a failed start.
- **SC-006**: The CPU image grows by no more than 5% relative to the current release.
- **SC-007**: Both image variants build successfully in continuous integration on
  every change to the default branch.

## Assumptions

- The accelerated variant targets NVIDIA CUDA, because the repository already
  carries a `cuda` build feature and the vendored native dependency supports it.
  Other backends the underlying library offers (ROCm, Vulkan, Metal, SYCL) are out
  of scope for this feature and may be added later by the same mechanism.
- Operators of the accelerated variant are responsible for installing a compatible
  driver and container runtime on the host; the image does not install drivers.
- The GPU is used for speech transcription only. Nothing else in the bot — audio
  mixing, Discord I/O, caption persistence, summarization — is affected.
- Both variants build from the same source tree and the same lockfiles; they differ
  only in build features and base images.
- "Comparable hardware" for SC-002 means the same host, comparing a run with the GPU
  in use against a run with GPU use disabled by configuration.
- The fixture recordings required by SC-003 are introduced by this feature if they
  do not already exist, since the constitution requires accuracy comparisons and no
  fixture set exists yet.
