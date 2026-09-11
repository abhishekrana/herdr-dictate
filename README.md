# herdr-dictate

[![ci](https://github.com/abhishekrana/herdr-dictate/actions/workflows/ci.yml/badge.svg)](https://github.com/abhishekrana/herdr-dictate/actions/workflows/ci.yml)
[![license](https://img.shields.io/badge/license-MIT-blue)](LICENSE)

Local speech-to-text dictation into the focused [Herdr](https://herdr.dev) pane. Press a key, speak, and the
transcript is typed into the pane you are looking at. Audio never leaves the machine.

> **Status: early.** The socket client and transcript delivery work and are tested. Capture, the speech engine and
> the recording indicator are not built yet, so there is nothing useful to install today.

## Design

**The target pane is the one Herdr names, and is never recomputed.** Herdr hands every plugin command a
`focused_pane_id`; deriving one instead from the agent list disagrees with the user as soon as two agents are live in
the same place. A caller that arrives with no invocation context - a global hotkey, say - asks the server which pane
is focused rather than guessing.

**Delivery inserts, it does not submit.** The transcript lands in the pane's input so you can stack takes, edit them
and send when you mean to. Submitting is a separate, explicit `--submit`.

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

It keeps a `.bak`, appends rather than re-serialising so your comments survive, refuses to touch a config that is
not valid TOML, and matches by action rather than by key - so a binding you moved is not offered again.

## Usage

```sh
herdr-dictate toggle [--submit]    # start recording, or stop and insert the transcript
herdr-dictate deliver [--submit]   # read a transcript on stdin, type it into the target pane
herdr-dictate setup [--apply|--print]
herdr-dictate doctor               # report the wiring this plugin depends on
```

| exit | meaning |
| ---- | ------- |
| 0 | success |
| 1 | failure |
| 2 | unknown subcommand or bad arguments |
| 3 | declared in the manifest, not built yet |

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

Set `HERDR_DICTATE_LOG=debug` for more. A failed delivery names the pane and Herdr's own error code, so
`pane_not_found` reads as exactly that rather than as silence.

## Building

```sh
cargo build --release
cargo test
```

MSRV is declared in `Cargo.toml`. There is deliberately no `rust-toolchain.toml`: pinning one would force every user
onto that exact toolchain. Acceleration for the speech engine will sit behind the `vulkan`, `cuda` and `metal`
features, with the CPU path always available.

## License

MIT
