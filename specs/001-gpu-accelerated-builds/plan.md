# Implementation Plan: GPU-Accelerated Transcription Builds

**Branch**: `001-gpu-accelerated-builds` | **Date**: 2026-09-09 | **Spec**: [spec.md](./spec.md)

**Input**: Feature specification from `/specs/001-gpu-accelerated-builds/spec.md`

## Summary

Make the existing but unreachable `cuda` build feature reachable in a published
artifact, without disturbing the CPU path.

Three pieces of work, in dependency order:

1. **Fixture set** — a committed, Opus-round-tripped audio corpus with reference
   transcripts. Required by this feature's SC-003 and by every success criterion
   in features 002-004, and nothing exists today.
2. **Accelerated image variant** — a second final stage in the existing
   Dockerfile, built on CUDA base images and published via
   `paws docker --target … --prepend-target`.
3. **Diagnosable backend reporting** — determine the effective compute backend
   once, distinguish the four reasons GPU use can fail, and surface it in both
   startup logs and `/k8s/metrics`.

## Technical Context

**Language/Version**: Rust 2024 edition, toolchain 1.91 (pinned in `Dockerfile`
via `ARG RUST_VERSION`)

**Primary Dependencies**: `whisper-rs` 0.15.1 with the vendored
`whisper-rs-sys` 0.14.1 (patched in via `[patch.crates-io]`); `serenity`/`poise`
for Discord; `songbird` for voice; `actix-web` for the telemetry server

**Storage**: Filesystem only — caption JSON under `CAPTION_OUTPUT_DIR`, models
under `WHISPER_MODEL_DIR`. Unchanged by this feature.

**Testing**: CI runs `cargo fmt --all -- --check`,
`cargo clippy --all-targets --locked -- -D warnings`, `cargo test --locked`,
`uv lock --check`, and a full image build — all mandatory gates per the
constitution. No tests existed before this feature; it adds the first unit tests
and the first fixture-based accuracy comparison.

Consequence worth stating: because `cargo test` is a gate and CI runners carry no
Whisper model, the fixture harness must **skip** rather than fail when no model
is present, or the gate becomes unpassable rather than meaningful.

**Target Platform**: Linux x86_64 containers. CPU variant on
`debian:bookworm-slim`; accelerated variant on `nvidia/cuda:12.9.2-runtime-ubuntu24.04`
(see [research.md](./research.md) R2 for why 12.x and not 13.x).

**Project Type**: Single Rust binary (Discord bot) shipped as a container image.

**Performance Goals**: Accelerated variant transcribes a fixture recording in
≤40% of the CPU variant's wall-clock time on the same host (SC-002).

**Constraints**: CPU image must not grow by more than 5% (SC-006). GPU
unavailability must never prevent startup (FR-009). Runtime image must not
contain the build toolchain (FR-005).

**Scale/Scope**: Two published image variants; one new HTTP-exposed field; one
new fixture directory. No change to the audio or transcription hot path.

## Constitution Check

*GATE: evaluated before Phase 0, re-evaluated after Phase 1 design.*

| Principle | Assessment |
|---|---|
| **I. Real-Time Voice Fidelity** | **Not engaged.** This feature touches build configuration, startup, and the telemetry surface. It adds nothing to the 20 ms voice tick path. Re-checked post-design: still true — backend detection happens once at worker construction, never per tick. |
| **II. Transcript Accuracy Is Measured, Not Asserted** | **Engaged, and this feature creates the means to satisfy it.** Switching compute backend can plausibly change transcript output, so SC-003 requires a fixture comparison between variants. The fixture set does not exist, so building it is in scope here (Phase 1, work item 1) rather than assumed. |
| **III. Hardware-Optional Parity** | **Directly served.** This is the principle the feature exists to honor. FR-002, FR-009, FR-010, and FR-011 collectively require the CPU path stay first-class and that GPU failure degrade rather than abort. |
| **IV. Configuration Over Recompilation** | **Partially at odds — justified below.** The GPU backend is a build-time feature, not a runtime switch. See Complexity Tracking. Note also a smaller tension resolved in Principle III's favor: IV asks a new tunable to "validate its input at startup with an actionable error", while III and FR-009 forbid failing to start over GPU trouble. So an out-of-range `WHISPER_GPU_DEVICE` warns actionably and continues on CPU rather than aborting. The message still names the requested index and the number of devices present, which is the part IV actually cares about. |
| **V. Observable by Default** | **Directly served, with one deliberate shape choice.** FR-006, FR-007, and FR-008 require the active backend and every fallback cause to be both logged and exposed on the telemetry surface. Principle V says failures must be "counted in metrics and logged"; the backend fallback is exposed as a set-once *state* field rather than a counter, because it is resolved once per process and a counter over it could only ever read 0 or 1. The recurring failure this principle is really aimed at — a device lost after startup — **is** counted, as `metrics.total_transcription_errors`. |

