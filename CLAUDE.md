# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

A [Herdr](https://herdr.dev) plugin: local speech-to-text dictation into the focused pane. One Rust crate, one binary,
no sidecar — whisper.cpp is compiled in, the model is downloaded and verified by this code, and the model server is
this same binary. Linux only (macOS needs a resampler). Audio never leaves the machine.

## Commands

```sh
scripts/install-deps.sh                   # build deps, Debian/Ubuntu (cmake, libclang-dev, libasound2-dev, libvulkan-dev, glslc)
cargo build --release
cargo build --release --features vulkan   # GPU; one Vulkan build serves AMD, Intel and NVIDIA
cargo test
cargo test --locked session::             # one module's tests; tests are inline, so paths are module paths
cargo test a_rebound_action_is_not_offered_again
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
RUSTDOCFLAGS=-D warnings cargo doc --no-deps
cargo deny check                          # licences and advisories; config in deny.toml
docker build -f test/Dockerfile -t herdr-dictate-src .   # build-from-source on a bare Ubuntu, as CI does
```

Acceleration features (`vulkan`, `cuda`, `metal`, `hipblas`) are mutually exclusive — never build with
`--all-features`. `vulkan` is wired to `whisper-rs-sys` directly because `whisper-rs` does not forward it.

Exercising the binary by hand: `herdr-dictate doctor`, `record --out FILE`, `transcribe FILE` (16 kHz mono WAV) and
`setup --print` all work outside Herdr. `toggle` and `deliver` need a live Herdr socket.
`HERDR_DICTATE_LOG=debug` raises the log level; all logging goes to stderr, which Herdr captures into
`herdr plugin log list --plugin abhishekrana.dictate`.

## Architecture

`src/lib.rs` holds everything; `src/main.rs` is a thin clap shell whose every subcommand is a plain function over the
library, so each stage is testable without a terminal, a microphone or a running Herdr.

**A dictation is two invocations of `toggle`.** The first latches the target pane, writes `session::state_path()`, and
records until SIGTERM or trailing silence. The second finds that state file and signals the first. Nothing about the
flow is a daemon; the only long-lived process is the optional model server.

Flow through the modules:

- `context` — parses `HERDR_PLUGIN_CONTEXT_JSON`. The focused pane is taken from Herdr and **never recomputed**; it is
  latched at record start so the words land where the user was looking. Absent context is not an error (a global
  hotkey arrives with none), in which case `ipc::Client::focused_pane` asks the server.
- `capture` → `audio` — `capture` is the single platform boundary (cpal/ALSA); the device is opened at 16 kHz mono so
  nothing resamples. `audio` is pure functions over buffers (`rms`, `SilenceDetector`), which is where recording-end
  decisions are tested.
- `engine` — the `Engine` trait plus `engine::build`. `engine::bundled` is whisper.cpp. `engine::strip_non_speech`
  drops whisper's `[BLANK_AUDIO]`-style annotations by *shape* (bracketed, no lowercase), not by a name list, and
  collapses whitespace — which is also what stops a dictated newline from submitting a prompt.
- `model` — a model is named three ways (`name` built-in / `path` local file / `url` + `sha256`), so an unknown model
  needs no code change. Downloads land in a `.part` file and are renamed only after the digest matches.
- `server` — holds the engine resident between presses. Spawned lazily and **after** delivering, so two copies never
  load at once. An unreachable server is never an error: `try_transcribe` returns `None` and that press transcribes
  in-process. The socket is bound only once the engine is ready, so connectability *is* the readiness signal. Its wire
  format is length-prefixed binary (`u32` sample count, then `i16` LE samples; reply is a status byte + `u32` length
  + UTF-8), separate from the Herdr JSON protocol.
- `ipc` — Herdr's socket API: one JSON object per line over `HERDR_SOCKET_PATH`. Spoken to directly rather than by
  spawning the `herdr` binary, keeping process spawns off the delivery path. Errors carry Herdr's own machine-readable
  code, so a failed delivery reports `pane_not_found` as exactly that.
- `indicator` — the `● dictating` / `◌ transcribing` pane label. Carries a TTL refreshed by a worker thread, so a
  killed recorder leaves nothing stale; cleared on `Drop`, before the words arrive.
- `config` + `setup` — Herdr plugins cannot register their own keys, so `setup` appends them to the user's
  `config.toml`. It **appends text rather than re-serialising** (a parse round-trip would drop comments and ordering),
  backs up, writes atomically via a temp file, refuses a file that is not valid TOML, and matches by action so a
  rebound key is not offered twice. `config::BINDINGS` is the single source of truth for suggested keys.
- `settings` — the plugin's own `config.toml` from `HERDR_PLUGIN_CONFIG_DIR`. Every section is `#[serde(default,
  deny_unknown_fields)]`, so a missing file is all defaults and a misspelt key is an error rather than silence.
- `doctor` — one named `Check` per thing that can break, each with a remedy; exits non-zero on any `Fail`.

### Environment contract

Herdr injects `HERDR_SOCKET_PATH`, `HERDR_PLUGIN_CONTEXT_JSON`, `HERDR_PANE_ID`, `HERDR_PLUGIN_CONFIG_DIR` and
`HERDR_PLUGIN_STATE_DIR`. Every lookup treats empty as unset and has a `HOME`-based fallback, so the binary stays
runnable outside Herdr. `HERDR_CONFIG_PATH` overrides the user config location (the tests use it). The model server's
socket lives under `XDG_RUNTIME_DIR`.

## Conventions

- `#![forbid(unsafe_code)]`. Library errors are the `Error` enum in `lib.rs`; `main.rs` uses `anyhow` with context.
- Failures that only cost a nicety (an indicator, a reload, a server spawn) are logged at debug and swallowed; only a
  lost transcript is an error.
- Tests are inline `#[cfg(test)] mod tests` next to the code. `tests/` is empty — do not add integration tests that
  need a microphone or a Herdr server to CI.
- Conventional Commits: `feat`, `fix`, `perf`, `refactor`, `docs`, `test`, `build` reach `CHANGELOG.md`; `ci` and
  `chore(release)` are filtered out (`cliff.toml`).
- `Cargo.toml` and `herdr-plugin.toml` both carry the version and CI fails if they disagree. `scripts/release.sh vX.Y.Z`
  sets both and regenerates the changelog; a published tag is never moved.
- MSRV is `rust-version` in `Cargo.toml` and is gated in CI, because users build this plugin themselves on install.
  There is deliberately no `rust-toolchain.toml`.
- Adding a subcommand means a `Command` variant, a function in `main.rs`, and — if Herdr should expose it — an entry in
  `herdr-plugin.toml`.
