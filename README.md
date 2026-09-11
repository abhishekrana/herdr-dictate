# herdr-dictate

[![ci](https://github.com/abhishekrana/herdr-dictate/actions/workflows/ci.yml/badge.svg)](https://github.com/abhishekrana/herdr-dictate/actions/workflows/ci.yml)
[![license](https://img.shields.io/badge/license-MIT-blue)](LICENSE)

Local speech-to-text dictation into the focused [Herdr](https://herdr.dev) pane. Press a key, speak, and the transcript
is typed where you are looking. Audio never leaves the machine.

Everything runs inside the plugin: whisper.cpp is compiled in, the model is downloaded and verified by it, and the model
server is this binary. There is no sidecar to install and nothing to run yourself.

> **Status: working, unreleased.** Linux only - macOS needs a resampler.

## Requirements

Installing fetches the release binary when one matches your machine - x86_64 Linux with `libvulkan1` - and builds from
source otherwise. A source build needs:

```sh
scripts/install-deps.sh          # Debian and Ubuntu
```

| dependency                               | needed for                                 |
| ---------------------------------------- | ------------------------------------------ |
| Rust 1.88+                               | building                                   |
| `build-essential` `cmake` `libclang-dev` | compiling whisper.cpp                      |
| `libasound2-dev` `pkg-config`            | microphone capture (ALSA)                  |
| `libvulkan-dev` `glslc`                  | GPU acceleration, `--features vulkan` only |

At runtime only `libasound2`, `libvulkan1` and a GPU driver are needed; a desktop system normally has them. TLS roots
are compiled in, so no system certificate store is required to fetch a model.

## Install

```sh
herdr plugin install abhishekrana/herdr-dictate
herdr plugin pane open --plugin abhishekrana.dictate --entrypoint setup
```

`herdr plugin install` puts the plugin in its own managed checkout and the binary is not placed on `PATH`, so `setup`
is opened as a plugin pane rather than run as a command. It fetches the release binary when it can, and compiles
whisper.cpp when it cannot, which takes about a minute.

Herdr plugins cannot register their own keys, so `setup` adds them. It backs up `config.toml`, appends rather than
rewriting so comments survive, refuses a file that is not valid TOML, and matches by action so a key you moved is not
offered again.

Defaults: `prefix+v` toggles, `prefix+shift+v` toggles and submits. If those keys are already bound, edit the block
`setup` shows you before accepting it.

## Usage

| command                                  | what it does                                           |
| ---------------------------------------- | ------------------------------------------------------ |
| `herdr-dictate toggle [--submit]`        | start recording, or stop and insert the transcript     |
| `herdr-dictate setup [--apply\|--print]` | write the keybindings                                  |
| `herdr-dictate doctor`                   | check every dependency; non-zero exit on failure       |
| `herdr-dictate serve`                    | hold the model resident (started automatically)        |
| `herdr-dictate serve-stop`               | stop the model server                                  |
| `herdr-dictate transcribe FILE`          | transcribe a 16 kHz mono WAV                           |
| `herdr-dictate record --out FILE`        | record a WAV, to check the microphone                  |
| `herdr-dictate deliver [--submit]`       | type a transcript read from stdin into the target pane |

Recording stops on a second press or after two seconds of silence. The pane shows `● dictating`, then `◌ transcribing`.
The transcript is inserted, not submitted, unless you use `--submit`.

After installing, `toggle`, `toggle-send` and `doctor` are Herdr actions - bind them with `setup`, or invoke one
directly:

```sh
herdr plugin action invoke abhishekrana.dictate.doctor
```

The table above is the binary's own interface, used when developing or when running it from a clone.

Exit codes: `0` success, `1` failure, `2` bad arguments.

## Configuration

Optional, at `config.toml` in `herdr plugin config-dir abhishekrana.dictate`:

```toml
[engine]
language = "en"
threads = 0                  # 0 = one per core
prompt = "Terms: worktree, dotfiles, herdr."   # biases the vocabulary

[engine.model]
name = "small.en-q8_0"       # tiny.en, base.en, base.en-q8_0, small.en-q8_0, small.en
# path = "/models/ggml-medium.en.bin"      # a local file, used as-is
# url = "https://.../ggml-large-v3.bin"    # any ggml model
# sha256 = "..."                           # required with url

[silence]
threshold = 300.0            # rms above which a frame counts as speech
trailing_secs = 2.0          # silence that ends a recording; 0 disables auto-stop
max_secs = 120.0

[server]
enabled = true
idle_secs = 300              # 0 keeps the model resident forever
```

`name`, `path` and `url` are alternatives; set exactly one. Models download on first use and are verified against a
pinned SHA-256.

## Models

Measured on 4.0 s of speech:

| model         | download | transcript      |
| ------------- | -------- | --------------- |
| tiny.en       | 74 MB    | mostly wrong    |
| base.en-q8_0  | 77 MB    | two words wrong |
| base.en       | 141 MB   | two words wrong |
| small.en-q8_0 | 252 MB   | exact (default) |
| small.en      | 465 MB   | exact           |

Within a size class the quantised `q8_0` build was faster for half the download and gave the same transcript.

## Acceleration

```sh
cargo build --release --features vulkan
```

One Vulkan build drives AMD, Intel and NVIDIA, because every vendor's driver ships a Vulkan ICD. With no GPU present the
same binary runs on the CPU, without configuration.

| machine                             | GPU   | CPU only |
| ----------------------------------- | ----- | -------- |
| discrete NVIDIA GPU, 24 CPU threads | 0.2 s | 2.0 s    |
| integrated AMD GPU, 16 CPU threads  | 0.6 s | 5.1 s    |

`cuda`, `metal` and `hipblas` also exist and may be faster on their own hardware, but each needs its own SDK. They are
mutually exclusive - never build with `--all-features`.

## Troubleshooting

```sh
herdr plugin action invoke abhishekrana.dictate.doctor
herdr plugin log list --plugin abhishekrana.dictate
```

`doctor` names each dependency, what it found, and what to do about it. It briefly opens the microphone to compare the
room against the speech threshold; an ambient level at or above it means auto-stop will not fire, so raise `threshold`.

`HERDR_DICTATE_LOG=debug` increases logging. A failed delivery reports Herdr's own error code, so `pane_not_found` reads
as exactly that.

## Design

- **The target pane comes from Herdr's invocation context** and is never recomputed. It is latched when recording
  starts, so the words land where you were looking when you spoke.
- **A dictation is two invocations.** The first records and waits; the second finds it through a state file and signals
  it. A state file whose process is gone is cleared rather than believed.
- **The model stays resident.** The first dictation starts a server, after delivering, so two copies never load at once.
  An unreachable server is not an error - that press transcribes in-process.
- **The indicator carries a TTL** and is refreshed while the process lives, so a killed recorder leaves nothing stale.

## Building

```sh
scripts/install-deps.sh          # build dependencies
scripts/install-dev-tools.sh     # cargo-deny, git-cliff, the MSRV toolchain
scripts/check.sh                 # the gate CI runs; SKIP_DOCKER=1 to skip the container
```

`scripts/check.sh` is what CI runs, so a clean local run means a clean CI run. Tools it cannot find are reported as
skipped rather than failing. Pinned tool versions live in `scripts/versions.env`.

MSRV is `rust-version` in `Cargo.toml` and is checked by the gate. There is deliberately no `rust-toolchain.toml`, which
would force every user onto one toolchain.

## Verifying a release

Release binaries are built by CI, never uploaded from a developer machine, and carry a signed provenance attestation
binding them to the workflow, commit and repository:

```sh
gh attestation verify herdr-dictate-0.1.0-x86_64-unknown-linux-gnu.tar.gz --repo abhishekrana/herdr-dictate
```

A SHA-256 is published alongside each archive. The plugin's own install step checks it before using a downloaded binary,
and builds from source if it does not match.

## Releasing

SemVer, with `v`-prefixed tags. A published tag is never moved; bump the patch instead.

```sh
scripts/check.sh                              # must pass with nothing skipped
scripts/release.sh v0.2.0                     # sets both manifests, regenerates CHANGELOG.md
git diff                                      # review
git commit -am "chore(release): v0.2.0"
git tag -a v0.2.0 -m "herdr-dictate 0.2.0"
git push && git push origin v0.2.0
gh run watch                                  # release.yml builds and publishes
```

The tag triggers `release.yml`, which re-runs the gate, builds with `--features vulkan`, publishes a tarball with its
checksum and a provenance attestation, and writes the notes from the commit log.

`CHANGELOG.md` is generated from [Conventional Commits](https://www.conventionalcommits.org/): `feat`, `fix`, `perf`,
`refactor`, `docs`, `test` and `build` appear; `ci` and `chore(release)` are filtered out.

## License

MIT
