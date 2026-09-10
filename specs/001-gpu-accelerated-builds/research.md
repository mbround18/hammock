# Phase 0 Research: GPU-Accelerated Transcription Builds

**Feature**: [spec.md](./spec.md) | **Date**: 2026-09-09

Every finding below was verified against this repository, the vendored native
dependency, the installed `paws` CLI, or a live registry query — not assumed.

## R1: Does the vendored `whisper-rs-sys` actually support a CUDA build?

**Decision**: Yes. Enable it through the existing `cuda` feature; no changes to
`vendor/` are required.

**Evidence**: `vendor/whisper-rs-sys/build.rs` gates real behavior on the feature:

- Line 187-190 sets `GGML_CUDA=ON`, `CMAKE_POSITION_INDEPENDENT_CODE=ON`, and
  `CMAKE_CUDA_FLAGS=-Xcompiler=-fPIC` on the CMake config.
- Line 319-320 links `ggml-cuda` statically.
- Line 56-58 links `cudart` and `cuda`.
- Line 365-384 searches `$CUDA_HOME`/`$CUDA_PATH`, then
  `/usr/local/cuda/targets/x86_64-linux/lib`, `/usr/local/cuda/lib64`, and the
  matching `stubs` directories.

**Consequences for the image**: the build stage needs the full CUDA toolkit
(`nvcc` for the CMake CUDA language, headers, and the link stubs). The runtime
stage needs only `libcudart` plus the driver, which is injected by the container
runtime rather than baked into the image.

**Alternatives considered**: Vulkan or HIP backends, both of which the vendored
crate also supports (`GGML_HIP`, line 194) and which would widen hardware
support beyond NVIDIA. Rejected for this feature because the spec scopes to
CUDA and because each backend multiplies the published image matrix. The build
is structured so a second backend is another stage, not a rewrite.

## R2: Which CUDA base image, and which version?

**Decision**: Build on `nvidia/cuda:12.9.2-devel-ubuntu24.04`, run on
`nvidia/cuda:12.9.2-runtime-ubuntu24.04`. Pin both to the same patch version
through a single build argument.

**Rationale**:

- Verified against the registry that both a `-devel-` and a matching
  `-runtime-` tag exist for this version, which is what lets the runtime image
  stay small while the build stage carries `nvcc`.
- **CUDA 12 rather than 13, deliberately.** The registry offers 13.3.1, but the
  13.x series drops support for older GPU architectures. 12.9 covers Pascal
  through Blackwell, which is the range a self-hoster is realistically running.
  Choosing the newest available version would silently exclude the GTX 10-series
  and similar cards that are perfectly capable of running a Whisper model.
- Build and runtime stages share the same Ubuntu 24.04 base, so the glibc the
  binary is linked against is the glibc it runs on.

**Consequence worth stating**: the accelerated image is Ubuntu-based while the
CPU image stays `debian:bookworm-slim`. That is intentional — NVIDIA does not
publish Debian-based CUDA images — but it means the two variants are not
byte-comparable at the OS layer, only behaviorally comparable. SC-003's
transcript-equivalence check is what covers that.

**Alternatives considered**: installing the CUDA toolkit onto the existing
`rust:1.91-bookworm` builder via NVIDIA's Debian repository. Rejected: it
reproduces what NVIDIA already publishes, and pins us to maintaining an apt
source and key rotation for no benefit.

## R3: How are two image variants published from one repository?

**Decision**: Add a second final stage to the existing multi-stage Dockerfile and
publish it with `paws docker --target <stage> --prepend-target`.

**Evidence**: Verified against the installed CLI's help output —

- `--target` — "Build a specific stage of a multi-stage Dockerfile instead of
  the final stage"
- `--prepend-target` — "Prefix the image tag with `--target`'s name, e.g.
  `<target>-<version>` instead of just `<version>`. Only used with `--target`"

This is exactly the two-variant case, and it satisfies FR-003 (variants
distinguishable by tag) without inventing a tagging scheme: the CPU build keeps
publishing untargeted tags as it does today, and the accelerated build publishes
`cuda-<version>` alongside.

**Rationale**: it keeps both variants building from one source tree and one set
of lockfiles (FR-004's requirement that neither can silently stop building
becomes two steps in the same job), and it leaves the existing CPU publish path
byte-identical to what just shipped and was verified green.

**Alternatives considered**: a separate `Dockerfile.cuda`. Rejected — it
duplicates the chef/builder/venv layers, which is precisely where drift between
the two variants would hide.

## R4: How does the running bot know, and report, which backend is active?

