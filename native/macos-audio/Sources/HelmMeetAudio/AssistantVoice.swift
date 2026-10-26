import AVFoundation
import AppKit
import Darwin

let maximumVoicePCMSamples = 16000 * 45

// The C ABI and the player must apply the same limit. Validate before copying
// caller memory; the injected sink also permits device-free boundary tests.
func submitVoicePCM(_ input: UnsafePointer<Float>, _ count: Int32, play: ([Float]) -> Int32) -> Int32 {
    guard count > 0, count <= maximumVoicePCMSamples else { return 1 }
    let pcm = Array(UnsafeBufferPointer(start: input, count: Int(count)))
    guard pcm.allSatisfy({ $0.isFinite && abs($0) <= 1 }) else { return 1 }
    return play(pcm)
}

// Runs on the main queue. No input node is ever accessed. Buffer synthesis is
// bounded before playback; Rust owns permission, turn validity and cancellation.
final class AssistantSpeaker {
    let synthesizer = AVSpeechSynthesizer()
    var engine: AVAudioEngine?
    var player: AVAudioPlayerNode?
    var generation: UInt64 = 0
    var playbackGeneration: UInt64 = 0
    var status: Int32 = 0 // 0 idle, 1 rendering, 2 playing, -1 unavailable
    var reference: [Float] = []
    var referenceStart: UInt64 = 0
    var lockObservers: [NSObjectProtocol] = []
    var screenLocked = false
    var cache: [String: [Float]] = [:]
    init() {
        for name in ["com.apple.screenIsLocked", "com.apple.screenIsUnlocked"] {
            lockObservers.append(DistributedNotificationCenter.default().addObserver(forName: NSNotification.Name(name), object: nil, queue: .main) { [weak self] event in
                self?.screenLocked = event.name.rawValue.hasSuffix("Locked")
                if self?.screenLocked == true { self?.stop() }
            })
        }
        lockObservers.append(NSWorkspace.shared.notificationCenter.addObserver(forName: NSWorkspace.willSleepNotification, object: nil, queue: .main) { [weak self] _ in self?.screenLocked = true; self?.stop() })
        lockObservers.append(NSWorkspace.shared.notificationCenter.addObserver(forName: NSWorkspace.didWakeNotification, object: nil, queue: .main) { [weak self] _ in self?.screenLocked = false })
    }
    func stop() {
        generation &+= 1
        playbackGeneration &+= 1
        synthesizer.stopSpeaking(at: .immediate)
        player?.stop(); engine?.stop(); player = nil; engine = nil
        reference = []; referenceStart = 0; status = 0
    }
    func speak(_ text: String, renderOnly: (([Float]) -> Void)? = nil) -> Int32 {
        stop()
        guard !screenLocked, text.count <= 600,
              let voice = AVSpeechSynthesisVoice.speechVoices().first(where: { $0.language == "zh-CN" }) else { status = -1; return 1 }
        let token = generation
        if let pcm = cache[text] { if let done = renderOnly { done(pcm) } else { play(pcm, token) }; return 0 }
        let utterance = AVSpeechUtterance(string: text)
        utterance.voice = voice
        utterance.rate = AVSpeechUtteranceDefaultSpeechRate
        status = 1
        var pcm: [Float] = []
        var converter: PacketConverter?
        var completed = false
        let target = AVAudioFormat(commonFormat: .pcmFormatFloat32, sampleRate: 16000, channels: 1, interleaved: false)!
        synthesizer.write(utterance) { [weak self] buffer in
            guard let self = self, let audio = buffer as? AVAudioPCMBuffer else { return }
            // write callbacks may not be delivered on the main queue. Own each
            // buffer until conversion completes, then serialize state updates.
            guard let copy = AVAudioPCMBuffer(pcmFormat: audio.format, frameCapacity: audio.frameLength) else { return }
            copy.frameLength = audio.frameLength
            let src = UnsafeMutableAudioBufferListPointer(audio.mutableAudioBufferList)
            let dst = UnsafeMutableAudioBufferListPointer(copy.mutableAudioBufferList)
            for i in 0..<src.count { if src[i].mDataByteSize > 0 { memcpy(dst[i].mData!, src[i].mData!, Int(src[i].mDataByteSize)) } }
            DispatchQueue.main.async {
                guard self.generation == token, !completed else { return }
                if copy.frameLength == 0 {
                    // AVSpeechSynthesizer may deliver more than one empty
                    // terminal buffer. Finish this utterance exactly once.
                    completed = true
                    guard !pcm.isEmpty else { self.status = -1; return }
                    if text == "小舵在" { self.cache[text] = pcm }
                    if let done = renderOnly { self.status = 0; done(pcm) } else { self.play(pcm, token) }
                    return
                }
                if converter == nil { converter = PacketConverter(copy.format, target) }
                guard let converted = converter?.convert(copy) else { self.stop(); self.status = -1; return }
                pcm.append(contentsOf: converted)
                if pcm.count > maximumVoicePCMSamples { self.stop(); self.status = -1 }
            }
        }
        return 0
    }
    func playPCM(_ pcm: [Float]) -> Int32 {
        guard !pcm.isEmpty, pcm.count <= maximumVoicePCMSamples,
              pcm.allSatisfy({ $0.isFinite && abs($0) <= 1 }) else { return 1 }
        stop()
        guard !screenLocked else { status = -1; return 1 }
        play(pcm, generation)
        return status == 2 ? 0 : 1
    }
    func finishPlayback(_ token: UInt64, _ playbackToken: UInt64, notBefore: UInt64) {
        guard generation == token, playbackGeneration == playbackToken else { return }
        let now = mach_absolute_time()
        if now < notBefore {
            // Keep the engine alive through the complete scheduled PCM and the
            // output route latency even if a device reports completion early.
            let delay = AVAudioTime.seconds(forHostTime: notBefore - now)
            DispatchQueue.main.asyncAfter(deadline: .now() + delay) { [weak self] in
                self?.finishPlayback(token, playbackToken, notBefore: notBefore)
            }
            return
        }
        player?.stop(); engine?.stop(); player = nil; engine = nil; status = 0
    }
    func play(_ pcm: [Float], _ token: UInt64) {
        guard generation == token, !screenLocked else { return }
        playbackGeneration &+= 1
        let playbackToken = playbackGeneration
        let format = AVAudioFormat(commonFormat: .pcmFormatFloat32, sampleRate: 16000, channels: 1, interleaved: false)!
        guard let buffer = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: AVAudioFrameCount(pcm.count)) else { status = -1; return }
        buffer.frameLength = buffer.frameCapacity
        pcm.withUnsafeBufferPointer { input in buffer.floatChannelData![0].update(from: input.baseAddress!, count: input.count) }
        let audio = AVAudioEngine(); let node = AVAudioPlayerNode()
        audio.attach(node); audio.connect(node, to: audio.mainMixerNode, format: format)
        do { try audio.start() } catch { status = -1; return }
        engine = audio; player = node; reference = pcm
        let time = mach_absolute_time() + AVAudioTime.hostTime(forSeconds: 0.05)
        referenceStart = UInt64((AVAudioTime.seconds(forHostTime: time) + audio.outputNode.presentationLatency) * 1_000_000_000)
        let end = time + AVAudioTime.hostTime(forSeconds: Double(pcm.count) / format.sampleRate + audio.outputNode.presentationLatency)
        status = 2
        node.scheduleBuffer(buffer, at: nil, options: [], completionCallbackType: .dataPlayedBack) { [weak self] _ in
            DispatchQueue.main.async { self?.finishPlayback(token, playbackToken, notBefore: end) }
        }
        node.play(at: AVAudioTime(hostTime: time))
    }
}
private var speaker: AssistantSpeaker?
private func onVoiceMain<T>(_ run: () -> T) -> T { Thread.isMainThread ? run() : DispatchQueue.main.sync(execute: run) }
private func getSpeaker() -> AssistantSpeaker { if speaker == nil { speaker = AssistantSpeaker() }; return speaker! }
@_cdecl("hm_voice_speak") public func hmVoiceSpeak(_ text: UnsafePointer<CChar>) -> Int32 {
    let owned = String(cString: text)
    return onVoiceMain { getSpeaker().speak(owned) }
}
@_cdecl("hm_voice_play_pcm") public func hmVoicePlayPCM(_ input: UnsafePointer<Float>, _ count: Int32) -> Int32 {
    // Own the caller's PCM before moving to the main queue. Uses the same player,
    // cancellation fence and timestamped AEC reference as synthesized answers.
    return submitVoicePCM(input, count) { pcm in
        onVoiceMain { getSpeaker().playPCM(pcm) }
    }
}
@_cdecl("hm_voice_stop") public func hmVoiceStop() { onVoiceMain { speaker?.stop() } }
@_cdecl("hm_voice_state") public func hmVoiceState() -> Int32 { onVoiceMain { getSpeaker().screenLocked ? -2 : getSpeaker().status } }
@_cdecl("hm_voice_reference") public func hmVoiceReference(_ host: UInt64, _ output: UnsafeMutablePointer<Float>, _ count: Int32) -> Int32 {
    guard count > 0, count <= 8192 else { return -1 }
    return onVoiceMain {
        let s = getSpeaker()
        for i in 0..<Int(count) { output[i] = 0 }
        guard s.referenceStart > 0 else { return 0 }
        var active: Int32 = 0
        for i in 0..<Int(count) {
            let at = Int64(host) + Int64(i) * 62500 - Int64(s.referenceStart)
            let index = at / 62500
            if at >= 0, index < s.reference.count { output[i] = s.reference[Int(index)]; active = 1 }
        }
        return active
    }
}
// Caller supplies a bounded output. Pinyin is then checked against model tokens;
// unsupported/polyphonic words are never silently accepted as a working wakeword.
@_cdecl("hm_voice_pinyin") public func hmVoicePinyin(_ text: UnsafePointer<CChar>, _ output: UnsafeMutablePointer<CChar>, _ capacity: Int32) -> Int32 {
    let value = String(cString: text).applyingTransform(.toLatin, reverse: false) ?? ""
    let bytes = Array(value.utf8CString)
    guard bytes.count <= Int(capacity) else { return 1 }
    output.update(from: bytes, count: bytes.count)
    return 0
}
