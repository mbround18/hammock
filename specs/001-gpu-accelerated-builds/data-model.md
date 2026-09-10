# Phase 1 Data Model: GPU-Accelerated Transcription Builds

**Feature**: [spec.md](./spec.md) | **Plan**: [plan.md](./plan.md)

This feature introduces no persisted data. The entities below are in-memory
values resolved once at startup and read thereafter.

## Entity: ComputeBackend

The processor actually executing transcription for this running instance,
together with why it is what it is.

| Field | Type | Description |
|---|---|---|
| `kind` | enum: `gpu` \| `cpu` | Which processor transcription runs on. |
| `device_index` | integer, present only when `kind = gpu` | The device actually bound. |
| `device_name` | string, present only when `kind = gpu` | Human-readable device identity, as reported by the native library during discovery. |
| `fallback_reason` | enum \| absent | Why GPU was requested but not used. Absent when the backend is what was requested. |

**Resolution**: computed exactly once, when the transcription worker is
constructed, immediately after the native context is created. Never recomputed.
This placement matters for Constitution Principle I — nothing about backend
identity is evaluated on the voice tick path or per utterance.

**Validation rules**:

- `device_index` and `device_name` MUST be absent when `kind = cpu`.
- `fallback_reason` MUST be absent when the resolved `kind` matches what
  configuration requested.
- `fallback_reason` MUST be present whenever GPU was requested and `kind = cpu`.
- A `fallback_reason` of `disabled_by_config` is never produced — an operator
  explicitly disabling GPU is not a fallback (FR-011 requires no warning).

## Entity: FallbackReason

The classified cause when GPU was requested but CPU is in use. These are the four
causes FR-008 requires be distinguishable, plus one for exhaustiveness.

| Value | Meaning | Detection | Operator action |
|---|---|---|---|
| `not_compiled` | Running a CPU image | `cfg!(feature = "cuda")` is false | Pull the accelerated image variant |
| `no_device` | No GPU visible to the process | Native library reports zero devices | Grant the container device access |
| `invalid_device` | Configured index does not exist | Requested index ≥ discovered count | Correct `WHISPER_GPU_DEVICE` |
| `init_failed` | Device present but unusable | Native context construction returned an error | Read the accompanying error; often driver or memory |

Every value maps to exactly one actionable operator instruction. That mapping is
the point of the enum — a single "GPU unavailable" warning would fail FR-008 and
Story 3's acceptance scenarios, which require the operator to identify the cause
from the bot's own output.

**State transitions**: none. The backend is resolved once and is immutable for
the process lifetime. A device that fails *after* startup surfaces as a counted,
logged transcription error (spec Edge Cases), not as a mutation of this value —
which keeps the reported backend an honest record of what was bound at startup.

## Entity: ImageVariant

Not a runtime value — a build-time property, recorded here because it determines
the possible values of `ComputeBackend.kind`.

| Variant | Base image | GPU compiled | `WHISPER_USE_GPU` default |
|---|---|---|---|
| CPU | `debian:bookworm-slim` | no | false |
| Accelerated | `nvidia/cuda:12.9.2-runtime-ubuntu24.04` | yes | true |

The default is derived from the compiled feature, which is what `src/config.rs`
already does (`unwrap_or(cfg!(feature = "cuda"))`). FR-010 is therefore already
satisfied by existing code once the accelerated variant exists — no change
needed, which is worth noting so it does not get "fixed".

## Relationship to existing types

- `MetricsSnapshot` (`src/telemetry/metrics.rs`) gains the resolved
  `ComputeBackend`. It is set-once rather than a counter, so it is carried
  alongside the atomics rather than becoming one.
- `BotConfig` (`src/config.rs`) already carries `whisper_use_gpu` and
  `whisper_gpu_device` — these are the *requested* values. `ComputeBackend` is
  the *resolved* result. Keeping the two distinct is what allows FR-008 to
  report the difference between them.