**Gate result: PASS.** One deviation, recorded and justified in Complexity
Tracking rather than waved through.

## Project Structure

### Documentation (this feature)

```text
specs/001-gpu-accelerated-builds/
├── spec.md              # Feature specification
├── plan.md              # This file
├── research.md          # Phase 0 output
├── data-model.md        # Phase 1 output
├── quickstart.md        # Phase 1 output
├── baseline.md          # pre-feature CPU image measurement, for SC-006
├── contracts/           # Phase 1 output
│   └── telemetry.md     # /k8s/metrics compute-backend contract
├── checklists/
│   └── requirements.md  # Spec quality checklist
└── tasks.md             # Created by /speckit-tasks, not this command
```

### Source Code (repository root)

```text
Dockerfile               # + cuda-builder and cuda-runtime stages
compose.yml              # + commented-out GPU device reservation
.env.sample              # + WHISPER_USE_GPU / WHISPER_GPU_DEVICE documentation
.github/workflows/
└── paws.yml             # + second paws docker step for the accelerated variant

src/
├── config.rs            # WHISPER_USE_GPU / WHISPER_GPU_DEVICE already parsed here
├── main.rs              # log the resolved backend at startup
├── transcription.rs     # backend resolution + log-forwarder device capture
└── telemetry/
    ├── backend.rs       # NEW — ComputeBackend, FallbackReason, ggml discovery
    ├── metrics.rs       # carry the resolved backend into MetricsSnapshot
    └── server.rs        # expose it on /k8s/metrics; update the Swagger doc

tests/                   # NEW — no test tree existed before this feature
├── fixtures/
│   ├── generate.sh      # regenerates the synthesized non-speech fixtures
│   └── audio/           # corpus + reference transcripts + provenance README
├── support/
│   └── mod.rs           # WAV reader, Opus round trip, WER, model discovery
└── transcription_fixtures.rs  # the accuracy harness itself
```

**Structure Decision**: Single-project layout, matching what exists. This feature
adds the `tests/` tree and one new source file (`src/telemetry/backend.rs`), and
otherwise modifies files in place. No new crate, module tree, or service is
introduced — the work is concentrated in the Dockerfile, the CI workflow, and
three existing source files.

## Complexity Tracking

| Violation | Why Needed | Simpler Alternative Rejected Because |
|-----------|------------|--------------------------------------|
| GPU backend selected at **build** time, not runtime — at odds with Principle IV ("Configuration Over Recompilation") | The CUDA backend is compiled into the native `ggml` library and statically linked (`vendor/whisper-rs-sys/build.rs:319-320`). A single binary cannot select it at runtime, and the runtime image would have to carry the full CUDA runtime for every CPU-only user. | Shipping one image with both backends was rejected because it forces the ~2 GB CUDA runtime onto every CPU-only self-hoster, violating Principle III's "CPU stays first-class" more severely than the deviation it would fix. Principle IV explicitly reserves build-time features for "things that genuinely cannot be selected at runtime"; this is that case. The deviation is contained: *which* backend is compiled is build-time, but *whether it is used* and *which device* remain runtime env vars (FR-011, FR-012). |
| Two published image variants instead of one | FR-001 and FR-002 require both an accelerated artifact and a CPU artifact with no GPU dependencies. | One variant cannot satisfy both without the size cost above. Mitigated by building both from a single Dockerfile and single lockfile set, so they cannot drift, and by CI building both on every change (FR-004). |

## Post-Design Constitution Re-Check

Re-evaluated after Phase 1 artifacts were written:

- **Principle I** — confirmed unaffected. `data-model.md` places backend
  resolution at worker construction; nothing added per-tick or per-utterance.
- **Principle II** — strengthened. The fixture harness (`quickstart.md`) is the
  reusable mechanism features 002-004 will each need, so the cost is paid once.
- **Principle V** — the telemetry contract (`contracts/telemetry.md`) makes the
  backend and its fallback reason machine-readable, not just loggable.
- **No new violations introduced.** The Complexity Tracking table above is
  complete and unchanged by the design phase.
