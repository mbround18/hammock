# GPU acceleration

Hammock transcribes on the CPU by default and ships a second image variant that
transcribes on an NVIDIA GPU. This page covers choosing between them, granting a
container access to a GPU, and reading what the bot tells you when it does not
work.

GPU acceleration is an optimization, never a requirement. Every feature works on
the CPU build, and a GPU that cannot be used never prevents the bot from
starting — it warns, says which of four things went wrong, and transcribes on
the CPU.

## Choosing a variant

| | CPU image | Accelerated image |
|---|---|---|
| Tags | `latest`, `sha-<sha>`, `v1.2.3` | `cuda-runtime-latest`, `cuda-runtime-sha-<sha>`, `cuda-runtime-v1.2.3` |
| Base | `debian:bookworm-slim` | `nvidia/cuda:12.9.2-runtime-ubuntu24.04` |
| GPU compiled in | no | yes |
| Needs a host driver | no | yes |
| `WHISPER_USE_GPU` default | `false` | `true` |

```sh
docker pull ghcr.io/mbround18/hammock:latest                 # CPU
docker pull ghcr.io/mbround18/hammock:cuda-runtime-latest    # accelerated
```

**The tag tells you which one you have.** A tag beginning `cuda-runtime-` is the
accelerated variant; anything else is the CPU build. There is no tag that is
ambiguous between the two, and there is no single image containing both — the
CUDA runtime is roughly 2 GB, and forcing it onto every CPU-only self-hoster to
avoid a second tag is a bad trade.

Neither variant needs any configuration to do the expected thing. The
accelerated image uses the GPU by default; the CPU image does not look for one.

## What the host needs

The accelerated image contains the CUDA **runtime**. It does not, and cannot,
contain a driver — drivers are kernel components and belong to the host. You
need:

1. **An NVIDIA driver** new enough for CUDA 12.9. Check with `nvidia-smi`; the
   "CUDA Version" it prints is the highest your driver supports, and it must be
   12.9 or higher. Drivers are backward compatible, so a driver reporting 13.x
   runs a 12.9 image fine.
2. **The NVIDIA Container Toolkit**, which is what makes `--gpus` work at all.
   Installation instructions are in
   [NVIDIA's documentation](https://docs.nvidia.com/datacenter/cloud-native/container-toolkit/latest/install-guide.html).
   Verify it with:
   ```sh
   docker run --rm --gpus all nvidia/cuda:12.9.2-runtime-ubuntu24.04 nvidia-smi
   ```
   If that prints your GPU, the container runtime is set up correctly. If it
   does not, fix that before looking at Hammock — the bot cannot see a device
   Docker will not pass through.

CUDA 12.9 covers Pascal (GTX 10-series) through Blackwell. We deliberately did
not build on CUDA 13, which drops the older architectures many self-hosters are
running.

## Running with a GPU

### docker run

```sh
docker run --rm \
  --gpus all \
  --env-file .env \
  -p 8080:8080 \
  -v ./captions:/app/captions \
  -v ./models:/app/models \
  ghcr.io/mbround18/hammock:cuda-runtime-latest
```

### compose

`compose.yml` ships the device reservation commented out. Uncomment it and set
`VERSION` to an accelerated tag:

```yaml
    deploy:
      resources:
        reservations:
          devices:
            - driver: nvidia
              count: 1
              capabilities: [gpu]
```

```sh
VERSION=cuda-runtime-latest docker compose up
```

## Configuration

Both variables are optional. See `.env.sample` for the full commentary.

| Variable | Default | Effect |
|---|---|---|
| `WHISPER_USE_GPU` | `true` on the accelerated image, `false` on the CPU image | Whether to use a GPU when one is available |
| `WHISPER_GPU_DEVICE` | `0` | Which GPU, by index, when the host has more than one |

Device indices match the order `nvidia-smi -L` lists them:

```sh
$ nvidia-smi -L
GPU 0: NVIDIA GeForce RTX 4070 (UUID: GPU-...)
GPU 1: NVIDIA GeForce GTX 1080 (UUID: GPU-...)
```

`WHISPER_GPU_DEVICE=1` selects the GTX 1080. The device actually bound is named
in the startup log and on `/k8s/metrics`, so you can confirm you got the one you
meant.

Setting `WHISPER_USE_GPU=false` on the accelerated image is supported and
silent — no warning, because deliberately running on the CPU is a legitimate
choice, and it is how you compare the two backends on one host without changing
anything else.

## Checking which backend is in use

At startup the bot logs the backend and, on a GPU, the device it bound:

```text
INFO transcription backend: GPU (device 0: NVIDIA GeForce RTX 4070)
```

Startup output scrolls away, so the same fact stays available over HTTP:

```sh
$ curl -s localhost:8080/k8s/metrics | jq .compute_backend
{
  "kind": "gpu",
  "device_index": 0,
  "device_name": "NVIDIA GeForce RTX 4070"
}
```

The full schema is published by the bot itself at `/docs`.

## When the GPU is not used

The bot always starts and always transcribes. When it wanted a GPU and did not
get one it says which of exactly four things happened, because the fix is
different for each. Both the log warning and `compute_backend.fallback_reason`
carry the same value.

| `fallback_reason` | What happened | What to do |
|---|---|---|
| `not_compiled` | You are running the CPU image | Pull a `cuda-runtime-*` tag |
| `no_device` | No GPU is visible to the container | Add `--gpus all` (or the compose reservation), and check the NVIDIA Container Toolkit is installed on the host |
| `invalid_device` | `WHISPER_GPU_DEVICE` names a GPU that does not exist | Set it to a real index — the warning says how many devices are present |
| `init_failed` | A device is there but would not initialize | Read the error in the log; usually a driver mismatch or not enough free GPU memory for the model |

A `no_device` on a machine where `nvidia-smi` works on the host almost always
means passthrough: the host has the GPU, the container was not given it. In that
case the accelerated image also logs, before anything else:

```text
hammock: no NVIDIA driver found in this container; using the CUDA stub
so the bot can start and fall back to CPU. Pass --gpus to use a GPU.
```

That line is a reliable tell that the container never received a device. The
binary links against the NVIDIA driver library, which lives on the host and is
injected by the container runtime; when it is absent the image substitutes
CUDA's own stub so the bot starts and degrades to CPU rather than failing in the
dynamic loader. The stub is used *only* when no real driver was found, so it
never interferes with a working GPU.

If the model is too large for the card, that surfaces as `init_failed` with an
out-of-memory error. Either free memory on the device or set a smaller
`WHISPER_MODEL_NAME`.

### After startup

The backend is resolved once and does not change for the life of the process. If
a device disappears later — driver reset, card removed — transcription jobs
start failing, and those failures are counted at
`/k8s/metrics` under `metrics.total_transcription_errors` as well as logged. A
rising count there with a healthy-looking `compute_backend` means the device
went away after the bot bound it; restart the container.

## Building the accelerated image yourself

```sh
docker build --target cuda-runtime -t hammock:cuda .
```

Both variants build from the same Dockerfile, the same source tree, and the same
lockfiles. They differ only in base image and in the `cuda` cargo feature, which
is what stops them drifting apart. An untargeted `docker build` still produces
the CPU image.

Expect the CUDA build to take substantially longer than the CPU one: it compiles
GPU kernels for every supported architecture.
