# Quickstart: Validating GPU-Accelerated Builds

**Feature**: [spec.md](./spec.md) | **Plan**: [plan.md](./plan.md)

How to prove this feature works. Each scenario maps to success criteria in the
spec. Scenarios 1, 3, and 4 run anywhere; Scenario 2 needs real GPU hardware.

## Prerequisites

| Scenario | Requires |
|---|---|
| 1, 3 (CPU cases), 4 | Docker, this repo |
| 2, 3 (GPU case) | An NVIDIA GPU host with driver + NVIDIA Container Toolkit |
| 5 | The fixture set from Scenario 4 |

Set `DISCORD_TOKEN` in `.env` before running the bot; see `.env.sample`.

---

## Scenario 1 — Both variants build

**Covers**: FR-001, FR-002, FR-004, FR-005, SC-006, SC-007

```sh
# CPU variant (the existing default stage)
docker build -t hammock:cpu .

# Accelerated variant
docker build --target cuda-runtime -t hammock:cuda .
```

**Expected**:

- Both succeed.
- `docker image inspect hammock:cpu --format '{{.Size}}'` is within 5% of the
  pre-feature image (SC-006).
- Neither image contains a Rust toolchain (FR-005):
  `docker run --rm hammock:cpu sh -c 'command -v cargo rustc'` prints nothing and
  exits non-zero. Repeat for `hammock:cuda`.
- `docker run --rm hammock:cpu sh -c 'ldconfig -p | grep -c cudart'` returns 0 —
  the CPU image carries no CUDA runtime (FR-002).

---

## Scenario 2 — GPU is actually used, and it is faster

**Covers**: SC-001, SC-002 — **requires GPU hardware**

```sh
docker run --rm --gpus all --env-file .env -p 8080:8080 hammock:cuda
```

**Expected**:

- Startup output names the selected device (FR-006).
- `curl -s localhost:8080/k8s/metrics | jq .compute_backend` reports
  `kind: "gpu"` with a device index and name.
- While transcribing, `nvidia-smi` shows the process holding GPU memory.

**Timing comparison** (SC-002 — at most 40% of CPU wall-clock). Run the same
fixture through both, on the same host, changing only the backend:

```sh
docker run --rm --gpus all -e WHISPER_USE_GPU=true  ... # accelerated
docker run --rm --gpus all -e WHISPER_USE_GPU=false ... # same image, CPU
```

Using one image with the flag flipped isolates the backend as the only variable.
Record both numbers and the GPU model in the PR — per Constitution Principle II,
the measurement is part of the deliverable, not a side note.

---

## Scenario 3 — Every fallback cause is distinguishable

**Covers**: FR-008, FR-009, SC-004, SC-005 — the Story 3 journey

Each case must start successfully, transcribe on CPU, and report a distinct
reason. Check both the startup log and
`curl -s localhost:8080/k8s/metrics | jq .compute_backend`.

| Case | How to induce | Expected `fallback_reason` |
|---|---|---|
| Wrong image | Run `hammock:cpu` with `WHISPER_USE_GPU=true` | `not_compiled` |
| No passthrough | Run `hammock:cuda` **without** `--gpus all` | `no_device` |
| Bad index | Run `hammock:cuda --gpus all` with `WHISPER_GPU_DEVICE=99` | `invalid_device` |
| Explicitly disabled | Run `hammock:cuda --gpus all` with `WHISPER_USE_GPU=false` | *absent* — not a fallback (FR-011) |

**Expected in every row**: the bot starts and produces transcripts (FR-009,
SC-005). A row that fails to start is a defect, not an acceptable outcome.

The last row is the easiest to get wrong: an operator opting out deliberately
must not be warned at them.

---

## Scenario 4 — The fixture set exists and is usable

**Covers**: the dependency every other feature inherits

```sh
ls tests/fixtures/audio/
```

**Expected**:

- Conversational clips with sibling reference transcripts.
- Silence / noise / music clips (no speech, so no reference transcript).
- A committed generator script that reproduces the synthesized fixtures.
- A README stating each fixture's provenance and licence.

**Critical property** — fixtures must be Opus round-tripped (research.md R5).
Discord delivers Opus at 48 kHz which songbird decodes to 16 kHz mono; a fixture
fed as clean PCM measures a pipeline this bot never runs. Verify the harness
encodes to Opus and decodes back before transcribing.

---

## Scenario 5 — Transcripts agree between variants

**Covers**: SC-003 — **requires GPU hardware**

Transcribe the full fixture set on both variants and diff the output.

**Expected**: identical transcript text, within the documented numerical
tolerance. Divergence beyond that is a finding to investigate and report, not to
tolerate quietly — it would mean the backend switch changed what users read.

---

## What CI covers, and what it does not

CI runs Scenario 1 only. Scenarios 2, 3 (GPU rows), and 5 need hardware
GitHub-hosted runners do not have, so they are verified by hand and the results
recorded in the PR.

Stated plainly because it is a real limitation: **a green CI run proves both
variants build, not that the GPU one works.** If no GPU host is available,
SC-002 and SC-003 ship unverified and must be called out as outstanding rather
than assumed.
