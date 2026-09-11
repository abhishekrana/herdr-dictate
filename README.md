# herdr-dictate

[![ci](https://github.com/abhishekrana/herdr-dictate/actions/workflows/ci.yml/badge.svg)](https://github.com/abhishekrana/herdr-dictate/actions/workflows/ci.yml)
[![license](https://img.shields.io/badge/license-MIT-blue)](LICENSE)

Local speech-to-text dictation into the focused [Herdr](https://herdr.dev) pane. Press a key, speak, and the transcript
is typed into the pane you are looking at. Audio never leaves the machine.

> **Status: working, unreleased.** Dictation runs end to end on Linux. There is no recording indicator yet, and macOS
> needs a resampler before it can be enabled.

## Design

**The target pane is the one Herdr names, and is never recomputed.** Herdr hands every plugin command a
`focused_pane_id`; deriving one instead from the agent list disagrees with the user as soon as two agents are live in
the same place. A caller that arrives with no invocation context - a global hotkey, say - asks the server which pane is
focused rather than guessing.

**A dictation is two invocations.** The first press records and waits; the second finds it and asks it to stop, and the
first then transcribes and delivers. The target pane is latched by the first press, because the words belong where you
were looking when you spoke. A state file whose process is gone is cleared rather than believed, so a recorder that
crashed cannot wedge every later press.

**The model stays resident.** A plugin action is a fresh process every press, so the first dictation starts a background
server that holds the engine and serves transcriptions over a socket until idle. It is spawned after that dictation is
delivered, never before, so two copies of the model never load at once. An unreachable server is not an error - the
press transcribes in process instead, so dictation always works.

**The pane says what is happening.** `● dictating` while recording, `◌ transcribing` while the model runs, cleared
before the words arrive. The label carries a TTL and is refreshed while the process lives, so a recorder that is killed
leaves nothing stale behind.

**Delivery inserts, it does not submit.** The transcript lands in the pane's input so you can stack takes, edit them and
send when you mean to. Submitting is a separate, explicit `--submit`.

**Nothing is inferred from the CLI.** The plugin speaks the socket API directly, and every method name and parameter
shape is taken from the bundled schema rather than from the shape of a shell command.

## Install

```sh
herdr plugin install abhishekrana/herdr-dictate
```

Herdr plugins cannot register their own keys, so bind them once:

```sh
herdr-dictate setup          # shows the bindings, asks, writes, reloads the server
```

It keeps a `.bak`, appends rather than re-serialising so your comments survive, refuses to touch a config that is not
valid TOML, and matches by action rather than by key - so a binding you moved is not offered again.

## Usage

```sh
herdr-dictate toggle [--submit]    # start recording, or stop and insert the transcript
herdr-dictate deliver [--submit]   # read a transcript on stdin, type it into the target pane
herdr-dictate setup [--apply|--print]
herdr-dictate doctor               # report the wiring this plugin depends on
```

| exit | meaning                             |
| ---- | ----------------------------------- |
| 0    | success                             |
| 1    | failure                             |
| 2    | unknown subcommand or bad arguments |

## Configuration

Optional, at `config.toml` in the plugin's config directory (`herdr plugin config-dir abhishekrana.dictate`):

```toml
[engine]
language = "en"
threads = 0                  # 0 = one per core
# Biases the vocabulary. Listed terms come out right; terms absent from it are
# likelier to be mis-heard, so change this by measurement rather than by taste.
prompt = "Dictation for a coding terminal. Terms: worktree, dotfiles, herdr."

[engine.model]
name = "small.en-q8_0"       # tiny.en, base.en, base.en-q8_0, small.en-q8_0, small.en
# path = "/models/ggml-medium.en.bin"    # a local file, used as-is
# url = "https://.../ggml-large-v3.bin"  # anything, with a digest
# sha256 = "..."

[silence]
threshold = 300.0            # rms above which a frame counts as speech
trailing_secs = 2.0          # silence that ends a recording; 0 disables auto-stop
max_secs = 120.0
```

Models download on first use into the plugin's state directory and are verified against a pinned SHA-256. `name`, `path`
and `url` are alternatives - set exactly one - and a URL without a digest is refused.

Run `herdr-dictate doctor` after changing `threshold`: it measures your room against it.

## Acceleration

GPU backends are opt-in cargo features, each needing its own SDK at build time, and they are mutually exclusive - do not
build with more than one, and never with `--all-features`:

```sh
cargo build --release --features vulkan    # any Vulkan GPU (AMD, Intel, NVIDIA)
cargo build --release --features cuda      # NVIDIA
cargo build --release --features metal     # Apple
cargo build --release --features hipblas   # AMD ROCm
```

`vulkan` enables the feature on `whisper-rs-sys` directly, because `whisper-rs` does not forward it.

**Measured, and worth knowing before you spend a build on it.** On an AMD Radeon 860M, 4.0 s of speech, median of paired
alternating runs:

| model    | CPU   | Vulkan |
| -------- | ----- | ------ |
| base.en  | 3.4 s | 2.9 s  |
| small.en | 6.5 s | 6.8 s  |

The GPU buys little here and nothing at all with the larger model, even though it is linked and active. The reason is
that each dictation is a fresh process, so Vulkan device setup and the model upload are paid every time and swamp the
compute saving on one clip. A resident model server would change this; that is what makes the same GPU 2.7x faster in a
long-running setup. Until then, CPU is a reasonable default and `openmp` may help more than a GPU feature.

## Models

Everything runs inside this plugin. The model is downloaded and digest-verified by it, whisper.cpp is compiled in, and
the resident server is this binary's own `serve` subcommand - there is no sidecar to install and nothing to run
yourself. At runtime the binary needs only system libraries.

Measured on the same clip, 4.0 s of speech:

| model         | download | time  | transcript      |
| ------------- | -------- | ----- | --------------- |
| tiny.en       | 74 MB    | -     | -               |
| base.en-q8_0  | 77 MB    | 2.7 s | two words wrong |
| base.en       | 141 MB   | 2.8 s | two words wrong |
| small.en-q8_0 | 252 MB   | 6.3 s | exact (default) |
| small.en      | 465 MB   | 8.1 s | exact           |

Within a size class the quantised `q8_0` build is strictly better here: the same transcript, faster, half the download.
Pick `base.en-q8_0` if you would rather have speed than the last few words.

## Debugging

`herdr-dictate doctor` runs one named check per thing that can break, each with a remedy, and exits non-zero when one
fails - so it works in a script as well as by eye:

```
ok    herdr.socket     ~/.config/herdr/herdr.sock
ok    herdr.pane       wA:p1 (asked the server)
ok    audio.device     Default Audio Device - will open at 16000 Hz mono I16
warn  audio.level      ambient rms 355, speech threshold 300
                       -> Room tone counts as speech, so auto-stop may not fire. Raise the threshold.
ok    config.file      ~/.config/herdr/config.toml
warn  config.bindings  unbound: toggle, toggle-send
                       -> Run `herdr-dictate setup`.
```

It opens the microphone briefly to measure the room against the speech threshold, which is the check that catches a
silent mic or a room too noisy for auto-stop before either becomes confusing.

Everything else goes to stderr, which Herdr captures:

```sh
herdr plugin log list --plugin abhishekrana.dictate
```

Set `HERDR_DICTATE_LOG=debug` for more. A failed delivery names the pane and Herdr's own error code, so `pane_not_found`
reads as exactly that rather than as silence.

## Building

```sh
cargo build --release
cargo test
```

MSRV is declared in `Cargo.toml`. There is deliberately no `rust-toolchain.toml`: pinning one would force every user
onto that exact toolchain. Acceleration for the speech engine will sit behind the `vulkan`, `cuda` and `metal` features,
with the CPU path always available.

## License

MIT
