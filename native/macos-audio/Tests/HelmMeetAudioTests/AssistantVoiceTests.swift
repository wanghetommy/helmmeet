import AVFoundation

import XCTest
@testable import HelmMeetAudio
final class AssistantVoiceTests: XCTestCase {
    func testPCMRejectsInvalidInputWithoutOpeningAudioDevices() {
        let speaker = AssistantSpeaker()
        for pcm: [Float] in [[], [.nan], [.infinity], [1.1], Array(repeating: 0, count: 64001)] {
            XCTAssertEqual(speaker.playPCM(pcm), 1)
            XCTAssertEqual(speaker.status, 0)
            XCTAssertNil(speaker.engine)
        }
    }
    func testOptInProvidedWakePCMPlaysEntireClipAndProvidesReference() throws {
        guard ProcessInfo.processInfo.environment["HELMMEET_TEST_SYNTHETIC_PLAYBACK"] == "1" else { throw XCTSkip("Explicit product prompt playback only; no microphone") }
        let native = URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
        let file = try AVAudioFile(forReading: native.appendingPathComponent("voice-assistant/assets/wake-acknowledgement.wav"))
        let buffer = AVAudioPCMBuffer(pcmFormat: file.processingFormat, frameCapacity: AVAudioFrameCount(file.length))!
        try file.read(into: buffer)
        XCTAssertEqual(buffer.format.sampleRate, 16000)
        XCTAssertEqual(buffer.format.channelCount, 1)
        let pcm = Array(UnsafeBufferPointer(start: buffer.floatChannelData![0], count: Int(buffer.frameLength)))
        XCTAssertEqual(pcm.count, 16384)
        let done = expectation(description: "provided prompt completes once")
        var speaker: AssistantSpeaker?
        DispatchQueue.main.async {
            speaker = AssistantSpeaker()
            XCTAssertEqual(speaker!.playPCM(pcm), 0)
            XCTAssertEqual(speaker!.reference, pcm)
            XCTAssertGreaterThan(speaker!.referenceStart, 0)
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.5) { XCTAssertEqual(speaker!.status, 2) }
            DispatchQueue.main.asyncAfter(deadline: .now() + Double(pcm.count) / 16000 + 0.8) {
                XCTAssertEqual(speaker!.status, 0)
                XCTAssertNil(speaker!.player)
                done.fulfill()
            }
        }
        wait(for: [done], timeout: 8)
        speaker?.stop()
    }
    func testOptInSyntheticPlaybackRemainsActiveUntilWholeUtteranceFinishes() throws {
        guard ProcessInfo.processInfo.environment["HELMMEET_TEST_SYNTHETIC_PLAYBACK"] == "1" else { throw XCTSkip("Explicit synthetic playback only; no microphone") }
        guard AVSpeechSynthesisVoice.speechVoices().contains(where: { $0.language == "zh-CN" }) else { throw XCTSkip("No Chinese voice") }
        let done = expectation(description: "two complete synthetic utterances")
        var speaker: AssistantSpeaker?
        DispatchQueue.main.async {
            speaker = AssistantSpeaker()
            func playNext(_ texts: [String]) {
                guard let text = texts.first else { done.fulfill(); return }
                XCTAssertEqual(speaker!.speak(text, renderOnly: { pcm in
                    let duration = Double(pcm.count) / 16000
                    XCTAssertGreaterThan(duration, 0.4)
                    speaker!.play(pcm, speaker!.generation)
                    XCTAssertEqual(speaker!.status, 2)
                    DispatchQueue.main.asyncAfter(deadline: .now() + duration * 0.5) {
                        XCTAssertEqual(speaker!.status, 2, "Playback stopped before the end of the utterance")
                    }
                    DispatchQueue.main.asyncAfter(deadline: .now() + duration + 0.8) {
                        XCTAssertEqual(speaker!.status, 0, "Playback did not complete")
                        XCTAssertNil(speaker!.player)
                        playNext(Array(texts.dropFirst()))
                    }
                }), 0)
            }
            playNext(["小舵在", "你希望先讨论哪个目标？"])
        }
        wait(for: [done], timeout: 25)
        speaker?.stop()
    }
    func testWakeAcknowledgementRendersAndCachesTheEntireBufferWithoutPlayback() throws {
        guard AVSpeechSynthesisVoice.speechVoices().contains(where: { $0.language == "zh-CN" }) else { throw XCTSkip("No installed Chinese system voice") }
        let ready = expectation(description: "complete acknowledgement and cached replay")
        var speaker: AssistantSpeaker?
        DispatchQueue.main.async {
            speaker = AssistantSpeaker()
            XCTAssertEqual(speaker!.speak("小舵在", renderOnly: { pcm in
                XCTAssertGreaterThan(pcm.count, 6400, "Acknowledgement must contain more than a clipped first syllable")
                XCTAssertLessThan(pcm.count, 16000 * 4)
                XCTAssertTrue(pcm.allSatisfy { $0.isFinite })
                XCTAssertEqual(speaker!.speak("小舵在", renderOnly: { cached in
                    XCTAssertEqual(cached, pcm)
                    XCTAssertNil(speaker!.engine)
                    XCTAssertNil(speaker!.player)
                    ready.fulfill()
                }), 0)
            }), 0)
        }
        wait(for: [ready], timeout: 20)
        speaker?.stop()
    }
    func testPinyinProducesConfiguredDefaultWithoutAudioDevices() {
        var buffer = [CChar](repeating: 0, count: 512)
        let code = "小舵同学".withCString { hmVoicePinyin($0, &buffer, 512) }
        XCTAssertEqual(code, 0)
        XCTAssertEqual(String(cString: buffer), "xiǎo duò tóng xué")
    }
    func testSystemChineseSpeechRendersPCMWithoutPlaybackOrCapture() throws {
        guard AVSpeechSynthesisVoice.speechVoices().contains(where: { $0.language == "zh-CN" }) else { throw XCTSkip("No installed Chinese system voice") }
        let ready = expectation(description: "local speech buffer")
        var speaker: AssistantSpeaker?
        DispatchQueue.main.async {
            speaker = AssistantSpeaker()
            XCTAssertEqual(speaker!.speak("小舵同学，目前有哪些目标没有澄清？", renderOnly: { pcm in
                XCTAssertGreaterThan(pcm.count, 1600)
                XCTAssertLessThan(pcm.count, 16000 * 45)
                XCTAssertGreaterThan(pcm.reduce(Float(0)) { $0 + $1 * $1 }, 0.01)
                XCTAssertTrue(pcm.allSatisfy { $0.isFinite && abs($0) <= 1.01 })
                if let output = ProcessInfo.processInfo.environment["HELMMEET_SYNTHETIC_VOICE_WAV"] {
                    var wav=Data()
                    func append<T: FixedWidthInteger>(_ value:T){var n=value.littleEndian;withUnsafeBytes(of:&n){wav.append(contentsOf:$0)}}
                    wav.append(contentsOf:Array("RIFF".utf8));append(UInt32(36+pcm.count*2));wav.append(contentsOf:Array("WAVEfmt ".utf8));append(UInt32(16));append(UInt16(1));append(UInt16(1));append(UInt32(16000));append(UInt32(32000));append(UInt16(2));append(UInt16(16));wav.append(contentsOf:Array("data".utf8));append(UInt32(pcm.count*2))
                    for sample in pcm {append(Int16(max(-32767,min(32767,Int(sample*32767)))))}
                    do {try wav.write(to:URL(fileURLWithPath:output),options:.withoutOverwriting)}catch{XCTFail("Synthetic WAV write failed: \(error)")}
                }
                XCTAssertTrue(speaker?.engine == nil)
                XCTAssertTrue(speaker?.player == nil)
                ready.fulfill()
            }), 0)
        }
        wait(for: [ready], timeout: 20)
        speaker?.stop()
    }
}
