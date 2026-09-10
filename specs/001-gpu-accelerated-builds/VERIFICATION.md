# Verification record

**Task T042.** What was actually verified for this feature, how, and what was
not. Research R6 assumed no GPU host would be available and that SC-002/SC-003
would ship unverified; that assumption did not hold, so both were measured.

Stated plainly up front: **one success criterion (SC-003) is only partly
verified, and one task (T005) is not done.** Details below.

## Hardware and toolchain used

| | |
|---|---|
| GPU | NVIDIA GeForce RTX 3090 Ti (24 GB) |
| Driver | 595.84 |
| Host CUDA toolkit | 12.0 (V12.0.140) — native builds only |
| Image CUDA | 12.9.2 (`nvidia/cuda:12.9.2-{devel,runtime}-ubuntu24.04`) |
| Model | `ggml-base.bin` |
| CPU | 32 threads |

**One caveat that matters.** The host has an NVIDIA driver and `nvcc`, but *not*
the NVIDIA Container Toolkit, so `docker run --gpus` is unavailable here. GPU
execution was therefore verified with **natively built binaries** (`cargo build
--release --features cuda`), and the **image** was verified for everything that
does not require a device. The gap is stated explicitly in the table below
rather than papered over.

## Success criteria

| | Criterion | Status | Evidence |
|---|---|---|---|
| SC-001 | Published image to GPU transcription with no compilation step | **Partly verified** | The accelerated image builds, contains the CUDA runtime, and links CUDA. Pulling a published tag and running it with `--gpus` was not exercised — no container toolkit on this host. |
| SC-002 | Accelerated ≤40% of CPU wall-clock | **PASS** | 0.29 s vs 6.13 s over the fixture corpus — **4.7%**, against a 40% budget. Same binary, same host, `WHISPER_USE_GPU` flipped, so the backend is the only variable. |
| SC-003 | Transcripts unchanged between variants | **PASS on the corpus that exists** | Byte-identical on all four fixtures. But the corpus is non-speech only (T005 outstanding), so this does not yet demonstrate equivalence on speech. |
| SC-004 | Operator can identify a misconfiguration from the bot's own output | **PASS** | All four causes produce distinct, actionable messages. Matrix below. |
| SC-005 | 100% of GPU-unavailable scenarios produce a running bot | **PASS** | All five rows start and transcribe. One required a fix — see Defects found. |
| SC-006 | CPU image grows ≤5% | **PASS** | +61,711 B = **+0.0006%**. See [baseline.md](./baseline.md). |
| SC-007 | Both variants build in CI on every change | **Verified locally, not yet in CI** | Both images build from this tree. The second `paws docker` step is in the workflow but has not run on GitHub yet — that happens on the PR. |

### SC-002 measurement

Whole fixture corpus, 35 s of audio, `--release`:

| Fixture | GPU | CPU |
|---|---|---|
| music-10s | 0.14 s | 1.54 s |
| noise-fan-10s | 0.05 s | 1.54 s |
| noise-keyboard-10s | 0.05 s | 1.55 s |
| silence-5s | 0.05 s | 1.50 s |
| **Total** | **0.29 s** | **6.13 s** |

Ratio **4.7%**, comfortably inside the 40% budget. GPU use was confirmed
independently of timing: `ggml_cuda_init: found 1 CUDA devices`,
`whisper_backend_init_gpu: using CUDA0 backend`, `CUDA0 total size = 147.37 MB`.

### SC-003 measurement

Every fixture produced identical normalized text on both backends — WER delta
0.00 against a documented tolerance of 0.02
(`tests/fixtures/audio/README.md`). Reports: `target/fixture-report-{cpu,gpu}.json`.

Three fixtures produced empty transcripts on both. `music-10s` produced
`(soft music)` on both — a sound-event annotation, i.e. the model correctly
reporting that nothing was said, which the harness distinguishes from invented
words.

**This is a weaker result than SC-003 intends.** With no speech fixtures, it
shows the backends agree about silence, not that they agree about speech.

## Fallback matrix (FR-008, SC-004, quickstart Scenario 3)

Every row started successfully and transcribed on CPU. Checked in both the
startup log and `GET /k8s/metrics`.

