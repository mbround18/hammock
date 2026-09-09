<!--
Sync Impact Report
==================
Version change: (unversioned template) → 1.0.0
Rationale: Initial ratification. All template placeholders replaced with concrete
project governance; no prior version existed.

Modified principles: none (initial adoption)

Added sections:
  - Core Principles I–V (Real-Time Voice Fidelity; Transcript Accuracy Is Measured,
    Not Asserted; Hardware-Optional Parity; Configuration Over Recompilation;
    Observable by Default)
  - Additional Constraints (technology and deployment constraints)
  - Development Workflow & Quality Gates
  - Governance

Removed sections: none

Follow-up TODOs: none. RATIFICATION_DATE set to the repository's first commit date
(2025-11-30), treated as the project's adoption date.
-->

# Hammock Constitution

Hammock is a Discord voice bot that joins a voice channel, transcribes what each
participant says in near real time, and persists speaker-attributed captions.
These principles govern how it is changed.

## Core Principles

### I. Real-Time Voice Fidelity

The audio receive path is a hard real-time budget and must never be blocked by
downstream work. Songbird delivers a voice tick every 20 ms; any handler on that
path MUST complete without awaiting transcription, disk I/O, network calls, or an
unbounded queue.

When a downstream stage cannot keep up, the system MUST shed load explicitly —
drop the work, count the drop, and log it — rather than apply backpressure to the
receive path. Silently stalling the tick loop loses audio for every speaker in the
channel, which is strictly worse than losing one measured chunk.

Rationale: dropped audio is unrecoverable and invisible. Making overload explicit
is the only way it can be diagnosed or tuned.

### II. Transcript Accuracy Is Measured, Not Asserted

Transcript quality is the product. Any change that can plausibly alter transcript
output — model selection, decoding parameters, segmentation, resampling, VAD — MUST
be evaluated against a fixed, committed audio fixture set before and after, and the
comparison MUST be reported in the change description.

"Sounds better" is not evidence. A change that improves one metric while degrading
another MUST state the tradeoff explicitly.

Rationale: speech recognition parameters interact in non-obvious ways, and
regressions are easy to ship and hard to notice in production, where there is no
ground truth to compare against.

### III. Hardware-Optional Parity

GPU acceleration is an optimization, never a requirement. Every feature MUST work
on a CPU-only build, and the CPU path MUST remain a supported, tested
configuration.

When GPU acceleration is requested but unavailable — the feature was not compiled
in, no device is present, or initialization fails — the system MUST log the reason
at warning level and continue on CPU. It MUST NOT fail to start, and it MUST NOT
silently pretend the GPU is in use.

Rationale: contributors and self-hosters largely do not have CUDA hardware. A build
that only runs on a GPU is a build most users cannot run.

### IV. Configuration Over Recompilation

Operational behavior is configured through environment variables, not code edits or
build flags. Any newly introduced tunable MUST have a documented default that is
safe for a CPU-only self-hoster, MUST appear in `.env.sample` with a comment
explaining its effect, and MUST validate its input at startup with an actionable
error rather than failing later at use.

Build-time features (such as GPU backends) are reserved for things that genuinely
cannot be selected at runtime.

Rationale: the primary deployment is a container image. Anything that requires a
rebuild to change is effectively unavailable to the people running it.

### V. Observable by Default

Every failure, drop, fallback, and degraded mode MUST be both counted in metrics and
logged with enough context to identify the guild, channel, and speaker involved.

An error path that neither increments a counter nor emits a log does not exist as
far as an operator is concerned and MUST NOT be merged. Health and readiness
endpoints MUST reflect the actual ability to serve, not merely that the process is
alive.

Rationale: this system runs unattended in voice channels the maintainer is not
listening to. Undetectable degradation is indistinguishable from working.

## Additional Constraints

**Technology stack**: Rust (2024 edition) using serenity + poise for Discord,
songbird for voice, and whisper-rs for transcription. `whisper-rs-sys` is vendored
under `vendor/` and pinned via `[patch.crates-io]`; the vendored version and the
`whisper-rs` version MUST be kept compatible, and a bump to either requires bumping
both together.

**Audio contract**: audio reaches the transcriber as 16 kHz mono PCM. Resampling
introduced anywhere in the pipeline MUST be anti-aliased; naive decimation is not
acceptable.

**Deployment**: the deliverable is a container image published to GHCR. Both a
CPU-only image and any accelerated variant MUST be buildable from the same
repository, and the runtime image MUST NOT require a toolchain that is only present
in the build stage.

**Dependency hygiene**: `Cargo.lock` and `uv.lock` are committed and authoritative;
builds are `--locked`. Dependency upgrades that cross a major version MUST be
verified to compile, lint, and pass tests before merge.

## Development Workflow & Quality Gates

All changes pass through CI driven by `paws` (`.github/workflows/paws.yml`). The
following gates are mandatory and non-negotiable:

- `cargo fmt --check` — formatting is not a review topic.
- `cargo clippy --all-targets -- -D warnings` — warnings are errors.
- `cargo test` — the suite must pass.
- The container image must build.

Additional workflow rules:

- Feature work follows the Spec Kit flow: specification → plan → tasks →
  implementation. Changes that alter transcript output or the real-time audio path
  require a written spec; routine maintenance does not.
- All commits MUST be cryptographically signed.
- Changes touching the voice or transcription path MUST state their measured effect
  on latency and on transcript accuracy, per Principle II.

## Governance

This constitution supersedes ad-hoc convention. Where a proposed change conflicts
with a principle here, the principle wins unless the constitution is amended first.

**Amendment procedure**: amendments are proposed as a change to this file,
accompanied by the rationale for the change and an assessment of what existing
behavior becomes non-compliant. An amendment is adopted when merged.

**Versioning policy**: this document is versioned semantically.
- MAJOR — a principle is removed or redefined in a backward-incompatible way.
- MINOR — a principle or section is added, or existing guidance is materially
  expanded.
- PATCH — clarification, rewording, or typo correction that does not change meaning.

**Compliance review**: every review verifies that the change complies with these
principles. Complexity that violates a principle MUST be justified in writing in
the change description, or the change MUST be simplified. Deviations accepted
without justification are defects.

**Version**: 1.0.0 | **Ratified**: 2025-11-30 | **Last Amended**: 2026-09-08
