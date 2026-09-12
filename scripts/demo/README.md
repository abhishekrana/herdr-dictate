# demo

Rebuilds `assets/demo.gif`, a recording of a real session: the plugin action fires, the chip
turns `●`, whisper transcribes, and the transcript is typed into a Claude Code pane and
submitted.

```sh
./scripts/demo/make-demo.sh
DEMO_LINE="Rename the parser module." ./scripts/demo/make-demo.sh
```

| file           | what it is                                                      |
| -------------- | --------------------------------------------------------------- |
| `make-demo.sh` | the entry point: sets up, records, renders, cleans up            |
| `say.py`       | speaks the instruction and captures it as the demo's microphone  |
| `record.py`    | runs Herdr in a pty and drives the dictation, capturing frames   |
| `render.py`    | turns captured frames into the GIF                               |
| `prompt.wav`   | the last spoken instruction, so a rebuild needs no speech engine |

Needs `uv`, `herdr`, `claude`, `ffmpeg`, `fontconfig`, `git`, and PulseAudio or PipeWire with
`spd-say`. `make-demo.sh` names anything absent and stops. `pyte` and `Pillow` install into a
throwaway virtualenv, never system-wide.

Nothing durable changes. The Herdr session, the sample repo and the virtual audio device are
created for the recording and removed after; the default microphone is never repointed, because
only the plugin's own recording stream is moved onto the virtual device. `record.py` refuses to
write frames that show the operator's home path, username or hostname.

`DEMO_LINE`, `DEMO_REPO`, `DEMO_SESSION`, `DEMO_COLS`, `DEMO_ROWS`, `DEMO_ANSWER_SECS`,
`DEMO_WAV`, `DEMO_FRAMES` and `DEMO_GIF` override the defaults.
