# Contributing to Hammock

Thanks for helping improve Hammock! This project is expressly designed for self-hosted
deployments, so every change should keep privacy, transparency, and operability front and
center. The guidelines below describe the preferred workflow.

## Prerequisites

- Rust toolchain 1.91 (the Docker image pins this version; install via `rustup default 1.91.0`)
- `cargo fmt`, `cargo clippy`, and `cargo test` must pass before opening a PR
- Python managed through [`uv`](https://docs.astral.sh/uv/) (used for installing Whisper CLI
  helpers and summaries dependencies)
- Docker (optional) for validating the multi-stage build and the Actix control-plane probes

## Project structure reminders

- `src/` hosts the Discord bot, Songbird integration, transcription worker, and telemetry
  server. Keep modules single-purpose and async-safe.
- `vendor/whisper-rs-sys/` contains the vendored Whisper bindings. Avoid modifying it unless
  you are updating the upstream crate as well.
- `docs/` is the source of truth for operator-facing documentation. Update it whenever you add
  configuration, env vars, or behavioral changes.

## Development workflow

1. **Fork & branch** – Create feature branches off `main`. Use descriptive names, e.g.,
   `feature/health-endpoint` or `fix/clippy-warnings`.
2. **Environment** – Copy `.env.sample` to `.env` if you want to run the bot locally. At a
   minimum you need `DISCORD_TOKEN` and a Whisper model path.
3. **Coding standards**
   - Rust edition 2024, `cargo fmt --all` formatting
   - Run `cargo clippy --all-targets -- -D warnings`
   - Run `cargo test` (or targeted tests if applicable)
   - For docs, keep Markdown lint-friendly (one sentence per line preferred but not required)
4. **Privacy first** – New features must respect the privacy contract documented in
   `docs/PRIVACY.md`. If you add any data flows or third-party integrations, call them out in
   the docs and gate them behind opt-in configuration.
5. **Observability** – Ensure new runtime features expose meaningful metrics or logging hooks
   when appropriate. Update the `/k8s/metrics` payload if the new behavior affects operator
   visibility.
6. **Documentation** – Update `docs/README.md`, `docs/PRIVACY.md`, and `docs/CONTRIBUTING.md`
   when you add commands, env vars, or other operator-facing behavior.
7. **Pull request** – Describe _why_ the change is needed, include manual/automated test
   evidence, and link to any relevant issues. If the change alters behavior, mention how to
   roll it out safely (config flags, migrations, etc.).

## Running the full integration loop

```bash
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo test
uv sync  # ensures Python deps resolve
cargo run --release  # optional manual end-to-end test
```

For container validation:

```bash
docker compose build
docker compose up --build
curl http://localhost:8080/k8s/readyz
```

If you need to regenerate the Docker entrypoint or add new runtime deps, keep the final image
lean (Cargo Chef prebuild + `uv` install) and document the change in `docs/README.md`.

## Questions?

Open a GitHub issue or start a discussion thread describing the idea. PRs that ship without
context or tests are likely to stall, so collaborating early helps everyone.