| Case | How induced | `compute_backend` | Warned? |
|---|---|---|---|
| GPU working | accelerated binary, GPU present | `{"kind":"gpu","device_index":0,"device_name":"NVIDIA GeForce RTX 3090 Ti"}` | no |
| No passthrough | `CUDA_VISIBLE_DEVICES=` (native); no `--gpus` (image) | `{"kind":"cpu","fallback_reason":"no_device"}` | yes, naming passthrough and the container toolkit |
| Bad index | `WHISPER_GPU_DEVICE=99` | `{"kind":"cpu","fallback_reason":"invalid_device"}` | yes — "does not exist; this host has 1 GPU device(s), numbered 0 to 0" |
| Explicitly disabled | `WHISPER_USE_GPU=false` on the accelerated build | `{"kind":"cpu"}` | **no** — 0 warnings, and no `fallback_reason` (FR-011) |
| CPU build, GPU requested | CPU binary, `WHISPER_USE_GPU=true` | `{"kind":"cpu","fallback_reason":"not_compiled"}` | yes, naming the `cuda-runtime-*` tags to pull |

The "explicitly disabled" row is the one the task list flagged as easiest to get
wrong. It was checked specifically: zero GPU warnings, no `fallback_reason`.

## Image assertions (quickstart Scenario 1)

| Assertion | CPU image | Accelerated image |
|---|---|---|
| Builds | yes | yes |
| `ldconfig -p \| grep -c cudart` | **0** (FR-002) | 1 |
| `command -v cargo rustc nvcc` | nothing (FR-005) | nothing (FR-005) |
| CUDA libs linked by the binary | 0 | 4 |
| Uncompressed size | 9.86 GB | 16.14 GB |
| Starts without a GPU | yes | yes (after the fix below) |

An untargeted `docker build` still produces the CPU image: `runtime` remains the
final stage in the Dockerfile (T026).

## Defects found and fixed during implementation

Two, both found by running the real artifact rather than by reading it:

1. **The accelerated image could not start without a GPU.** The binary links
   `libcuda.so.1`, the NVIDIA *driver* library, which the container runtime
   injects only when a device is passed through. Without `--gpus` the dynamic
   loader refused to start the process at all:
   `error while loading shared libraries: libcuda.so.1`. That is a failure to
   start, which FR-009 forbids and which is precisely Story 3's central
   scenario. Fixed by shipping CUDA's own driver stub off the default search
   path and adding it to `LD_LIBRARY_PATH` from the entrypoint **only when no
   real driver is present** (`docker/cuda-entrypoint.sh`), so it can never
   shadow a working driver. Verified: the image now starts without `--gpus` and
   reports `no_device`.

2. **The CUDA build failed for want of `git`.** ggml's CMake calls
   `find_program(GIT_EXE)` on the CUDA path. The Rust base image happened to
   provide it; `nvidia/cuda:*-devel` does not. Added to the `cuda-builder` apt
   list.

## Findings that are not defects in this feature

**The bot writes sound-event annotations into captions.** Handed background
music, Whisper emits `(soft music)`, and `src/transcription.rs` filters only
`[blank_audio]` — so that text reaches a caption file as though someone had said
it. Real, reproducible, and visible in both fixture reports.

Deliberately not fixed here. This feature is not supposed to change transcript
output, and suppressing annotations would do exactly that, invalidating SC-003's
premise. It belongs with the hallucination work in feature 003. The harness
records it rather than hiding it: every fixture report carries a
`sound_event_annotation` flag.

## Outstanding

| Item | Why | Consequence |
|---|---|---|
| **T005** — 3-5 public-domain conversational speech clips with reference transcripts | Requires sourcing licensed recordings and transcribing them **by listening**, which the implementing agent cannot do. Machine-transcribing them would make the ground truth circular and the WER meaningless. | SC-003 is verified only against non-speech audio. The WER path is implemented, unit-tested, and unexercised on real speech. `tests/fixtures/audio/README.md` documents the exact drop-in procedure. |
| **SC-001 end-to-end from a published tag** | Needs the NVIDIA Container Toolkit, absent on this host. | The image is verified to contain and link CUDA; the last mile — `docker run --gpus` on a published tag — is not. |
| **SC-007 in CI** | The workflow change has not run on GitHub yet. | Verified locally; confirm on the PR's first green run. |

None of these block the feature. All three are stated so a green check is not
mistaken for a claim nobody made — which is the point research.md R6 was making.
