# Changelog

Notable changes, in [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) spirit and versioned with
[SemVer](https://semver.org/).

## [0.1.1] - 2026-09-12

### Added

- The tab bar chip's glyphs come from config (2f45592)
- The tab bar chip turns green while recording (d58a4b9)
- Deploy the working tree, and doctor reports which build Herdr runs (4be933a)
- A dictation chip for the tab bar, and a global hotkey (7fc6340)

### Build

- Docs do not rebuild the container, and CI keeps its layers (3fb5545)
- The secret scan reads the source, not the build output (98ced1b)
- The container caches its dependency layer (e8d5299)

### Changed

- Assets holds the demo, scripts/demo builds it (3cdbcb5)
- The chip config holds whole strings (20b59c6)

### Documentation

- Generate the social preview card with the demo (8a46bd1)
- Render the demo at 2x, and keep the frames (3e0fb42)
- Cut the README to what a new reader needs (41feade)
- A recorded demo of the whole workflow (4ec551b)
- Restructure the README and add compatibility badges (35f5b04)
- Glyph appearance is the terminal's font choice (379902f)
- What deploy means for a linked plugin (b1f171f)
- The test loop, cheapest first (c90b63c)
- Trim the comments to what the code does (2a50bce)
- A bare modifier binds as a global hotkey (c575f1d)

### Fixed

- The gate lints the demo scripts too (43ff61b)
- The chip reads the state file the recorder writes (440b9d1)

### Performance

- Drop LTO, which the C++ hot path never sees (78fe8a2)

## [0.1.0] - 2026-09-11

### Added

- Install fetches the release build, and releases carry provenance (3445e0d)
- One gate script for both local runs and CI, on latest stable deps (b28cb2a)
- One command to prepare a release, and a check that versions agree (d7a61bb)
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

### Build

- Allow the two permissive licences the tree actually uses (96f7559)

### Documentation

- The runbook tags only a commit CI has passed (2fcf79d)
- Trim the README to what a user needs (42c080a)
- Put the GPU choice where the reader decides, without brittle numbers (6c0cdc7)
- Correct how the plugin is set up after installing (8614a6a)
- The gate and the rules a future change could get wrong (9a7cc2e)
- Build requirements, and one GPU build for every vendor (53480df)

### Fixed

- Pin the toolchain lints run under (4aaa01f)
- Install uses the GPU wherever it can, not only from a release (b37bdc4)
- Release notes come from the pinned git-cliff (0c55d3d)
- Identify our own process by its name, not by its path (4e69287)
- Serve-stop stops the server, and tighten the audit (04ed898)
- Ask whisper.cpp for the GPU, and stop retrying low-confidence windows (ed40a60)

