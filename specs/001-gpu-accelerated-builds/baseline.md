# CPU Image Baseline (pre-feature)

**Task**: T001 | **Serves**: SC-006 ("the CPU image grows by no more than 5%
relative to the current release")

This is the "before" measurement the CPU variant is compared against after the
accelerated variant is introduced. It is recorded here rather than re-derived
later, because the image it describes will have been superseded by the time the
comparison is made.

## Reference artifact

| Property | Value |
|---|---|
| Image | `ghcr.io/mbround18/hammock:sha-4a2bdae` |
| Source commit | `4a2bdae` (merge of #22, the last commit before this feature) |
| Manifest digest | `sha256:a861f6354f28a46e0eb35e5e7542b68c6203f2d0a2d236d3ea93ff003261b55f` |
| Config digest | `sha256:4f437fa86f6b6458dba29c30e972e6c0eba08bda4ada9e6db97aabc2ee2a591f` |
| Media type | `application/vnd.oci.image.manifest.v1+json` |
| Layers | 12 |
| Total compressed size | 3,343,393,081 B = **3343.4 MB** (3188.5 MiB) |

Measured with `docker manifest inspect`, summing `layers[].size`. That is the
**compressed** transfer size — the number an operator waits on when pulling —
not `docker image inspect --format '{{.Size}}'`, which reports the uncompressed
on-disk size and is roughly 2x larger. The two must not be mixed when the
comparison is made.

## Layer breakdown

| # | Size | What it is |
|---|---|---|
| 0 | 26.92 MiB | `debian:bookworm-slim` base |
| 1 | 0.00 MiB | metadata |
| 2 | 128.37 MiB | runtime apt packages |
| 3 | 21.03 MiB | uv binary |
| 4 | 0.20 MiB | app skeleton / user creation |
| 5-7 | ~0.02 MiB | metadata, volumes, entrypoint |
| 8 | **2999.20 MiB** | Python venv (`openai-whisper` and its torch dependency) |
| 9 | 12.65 MiB | the `hammock` binary |
| 10-11 | 0.12 MiB | metadata |

**Worth stating plainly**: 90% of the CPU image is the Python venv, not
anything this feature touches. Layer 9 — the entire Rust binary — is 12.65 MiB.

## The SC-006 budget

5% of 3343.4 MB is **167.2 MB**. The CPU variant passes SC-006 if its total
compressed size is **≤ 3510.6 MB**.

This is a generous budget for a feature whose CPU-path changes are confined to
`src/`, and that is the point: it means any SC-006 failure would indicate the
CUDA runtime leaked into the CPU image, which is exactly the regression the
criterion exists to catch. T027's `ldconfig -p | grep -c cudart` assertion is
the sharper instrument; SC-006 is the backstop.

## The comparison as actually performed (T029)

Registry-vs-local numbers are not comparable, so the SC-006 check was made
local-vs-local: the pre-feature commit was checked out into a git worktree and
built with the same Docker daemon and layer cache as the feature build.

```sh
git worktree add /tmp/baseline-tree 4a2bdae
(cd /tmp/baseline-tree && docker build -t hammock:cpu-baseline .)
docker build -t hammock:cpu .
```

| Image | `docker image inspect --format '{{.Size}}'` |
|---|---|
| `hammock:cpu-baseline` (4a2bdae) | 9,859,767,104 B |
| `hammock:cpu` (this feature) | 9,859,828,815 B |
| **Delta** | **+61,711 B — +0.0006%** |

Budget is +5%. **SC-006 passes** with three orders of magnitude of headroom.

The delta is the compiled binary growing by ~60 KB, which is the whole of this
feature's CPU-path footprint: the backend enum, the ggml log parser, and the
extra metrics field. Nothing else about the CPU image changed.

Note these are uncompressed on-disk sizes, roughly 3x the compressed figures in
the table above. That is fine — both sides of this comparison are measured the
same way, which is the only property that matters.

## How to re-measure

```sh
docker manifest inspect ghcr.io/mbround18/hammock:<new-tag> \
  | python3 -c 'import json,sys; print(sum(l["size"] for l in json.load(sys.stdin)["layers"]))'
```

For a locally built image that has not been pushed, compare uncompressed sizes
against a locally built baseline instead — `docker image inspect` numbers are
only comparable with other `docker image inspect` numbers.
