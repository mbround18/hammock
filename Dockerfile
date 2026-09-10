ARG RUST_VERSION=1.91
ARG DEBIAN_SUITE=bookworm
ARG RUNTIME_VARIANT=bookworm-slim
# One knob for both CUDA stages, so the devel image the binary is compiled
# against and the runtime image it executes on can never drift apart.
#
# CUDA 12, not 13, deliberately: the 13.x series drops pre-Turing GPUs, and
# 12.9 covers Pascal through Blackwell — the range a self-hoster is realistically
# running. See specs/001-gpu-accelerated-builds/research.md R2.
ARG CUDA_VERSION=12.9.2
ARG CUDA_UBUNTU=ubuntu24.04
ARG APP_HOME=/app
ARG BIN_NAME=hammock
ARG VENV_PATH=/app/venv
ARG BOT_UID=1000
ARG BOT_GID=1000

FROM rust:${RUST_VERSION}-${DEBIAN_SUITE} AS chef
ARG APP_HOME
WORKDIR ${APP_HOME}
RUN cargo install cargo-chef --locked
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM rust:${RUST_VERSION}-${DEBIAN_SUITE} AS builder
ARG APP_HOME
WORKDIR ${APP_HOME}

RUN apt-get update \
    && apt-get install -y --no-install-recommends build-essential pkg-config libssl-dev clang libclang-dev cmake \
    && rm -rf /var/lib/apt/lists/*

COPY --from=chef /usr/local/cargo/bin/cargo-chef /usr/local/cargo/bin/cargo-chef
COPY --from=chef ${APP_HOME}/recipe.json recipe.json
COPY ./vendor ./vendor

RUN cargo chef cook --release --locked --recipe-path recipe.json

COPY . .

RUN cargo build --release --locked

# ---------------------------------------------------------------------------
# Accelerated variant
#
# Everything below produces the CUDA image. It builds from the same source tree,
# the same lockfiles, and the same cargo-chef recipe as the CPU stages above —
# the two variants differ only in base image and build features, which is what
# keeps them from drifting (FR-004, research.md R3).
#
# These stages sit before `runtime` on purpose: `runtime` must stay the last
# stage in the file so an untargeted `docker build` still produces the CPU image
# (T026). Docker only builds the stages its target depends on, so a default
# build never touches anything here.
# ---------------------------------------------------------------------------

FROM nvidia/cuda:${CUDA_VERSION}-devel-${CUDA_UBUNTU} AS cuda-builder
ARG APP_HOME
ARG RUST_VERSION
WORKDIR ${APP_HOME}

ENV RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    PATH=/usr/local/cargo/bin:/usr/local/cuda/bin:${PATH}

# The same native packages the CPU builder installs, plus three the NVIDIA base
# lacks that the Rust `rust:*-bookworm` base happened to provide:
#   curl, ca-certificates — to fetch rustup; this base carries CUDA but no Rust.
#   git — ggml's CMake requires it on the CUDA path (`find_program(GIT_EXE)` in
#         ggml/CMakeLists.txt) to stamp a build version. Without it the CUDA
#         configure step fails outright; the CPU build never needed it.
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
    build-essential pkg-config libssl-dev clang libclang-dev cmake curl ca-certificates git \
    && rm -rf /var/lib/apt/lists/*

RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
    | sh -s -- -y --no-modify-path --profile minimal --default-toolchain ${RUST_VERSION}

# cargo-chef and the recipe come from the shared `chef` stage rather than being
# rebuilt, so both variants cook an identical dependency graph.
COPY --from=chef /usr/local/cargo/bin/cargo-chef /usr/local/cargo/bin/cargo-chef
COPY --from=chef ${APP_HOME}/recipe.json recipe.json
COPY ./vendor ./vendor

RUN cargo chef cook --release --locked --features cuda --recipe-path recipe.json

COPY . .

# The one line that makes GPU acceleration reachable at all. `cuda` switches
# whisper-rs-sys to GGML_CUDA=ON and links ggml-cuda statically (research.md R1).
RUN cargo build --release --locked --features cuda

FROM ghcr.io/astral-sh/uv:latest AS uv

FROM nvidia/cuda:${CUDA_VERSION}-runtime-${CUDA_UBUNTU} AS cuda-runtime
ARG APP_HOME
ARG BIN_NAME
ARG VENV_PATH
ARG BOT_UID
ARG BOT_GID
WORKDIR ${APP_HOME}

# The `-runtime-` base carries libcudart and friends but not nvcc, so no CUDA
# toolchain reaches the final image. The driver is injected by the container
# runtime at run time and is deliberately not installed here — the operator owns
# the driver (spec Assumptions).
#
# Ubuntu 24.04 ships no `bot` uid/gid conflict at 1000 the way some bases do,
# but it does create a default `ubuntu` user at 1000; it is removed so the uid
# is free for the same `bot` account the CPU image uses.
RUN --mount=type=cache,target=/var/lib/apt/lists \
    --mount=type=cache,target=/var/cache/apt \
    apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl libopus0 libgomp1 cmake build-essential pkg-config \
    && rm -rf /var/lib/apt/lists/* /var/cache/apt/archives/* \
    && (userdel -r ubuntu 2>/dev/null || true) \
    && groupadd --gid ${BOT_GID} bot \
    && useradd --uid ${BOT_UID} --gid ${BOT_GID} --create-home --home ${APP_HOME} bot

COPY --from=uv /uv /uvx /bin/

ENV UV_PROJECT_ENVIRONMENT=${VENV_PATH} \
    VIRTUAL_ENV=${VENV_PATH} \
    PATH=${VENV_PATH}/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin \
    WHISPER_CLI_PATH=${VENV_PATH}/bin/whisper \
    CAPTION_OUTPUT_DIR=${APP_HOME}/captions \
    UV_CACHE_DIR=${APP_HOME}/.cache/uv \
    PIP_DISABLE_PIP_VERSION_CHECK=1 \
    PYTHONUNBUFFERED=1

RUN mkdir -p ${CAPTION_OUTPUT_DIR} ${APP_HOME}/models ${VENV_PATH} ${UV_CACHE_DIR} \
    && chown -R bot:bot ${APP_HOME}

USER bot

COPY --chown=bot:bot pyproject.toml uv.lock ./

# See the CPU runtime stage for why there is no `--mount=type=cache` here.
RUN uv python install 3.13 \
    && uv python pin 3.13 \
    && uv sync --locked

USER root

COPY --from=cuda-builder ${APP_HOME}/target/release/${BIN_NAME} /usr/local/bin/${BIN_NAME}
COPY --chown=bot:bot resources ./resources

# CUDA's own driver stub, kept off the default library search path. The
# entrypoint puts it on LD_LIBRARY_PATH only when no real driver was injected,
# so a container started without `--gpus` loads the binary and falls back to CPU
# (FR-009) instead of dying in the dynamic loader. See docker/cuda-entrypoint.sh.
COPY --from=cuda-builder /usr/local/cuda/lib64/stubs/libcuda.so /opt/cuda-stubs/libcuda.so.1
COPY docker/cuda-entrypoint.sh /usr/local/bin/cuda-entrypoint.sh
RUN chmod +x /usr/local/bin/cuda-entrypoint.sh

# FR-005: the compiler toolchain used to build this image must not ship in it.
RUN --mount=type=cache,target=/var/lib/apt/lists \
    --mount=type=cache,target=/var/cache/apt \
    apt-get purge -y --auto-remove build-essential pkg-config cmake \
    && rm -rf /var/lib/apt/lists/* /var/cache/apt/archives/*

VOLUME ["/app/captions", "/app/models"]
USER bot
HEALTHCHECK --interval=30s --timeout=5s --start-period=30s --retries=3 \
    CMD curl -fsS http://localhost:8080/k8s/readyz || exit 1
EXPOSE 8080
ENTRYPOINT ["cuda-entrypoint.sh"]

# ---------------------------------------------------------------------------
# CPU variant — the default. Must remain the final stage in this file, so an
# untargeted `docker build` keeps producing the CPU image it always has.
# ---------------------------------------------------------------------------

FROM debian:${RUNTIME_VARIANT} AS runtime
ARG APP_HOME
ARG BIN_NAME
ARG VENV_PATH
ARG BOT_UID
ARG BOT_GID
WORKDIR ${APP_HOME}

RUN --mount=type=cache,target=/var/lib/apt/lists \
    --mount=type=cache,target=/var/cache/apt \
    apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl libopus0 libgomp1 cmake build-essential pkg-config \
    && rm -rf /var/lib/apt/lists/* /var/cache/apt/archives/* \
    && groupadd --gid ${BOT_GID} bot \
    && useradd --uid ${BOT_UID} --gid ${BOT_GID} --create-home --home ${APP_HOME} bot

COPY --from=uv /uv /uvx /bin/

ENV UV_PROJECT_ENVIRONMENT=${VENV_PATH} \
    VIRTUAL_ENV=${VENV_PATH} \
    PATH=${VENV_PATH}/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin \
    WHISPER_CLI_PATH=${VENV_PATH}/bin/whisper \
    CAPTION_OUTPUT_DIR=${APP_HOME}/captions \
    UV_CACHE_DIR=${APP_HOME}/.cache/uv \
    PIP_DISABLE_PIP_VERSION_CHECK=1 \
    PYTHONUNBUFFERED=1

RUN mkdir -p ${CAPTION_OUTPUT_DIR} ${APP_HOME}/models ${VENV_PATH} ${UV_CACHE_DIR} \
    && chown -R bot:bot ${APP_HOME}

USER bot

COPY --chown=bot:bot pyproject.toml uv.lock ./

# No `--mount=type=cache` here. An ownership-scoped cache mount (uid=/gid=,
# needed because this runs as `bot`) makes BuildKit emit a `file.mkdir` op
# without makeParents, which Dagger's Dockerfile converter cannot represent:
# "llbtodagger: unsupported op file.mkdir". That breaks `paws docker`, which
# builds through Dagger. Cache mounts never become part of the image, so
# dropping it changes nothing about the result — it only forgoes partial uv
# download reuse on builds where uv.lock itself changed. Any build where the
# lockfile is unchanged still hits the layer cache for this whole step.
RUN uv python install 3.13 \
    && uv python pin 3.13 \
    && uv sync --locked

USER root

COPY --from=builder ${APP_HOME}/target/release/${BIN_NAME} /usr/local/bin/${BIN_NAME}
COPY --chown=bot:bot resources ./resources

RUN --mount=type=cache,target=/var/lib/apt/lists \
    --mount=type=cache,target=/var/cache/apt \
    apt-get purge -y --auto-remove build-essential pkg-config cmake \
    && rm -rf /var/lib/apt/lists/* /var/cache/apt/archives/*

VOLUME ["/app/captions", "/app/models"]
USER bot
HEALTHCHECK --interval=30s --timeout=5s --start-period=30s --retries=3 \
    CMD curl -fsS http://localhost:8080/k8s/readyz || exit 1
EXPOSE 8080
ENTRYPOINT ["hammock"]
