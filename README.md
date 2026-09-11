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

## Usage

```sh
herdr-dictate deliver [--submit]   # read a transcript on stdin, type it into the target pane
herdr-dictate doctor               # report the wiring this plugin depends on
```

## Debugging

`doctor` prints the socket it found, the pane Herdr says is focused, and the pane it would deliver to. Everything else
goes to stderr, which Herdr captures:

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
