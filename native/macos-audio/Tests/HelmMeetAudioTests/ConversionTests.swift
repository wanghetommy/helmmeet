import XCTest
import AVFoundation
@testable import HelmMeetAudio

// Pure synthetic conversion: no AVAudioEngine start, microphone, ScreenCaptureKit or permissions.
final class ConversionTests: XCTestCase {
    static var output: [Float] = []
    static var boundaries: [Int] = []
    static var faults = 0
    func testStreamedConversionMatchesContinuousInput() throws {
        var maximumDifference: Float = 0
        for rate in [44100.0, 48000.0] {
            for channels: AVAudioChannelCount in [1, 2] {
                for interleaved in [false, true] {
                    let frames = Int(rate * 6)
                    let format = AVAudioFormat(commonFormat: .pcmFormatFloat32, sampleRate: rate, channels: channels, interleaved: interleaved)!
                    let source: Int32 = channels == 1 ? 0 : 1
                    func convert(packet: Int, reuse: Bool = false) -> [Float] {
                        Self.output = []; Self.boundaries = []; Self.faults = 0
                        let recorder = Recorder()
                        recorder.callback = { data, actualSource, count, _ in
                            guard actualSource >= 0, count > 0, let data = data else { ConversionTests.faults += 1; return }
                            ConversionTests.output.append(contentsOf: UnsafeBufferPointer(start: data, count: Int(count)))
                        }
                        let reusable = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: AVAudioFrameCount(packet))!
                        var position = 0
                        while position < frames {
                            let count = min(packet, frames - position)
                            let pcm = reuse ? reusable : AVAudioPCMBuffer(pcmFormat: format, frameCapacity: AVAudioFrameCount(count))!
                            pcm.frameLength = AVAudioFrameCount(count)
                            for n in 0..<count {
                                let value = Float(0.3 * sin(2 * .pi * 997 * Double(position + n) / rate))
                                for channel in 0..<Int(channels) {
                                    pcm.floatChannelData![interleaved ? 0 : channel][interleaved ? n * Int(channels) + channel : n] = value
                                }
                            }
                            // Mimic the LIFO tap pool: refill the same storage on the next call.
                            recorder.deliver(pcm, source: source, ns: UInt64(Double(position) / rate * 1_000_000_000))
                            position += count
                        }
                        XCTAssertEqual(Self.faults, 0)
                        return Self.output
                    }
                    let reference = convert(packet: frames)
                    XCTAssertGreaterThan(reference.count, 95872)
                    for packet in [Int(rate / 10), 2048, 511] {
                        let streamed = convert(packet: packet, reuse: true)
                        // A finite stream can retain a short converter tail. Its common
                        // prefix must be sample-identical regardless of packet storage.
                        XCTAssertLessThan(abs(streamed.count - reference.count), 128)
                        let difference = zip(streamed, reference).map { abs($0 - $1) }.max() ?? 0
                        maximumDifference = max(maximumDifference, difference)
                        XCTAssertLessThan(difference, 0.00002, "Reused packet \(packet) corrupted \(rate)Hz / \(channels)ch / interleaved=\(interleaved)")
                    }
                }
            }
        }
        print("Synthetic pooled-input vs continuous conversion maximum error: \(maximumDifference)")
    }
    func testPacketConversionHasNoArtificialClicks() throws {
        for rate in [44100.0, 48000.0] {
            for channels: AVAudioChannelCount in [1, 2] {
                Self.output = []; Self.boundaries = []; Self.faults = 0
                let recorder = Recorder()
                recorder.callback = { data, source, count, _ in
                    guard source == 0, count > 0, let data = data else { ConversionTests.faults += 1; return }
                    ConversionTests.boundaries.append(ConversionTests.output.count)
                    ConversionTests.output.append(contentsOf: UnsafeBufferPointer(start: data, count: Int(count)))
                }
                let format = AVAudioFormat(commonFormat: .pcmFormatFloat32, sampleRate: rate, channels: channels, interleaved: false)!
                var position = 0
                for i in 0..<96 {
                    let count = [2048, 1024, 511, 137][i % 4]
                    let pcm = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: AVAudioFrameCount(count))!
                    pcm.frameLength = AVAudioFrameCount(count)
                    for channel in 0..<Int(channels) {
                        for n in 0..<count { pcm.floatChannelData![channel][n] = Float(0.3 * sin(2 * .pi * 997 * Double(position + n) / rate)) }
                    }
                    recorder.deliver(pcm, source: 0, ns: UInt64(Double(position) / rate * 1_000_000_000))
                    position += count
                }
                XCTAssertEqual(Self.faults, 0)
                XCTAssertGreaterThan(Self.output.count, 16000)
                XCTAssertLessThan(abs(Double(Self.output.count) - Double(position) * 16000 / rate), 128)
                let jumps = Self.boundaries.filter { $0 > 128 && $0 < Self.output.count }.map { abs(Self.output[$0] - Self.output[$0 - 1]) }
                XCTAssertLessThan(jumps.max() ?? 0, 0.14, "Conversion introduced a packet click at \(rate)Hz / \(channels) channels")
                XCTAssertTrue(Self.output.allSatisfy { $0.isFinite && abs($0) < 0.4 })
            }
        }
    }
}
