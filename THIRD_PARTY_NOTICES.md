# Third-party components

HelmMeet source uses the repository Apache-2.0 license. Dependencies retain their own licenses. Cargo.lock and package-lock.json pin exact package versions and registry checksums; this list highlights primary components rather than replacing a complete distribution audit.

| Component | Purpose | Upstream / license |
| --- | --- | --- |
| Tauri 2 | macOS shell and IPC | https://github.com/tauri-apps/tauri · MIT OR Apache-2.0 |
| unicode-segmentation 1.13.3 | Lossless reading boundaries at Unicode graphemes | https://github.com/unicode-rs/unicode-segmentation · MIT OR Apache-2.0 |
| React / React DOM | UI | https://github.com/facebook/react · MIT |
| Vite | UI build | https://github.com/vitejs/vite · MIT |
| whisper-rs 0.16.0 / sys 0.15.0 | Rust wrappers | https://github.com/tazz4843/whisper-rs · see pinned package LICENSE |
| whisper.cpp 1.8.3 | local speech inference / Metal | https://github.com/ggml-org/whisper.cpp · MIT |
| SQLite / rusqlite | local persistence | https://sqlite.org/ (public domain) / https://github.com/rusqlite/rusqlite (MIT) |
| reqwest / native-tls | HTTP with system trust | https://github.com/seanmonstar/reqwest · MIT OR Apache-2.0 |
| Tokio | cancellable asynchronous HTTP and timers | https://github.com/tokio-rs/tokio · MIT |
| swift-rs | Swift build and linking | https://github.com/Brendonovich/swift-rs · MIT OR Apache-2.0 |
| security-framework | macOS Keychain | https://github.com/kornelski/rust-security-framework · see pinned package LICENSE |
| rfd | native file dialogs | https://github.com/PolyMeilex/rfd · MIT |
| webrtc-audio-processing 2.1 (optional `audio-processing` feature) | AP02 standalone local DSP probe; no RTC networking | https://gitlab.freedesktop.org/pulseaudio/webrtc-audio-processing · BSD-3-Clause; upstream WebRTC license/PATENTS and included DSP notices retained |
| Abseil 20240722.1 (APM dependency) | static C++ support library | https://github.com/abseil/abseil-cpp · Apache-2.0 |

APM source archive hashes and target are pinned in `native/audio-processing/dependencies.lock.json`. Redistribution texts for WebRTC, Abseil, PFFFT, embedded RNNoise/VAD, Ooura, SPL sqrt and FFT are in `native/audio-processing/licenses/` and bundled under `licenses/audio-processing/`. The embedded APM RNNoise code is distinct from adopting a separate RNNoise denoising backend. Meson/Ninja/CMake/pkgconf are local build tools, not runtime libraries shipped in the app. No standalone third-party APM binary is downloaded or trusted.

The user's GGML model is read from their local path and is not redistributed in HelmMeet.app. Model source and redistribution terms must be checked before any future bundled download or external release. DeepSeek is an optional user-configured remote service, not code bundled in this repository. macOS frameworks and Swift runtime are provided by the operating system.

For external distribution, generate a complete notice inventory from all locked transitive dependencies and include the required license texts. The current artifact is a local development build without Developer ID notarization.
