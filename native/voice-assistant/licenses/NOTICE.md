# Offline voice dependencies

Sherpa-onnx v1.13.8 (k2-fsa / Xiaomi), Apache-2.0. Universal static no-TTS release, unmodified; archive and model hashes are in ../dependencies.lock.json. Linked: sherpa C/core, ONNX Runtime 1.28.2 (MIT, third-party notices included), kaldi-native-fbank/decoder, OpenFst/kaldifst and SentencePiece (Apache-2.0), KissFFT (BSD-3-Clause, copyright Mark Borgerding). No PortAudio capture library or neural TTS weights are used.

KWS: pkufool, icefall-kws-zipformer-zh-en-3M-2025-12-20, author-declared Apache-2.0; see KWS-MODEL-NOTICE.md. Int8 ONNX conversion supplied by sherpa upstream. VAD: Silero Team, MIT, see silero-vad-LICENSE. Model assets are unmodified official release files. The VAD release filename does not identify an upstream semantic version; pin the exact SHA256, not a claimed version.

System TTS is provided by macOS AVSpeechSynthesizer and is not a bundled third-party voice model. Agent Node and npm licenses are supplied inside agent-runtime/LICENSE and node_modules packages; Pi is pinned to 1.3.0.
