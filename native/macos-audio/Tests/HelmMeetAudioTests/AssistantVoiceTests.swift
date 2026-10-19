import AVFoundation

import XCTest
@testable import HelmMeetAudio
final class AssistantVoiceTests: XCTestCase {
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
