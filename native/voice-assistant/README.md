# Meeting-only offline voice

`src/voice.cpp` wraps pinned sherpa-onnx KWS + Silero VAD through a single-owner C ABI. It accepts finite 16kHz mono PCM and never opens devices. Chinese wakeword tokenization uses macOS ICU transliteration and validates every phoneme against tokens.txt; unsupported spellings fail during settings save. The default is 小舵同学.

`python3 scripts/prepare-voice.py --download` explicitly downloads locked release assets, validates archives and individual model hashes, and prepares `.tools/voice/bundle`. Ordinary builds are offline. Only chunk-8 int8 encoder/joiner, fp32 decoder, tokens and VAD (~5.8MiB) are bundled. Universal2 static libraries contain both architectures; the wrapper is compiled per Rust target. Licenses and the model author's license declaration are in licenses/.

Rust `voice.rs` receives a bounded, nonblocking copy after microphone persistence. It gates on actual recording/source state, checks epochs, buffers a two-second prefix, detects VAD endpoint/timeout, and schedules question ASR on the existing Whisper worker/context. Model requests use the same `assistant.rs` and Pi session as typed chat. No second large Whisper context is loaded.

TTS lives in `native/macos-audio/AssistantVoice.swift`: AVSpeechSynthesizer writes PCM, the existing PacketConverter owns conversion buffers, and an AVAudioPlayerNode plays a bounded validated answer. Our own PCM/host-clock reference feeds an independent online APM instance. KWS is suspended during answer playback and its echo tail; button/shortcut interruption is supported. Automatic voice barge-in is not enabled before physical acceptance. Raw meeting tracks are never altered; overlapping assistant playback is marked ambiguous for factual analysis, without deleting human double-talk.

Synthetic KWS fixture, no-input/no-output TTS buffer rendering and recording-state gates are tested separately. Real default-wake accuracy, noise/distance, end-to-end latency, speaker echo and Intel performance require actual devices; see the development validation document. Two-hour physical stability remains deferred.