**Decision**: Determine the effective backend once at worker startup and thread
it into both startup logging and the existing HTTP telemetry surface.

**Evidence**: the pieces already exist —

- `src/transcription.rs:59-66` already computes `effective_use_gpu` from the
  requested setting AND `cfg!(feature = "cuda")`, and already warns when GPU was
  requested but not compiled in. That is one of the four causes FR-008 requires
  distinguishing; the other three are not yet separated.
- `src/telemetry/server.rs:53-57` exposes `/k8s/readyz`, `/k8s/livez`,
  `/k8s/metrics`, `/invite`, and `/docs`. `/k8s/metrics` is the natural home for
  FR-007, and `/docs` serves a Swagger document that must be updated alongside.
- `src/telemetry/metrics.rs` holds `AppMetrics` as atomics with a
  `snapshot()` → `MetricsSnapshot`. The backend is set-once, not a counter, so it
  belongs beside the snapshot rather than as another atomic counter.
- `src/transcription.rs:158-180` already installs a `whisper_log_set` forwarder,
  which is where ggml reports CUDA device discovery. This is the seam for
  telling "no device present" apart from "initialization failed".

**The four causes FR-008 must distinguish, and how each is detected**:

| Cause | Detection |
|---|---|
| Not compiled in | `cfg!(feature = "cuda")` — already implemented |
| No device present | ggml reports no CUDA devices via the log forwarder |
| Invalid device index | requested index ≥ discovered device count |
| Initialization failed | `WhisperContext::new_with_params` returns `Err` |

**Consequence**: the last three require the log forwarder to capture device
discovery into shared state rather than only re-emitting it as a `debug!` line,
which is the one non-trivial piece of implementation in this feature.

**Alternatives considered**: shelling out to `nvidia-smi`. Rejected — it reports
what the host has, not what this process successfully bound, and it would add a
runtime dependency the image otherwise does not need.

## R5: The audio fixture set

**Decision**: Build a small, committed fixture set from three sources, stored in
plain git under `tests/fixtures/audio/`, with reference transcripts as sibling
files.

| Fixture class | Source | Purpose |
|---|---|---|
| Conversational speech | Public-domain / CC0 recordings | WER baseline (SC-003, and 003/004's criteria) |
| Silence, noise, music | Generated | Hallucination checks (003 SC-004, 004 SC-003) |
| Vocabulary / proper nouns | Synthesized speech, script committed | 004's SC-002 and SC-005 |

**Rationale**:

- Generated and synthesized fixtures are reproducible and license-clean, and are
  the only way to guarantee a specific recurring proper noun exists for 004's
  SC-005. The generation script is committed so the fixtures can be rebuilt.
- Real recordings are still required for the WER baseline, because synthesized
  speech is unrepresentative of what the recognizer faces.
- **Fixtures must be Opus round-tripped.** Discord delivers Opus at 48 kHz which
  songbird decodes to 16 kHz mono. A fixture fed as clean 16 kHz PCM measures a
  pipeline this bot never runs. The harness must encode to Opus and decode back
  so measurements reflect production audio.

**Sizing**: a handful of 10-30 second 16 kHz mono clips is a few megabytes
total. This repository has no Git LFS configured, and introducing it for a few MB
would add a clone-time dependency for every contributor. Plain git is the right
choice at this size; revisit only if the set grows past tens of megabytes.

**Alternatives considered**: generating fixtures at test time from a network
source. Rejected — it makes the accuracy gate depend on network availability and
on a third party not changing the files underneath us, which would make
regression comparisons meaningless.

## R6: Verifying SC-002 without GPU hardware in CI

**Decision**: CI verifies that both variants *build* (FR-004). The performance
and equivalence criteria (SC-002, SC-003) are verified manually on a GPU host and
the result recorded in the PR, per Constitution Principle II.

**Rationale**: GitHub-hosted runners have no GPU. Verifying SC-002 in CI would
require a self-hosted GPU runner, which is infrastructure this project does not
have and which this feature should not smuggle in. Making the claim honestly —
"measured by hand on this hardware, recorded here" — is better than a green check
that proves nothing.

**Open dependency this creates**: the person implementing needs access to an
NVIDIA GPU host to close SC-002 and SC-003. If no such host is available, those
two criteria cannot be signed off and the feature ships with them explicitly
outstanding rather than silently assumed.

## Resolved unknowns

Every `NEEDS CLARIFICATION` from Technical Context is resolved above. One item is
deliberately carried forward as a **risk, not an unknown**: whether a GPU host is
available for manual verification (R6). It does not block design or
implementation, only final sign-off of two success criteria.
