ARG RUST_VERSION=1.91
ARG DEBIAN_SUITE=bookworm
ARG RUNTIME_VARIANT=bookworm-slim
ARG APP_HOME=/app
ARG BIN_NAME=hammock
ARG VENV_PATH=/app/venv

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

FROM ghcr.io/astral-sh/uv:latest AS uv

FROM debian:${RUNTIME_VARIANT} AS runtime
ARG APP_HOME
ARG BIN_NAME
ARG VENV_PATH
WORKDIR ${APP_HOME}

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl libopus0 libgomp1 cmake build-essential pkg-config \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --create-home --home ${APP_HOME} bot

COPY --from=uv /uv /uvx /bin/

ENV UV_PROJECT_ENVIRONMENT=${VENV_PATH} \
    VIRTUAL_ENV=${VENV_PATH} \
    PATH=${VENV_PATH}/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin \
    WHISPER_CLI_PATH=${VENV_PATH}/bin/whisper \
    CAPTION_OUTPUT_DIR=${APP_HOME}/captions

COPY pyproject.toml uv.lock ./

RUN uv python install 3.13 \
    && uv python pin 3.13 \
    && uv sync --locked

COPY --from=builder ${APP_HOME}/target/release/${BIN_NAME} /usr/local/bin/${BIN_NAME}
COPY resources ./resources

RUN mkdir -p ${CAPTION_OUTPUT_DIR} && chown -R bot:bot ${APP_HOME}

VOLUME ["/app/captions", "/app/models"]
USER bot
HEALTHCHECK --interval=30s --timeout=5s --start-period=30s --retries=3 \
    CMD curl -fsS http://localhost:8080/k8s/readyz || exit 1
EXPOSE 8080
ENTRYPOINT ["hammock"]
