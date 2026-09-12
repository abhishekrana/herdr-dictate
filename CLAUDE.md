# CLAUDE.md

Guidance for Claude Code working in this repository.

## What this is

A [Herdr](https://herdr.dev) plugin: local speech-to-text dictation into the focused pane. One Rust crate, one binary,
no sidecar — whisper.cpp is compiled in, the model is downloaded and verified by this code, and the model server is
this same binary. Linux only (macOS needs a resampler). Audio never leaves the machine.

## Commands

The loop, cheapest first. Each step subsumes the one above it, so run only as far as the change warrants.

```sh
cargo check                               # logic only, no codegen
cargo test                                # inline tests; `cargo test session::` for one module
cargo build --release --features vulkan   # what Herdr runs
scripts/check.sh                          # the gate, before committing
```

`scripts/check.sh` is the gate: a clean local run means a clean CI run, and CI runs this same script. Missing tools are
reported as skipped rather than failing. `SKIP_DOCKER=1` skips the container step, which rebuilds only when `src/` or
the manifests change.

```sh
scripts/install-deps.sh        # build deps, Debian/Ubuntu
scripts/install-dev-tools.sh   # cargo-deny, git-cliff, MSRV toolchain; pinned in scripts/versions.env
```

Acceleration features (`vulkan`, `cuda`, `metal`, `hipblas`) are mutually exclusive — never `--all-features`. One
Vulkan build serves AMD, Intel and NVIDIA. `vulkan` is wired to `whisper-rs-sys` directly because `whisper-rs` does
not forward it. `lto` is off: the work is in whisper.cpp, which it does not reach.

`doctor`, `record --out FILE`, `transcribe FILE` (16 kHz mono WAV) and `setup --print` run outside Herdr; `toggle` and
`deliver` need a live socket. `HERDR_DICTATE_LOG=debug` raises the level; all logging goes to stderr, which Herdr
captures into `herdr plugin log list --plugin abhishekrana.dictate`.

## Architecture

`src/lib.rs` holds everything; `src/main.rs` is a thin clap shell whose every subcommand is a plain function over the
library, so each stage is testable without a terminal, a microphone or a running Herdr.

**A dictation is two invocations of `toggle`.** The first latches the target pane, writes `session::state_path()`, and
records until SIGTERM or trailing silence. The second finds that state file and signals the first. Nothing is a
daemon; the only long-lived process is the optional model server.

- `context` — parses `HERDR_PLUGIN_CONTEXT_JSON`. The focused pane comes from Herdr and is **never recomputed**; it is
  latched at record start so words land where the user was looking. Absent context is not an error (a global hotkey
  arrives with none); `ipc::Client::focused_pane` then asks the server.
- `capture` → `audio` — `capture` is the single platform boundary (cpal/ALSA), opened at 16 kHz mono so nothing
  resamples. `audio` is pure functions over buffers, which is where recording-end decisions are tested.
- `engine` — the `Engine` trait plus `engine::build`; `engine::bundled` is whisper.cpp. `strip_non_speech` drops
  whisper's annotations by *shape* (bracketed, no lowercase), not a name list, and collapses whitespace — which also
  stops a dictated newline from submitting a prompt.
- `model` — a model is named three ways (`name` / `path` / `url` + `sha256`), so an unknown model needs no code
  change. Downloads land in `.part` and are renamed only after the digest matches.
- `server` — holds the engine resident between presses. Spawned lazily and **after** delivering, so two copies never
  load at once. Unreachable is never an error: that press transcribes in-process. The socket is bound only once the
  engine is ready, so connectability *is* readiness. Length-prefixed binary wire format, separate from Herdr's JSON.
- `ipc` — Herdr's socket API, one JSON object per line over `HERDR_SOCKET_PATH`. Spoken to directly rather than by
  spawning `herdr`, keeping process spawns off the delivery path. Errors carry Herdr's own codes.
- `indicator` — the pane label during a dictation. TTL refreshed by a worker thread, so a killed recorder leaves
  nothing stale; cleared on `Drop`, before the words arrive.
- `config` + `setup` — Herdr plugins cannot register their own keys, so `setup` appends them to the user's
  `config.toml`. It **appends text rather than re-serialising** (a round-trip would drop comments and ordering), backs
  up, writes atomically, refuses invalid TOML, and matches by action so a rebound key is not offered twice.
  `config::BINDINGS` is the single source of truth.
- `settings` — the plugin's own `config.toml`. Every section is `#[serde(default, deny_unknown_fields)]`, so a missing
  file is all defaults and a misspelt key is an error rather than silence.
- `session` — the state file behind the two-invocation toggle. Written whole via temp + rename, and `peek` never
  writes, so `status` polling cannot destroy a running dictation. `live` is `peek` filtered to `Phase::Recording`, so
  a press during transcription does not signal the model. `end` only clears a file that still names its own pid.
- `doctor` — one named `Check` per thing that can break, each with a remedy; exits non-zero on any `Fail`.

### Environment contract

Herdr injects `HERDR_SOCKET_PATH`, `HERDR_PLUGIN_CONTEXT_JSON`, `HERDR_PANE_ID`, `HERDR_PLUGIN_CONFIG_DIR` and
`HERDR_PLUGIN_STATE_DIR`. Every lookup treats empty as unset and falls back to the path Herdr would have given, so the
binary stays runnable outside Herdr — the tab bar chip runs exactly that way. `HERDR_CONFIG_PATH` overrides the user
config location. The model server's socket lives under `XDG_RUNTIME_DIR`.

## Rules

- **Self-sufficient.** No external server, script, font or runtime the user has to install; the only program this
  binary starts is itself. It assumes no particular terminal, theme or dotfiles.
- **Nothing personal or work-related in this repository.** The public identity this project is published under is the
  exception and the whole of it: the owner name in `LICENSE`, and the GitHub handle in the plugin id, URLs and the
  commit identity. Everything else stays out — employer, colleagues, hostnames, addresses, home directory paths,
  machine names, credentials — in code, comments, fixtures and commit messages alike. The git identity is repo-local;
  CI runs gitleaks over the tree and the full history.
- **Build dependencies are user dependencies.** `herdr plugin install` compiles on the user's machine, so anything the
  build needs belongs in the README's Requirements.
- **Herdr colours nothing for a plugin.** A tab bar segment is text with no style, and the API exposes no colour
  anywhere, so the chip always takes the tab bar's own colour.

## Deploy

Commit and push first, then make it live:

```sh
scripts/check.sh
git commit && git push
scripts/deploy.sh
```

`deploy.sh` refuses a dirty tree and unpushed commits, so what runs is what is on the branch. It builds with the GPU
backend when the toolchain allows, re-registers the plugin, stops the resident server so it reloads, and ends with
`doctor`.

Verifying without a microphone:

- `doctor` names every dependency and fails on any it cannot satisfy. `plugin.registration` catches a Herdr running a
  different build from the tree being edited.
- `herdr plugin log list --plugin abhishekrana.dictate` is every invocation Herdr made, with exit status.
- `herdr plugin action invoke abhishekrana.dictate.doctor` exercises the path an action really takes.

Development registers the working tree; a release is consumed with `herdr plugin install abhishekrana/herdr-dictate`.
The two are mutually exclusive — unlink before installing.

## Cutting a release

Run in order, stop at the first failure.

1. **Preconditions.** `git status --porcelain` empty, `git fetch && git rev-list --count origin/main..main` is `0`.
2. **Gate.** `scripts/check.sh` passes with **nothing skipped**. A skip means a missing tool — install it rather than
   release unverified.
3. **CI green on `HEAD`.** Local success is not evidence. A cancelled run is not a pass; pushes cancel each other's.
   ```sh
   curl -s "https://api.github.com/repos/abhishekrana/herdr-dictate/actions/runs?head_sha=$(git rev-parse HEAD)" \
     | jq -r '.workflow_runs[] | "\(.name) \(.status)/\(.conclusion)"'
   ```
4. **Version.** `git tag --list 'v*' --sort=-v:refname | head -1` is the previous one. SemVer; on `0.x` a breaking
   change bumps the minor.
5. **Prepare.** `scripts/release.sh vX.Y.Z` sets both manifests and regenerates `CHANGELOG.md`.
6. **Review.** `git diff` — the changelog names every user-visible change, both manifests carry the new version.
7. **Commit and push.** `git commit -am "chore(release): vX.Y.Z" && git push`
8. **CI green on the release commit.** Repeat step 3, so the tag names a commit the runner has already passed.
9. **Tag.** `git tag -a vX.Y.Z -m "herdr-dictate X.Y.Z" && git push origin vX.Y.Z`
10. **Watch.** `gh run watch` — the tag triggers `release.yml`, which re-runs the gate, builds with `--features
    vulkan`, and publishes the tarball, its checksum and a provenance attestation.
11. **Verify.** `gh release view vX.Y.Z`, and `gh attestation verify <tarball> --repo abhishekrana/herdr-dictate`.

On failure, fix forward and cut the next patch. **A published tag is never moved or deleted** — installs resolve their
download by version.

## Conventions

- `#![forbid(unsafe_code)]`. Library errors are the `Error` enum in `lib.rs`; `main.rs` uses `anyhow` with context.
- Failures that cost only a nicety (an indicator, a reload, a server spawn) are logged at debug and swallowed; only a
  lost transcript is an error.
- Tests are inline `#[cfg(test)] mod tests`. Do not add integration tests needing a microphone or a Herdr server.
- Conventional Commits: `feat`, `fix`, `perf`, `refactor`, `docs`, `test`, `build` reach `CHANGELOG.md`; `ci` and
  `chore(release)` are filtered out.
- `Cargo.toml` and `herdr-plugin.toml` both carry the version and CI fails if they disagree.
- Three toolchain versions, deliberately distinct: `rust-version` in `Cargo.toml` is the floor users must have and is
  gated in CI; `RUST_VERSION` in `scripts/versions.env` is what lints run under, so a new clippy is adopted on
  purpose; builds and tests use whatever stable is installed. There is deliberately no `rust-toolchain.toml`.
- Adding a subcommand means a `Command` variant, a function in `main.rs`, and — if Herdr should expose it — an entry
  in `herdr-plugin.toml`.
