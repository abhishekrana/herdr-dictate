# Changelog

Notable changes, in [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) spirit and versioned with
[SemVer](https://semver.org/).

## Unreleased

### Added

- Release workflow, dependency verification and supply-chain checks (b956c74)
- Quantised models, and small.en-q8_0 as the default (c786e70)
- Keep the model resident between dictations (dbb9e62)
- Opt-in GPU backends, and what they are actually worth (ef183da)
- The pane shows what a dictation is doing (7af9f94)
- Toggle records, transcribes and delivers (b7e7009)
- Speech engine with a bundled whisper.cpp backend (7589495)
- Doctor reports one named check per failure (7ae7378)
- Microphone capture at 16 kHz mono (36e6c31)
- Sample conversion and silence detection (77a6dc2)
- Setup action writes the keybindings (e22eb06)
- Plugin manifest and README (c964599)
- Socket client and transcript delivery (aa1a99b)

### Documentation

- Build requirements, and one GPU build for every vendor (53480df)

### Fixed

- Serve-stop stops the server, and tighten the audit (04ed898)
- Ask whisper.cpp for the GPU, and stop retrying low-confidence windows (ed40a60)

