# herdr-dictate

[![ci](https://github.com/abhishekrana/herdr-dictate/actions/workflows/ci.yml/badge.svg)](https://github.com/abhishekrana/herdr-dictate/actions/workflows/ci.yml)
[![license](https://img.shields.io/badge/license-MIT-blue)](LICENSE)

Local speech-to-text dictation into the focused [Herdr](https://herdr.dev) pane. Press a key, speak, and the
transcript is typed where you are looking. Audio never leaves the machine.

whisper.cpp is compiled in, the model is downloaded and verified by the plugin, and the model server is this same
binary. There is no sidecar to install and nothing to run yourself.

> Linux only - macOS needs a resampler.

## Requirements

```sh
scripts/install-deps.sh          # Debian and Ubuntu
```

**Install `libvulkan-dev` and `glslc` if the machine has a GPU** - the script does. Installing then uses the GPU
automatically, which is several times faster. Without them you get a CPU-only build, which works everywhere and is the
fallback rather than the default.

| dependency                               | needed for                                        |
| ---------------------------------------- | ------------------------------------------------- |
| Rust 1.88+                               | building                                          |
| `build-essential` `cmake` `libclang-dev` | compiling whisper.cpp                             |
| `libasound2-dev` `pkg-config`            | microphone capture (ALSA)                         |
| `libvulkan-dev` `glslc`                  | GPU acceleration; used automatically when present |

At runtime only `libasound2`, `libvulkan1` and a GPU driver are needed.

## Install

```sh
herdr plugin install abhishekrana/herdr-dictate
herdr plugin pane open --plugin abhishekrana.dictate --entrypoint setup
```

Installing picks the fastest option this machine can run, in order: the released Vulkan binary, else a source build
with Vulkan when `libvulkan-dev` and `glslc` are present, else a plain CPU build. Compiling takes a while; fetching
does not.

The binary is not placed on `PATH`, so `setup` is opened as a plugin pane. Herdr plugins cannot register their own
keys, so `setup` adds them: `prefix+v` toggles, `prefix+shift+v` toggles and submits. It backs up `config.toml`,
appends rather than rewriting so comments survive, refuses a file that is not valid TOML, and matches by action so a
key you moved is not offered again.

## Usage

Press the key, speak, press it again - or stop talking and let trailing silence end it. The pane shows `● dictating`,
then `◌ transcribing`. The transcript is inserted, not submitted. The first dictation downloads the model, so it takes
noticeably longer than the rest.

| command                    | what it does                                           |
| -------------------------- | ------------------------------------------------------ |
| `toggle [--submit]`        | start recording, or stop and insert the transcript     |
| `setup [--apply\|--print]` | write the keybindings                                  |
| `doctor`                   | check every dependency; non-zero exit on failure       |
| `serve` / `serve-stop`     | the model server, started automatically                |
| `transcribe FILE`          | transcribe a 16 kHz mono WAV                           |
| `record --out FILE`        | record a WAV, to check the microphone                  |
| `deliver [--submit]`       | type a transcript read from stdin into the target pane |

`toggle`, `toggle-send` and `doctor` are Herdr actions once installed:

```sh
herdr plugin action invoke abhishekrana.dictate.doctor
```

The table is the binary's own interface, for running it from a clone. Exit codes: `0` success, `1` failure, `2` bad
arguments.

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

| model         | download | accuracy       |
| ------------- | -------- | -------------- |
| tiny.en       | 74 MB    | poor           |
| base.en-q8_0  | 77 MB    | fair           |
| base.en       | 141 MB   | fair           |
| small.en-q8_0 | 252 MB   | good (default) |
| small.en      | 465 MB   | good           |

Within a size class the quantised `q8_0` build is faster for half the download, with no accuracy difference observed.

One Vulkan build drives AMD, Intel and NVIDIA, because every vendor's driver ships a Vulkan ICD, and it still runs on
the CPU when no GPU is present. `cuda`, `metal` and `hipblas` exist for their own hardware but each needs its own SDK
and none is selected automatically. They are mutually exclusive - never build with `--all-features`.

## Troubleshooting

```sh
herdr plugin action invoke abhishekrana.dictate.doctor
herdr plugin log list --plugin abhishekrana.dictate
```

`doctor` names each dependency, what it found and what to do about it. It briefly opens the microphone to compare the
room against the speech threshold; an ambient level at or above it means auto-stop will not fire, so raise
`threshold`.

`HERDR_DICTATE_LOG=debug` increases logging. A failed delivery reports Herdr's own error code, so `pane_not_found`
reads as exactly that.

## Development

```sh
scripts/install-dev-tools.sh     # cargo-deny, git-cliff, the MSRV toolchain
scripts/check.sh                 # the gate CI runs; SKIP_DOCKER=1 skips the container
```

A clean local run means a clean CI run: lints use the toolchain pinned in `scripts/versions.env`, the same one CI
installs. Tools the gate cannot find are reported as skipped. Release steps are in
`CLAUDE.md`. Release binaries are built by CI and carry a provenance attestation, verifiable with
`gh attestation verify <archive> --repo abhishekrana/herdr-dictate`.

## License

MIT
