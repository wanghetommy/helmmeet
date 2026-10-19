import AVFoundation
import ScreenCaptureKit
import CoreMedia
import Darwin
import CoreAudio
import AudioToolbox
import AppKit

public typealias AudioCallback = @convention(c) (UnsafePointer<Float>?, Int32, Int32, UInt64) -> Void

private final class BufferPool {
    let lock = NSLock()
    var buffers: [AVAudioPCMBuffer]
    init(_ format: AVAudioFormat) {
        buffers = (0..<24).compactMap { _ in AVAudioPCMBuffer(pcmFormat: format, frameCapacity: 8192) }
    }
    func take() -> AVAudioPCMBuffer? {
        guard lock.try() else { return nil }
        defer { lock.unlock() }
        return buffers.popLast()
    }
    func put(_ buffer: AVAudioPCMBuffer) {
        lock.lock(); buffers.append(buffer); lock.unlock()
    }
}

// AudioConverter may keep the memory returned by its input block until a later
// conversion call. Never lend a tap's pooled buffer: it is refilled as soon as
// deliver() returns. Keep the last loan alive until the converter requests more
// input, and supply only the requested number of frames.
private final class PacketConverter {
    let converter: AVAudioConverter
    var loan: AVAudioPCMBuffer?
    init?(_ input: AVAudioFormat, _ output: AVAudioFormat) {
        guard let converter = AVAudioConverter(from: input, to: output) else { return nil }
        self.converter = converter
    }
    func convert(_ input: AVAudioPCMBuffer) -> [Float]? {
        var offset: AVAudioFrameCount = 0
        var result: [Float] = []
        let capacity = AVAudioFrameCount(ceil(Double(input.frameLength) * converter.outputFormat.sampleRate / input.format.sampleRate)) + 64
        while true {
            guard let output = AVAudioPCMBuffer(pcmFormat: converter.outputFormat, frameCapacity: capacity) else { return nil }
            var error: NSError?
            var allocationFailed = false
            let status = converter.convert(to: output, error: &error) { requested, status in
                guard offset < input.frameLength else { status.pointee = .noDataNow; return nil }
                let count = min(AVAudioFrameCount(requested), input.frameLength - offset)
                guard count > 0, let owned = AVAudioPCMBuffer(pcmFormat: input.format, frameCapacity: count) else {
                    allocationFailed = true; status.pointee = .noDataNow; return nil
                }
                owned.frameLength = count
                let source = UnsafeMutableAudioBufferListPointer(input.mutableAudioBufferList)
                let destination = UnsafeMutableAudioBufferListPointer(owned.mutableAudioBufferList)
                let stride = Int(input.format.streamDescription.pointee.mBytesPerFrame)
                for channel in 0..<source.count {
                    memcpy(destination[channel].mData!, source[channel].mData!.advanced(by: Int(offset) * stride), Int(count) * stride)
                }
                self.loan = owned
                offset += count
                status.pointee = .haveData
                return owned
            }
            guard error == nil, status != .error, !allocationFailed else { return nil }
            if output.frameLength > 0 {
                result.append(contentsOf: UnsafeBufferPointer(start: output.floatChannelData![0], count: Int(output.frameLength)))
            }
            if status == .inputRanDry || status == .endOfStream { return result }
            // Drain full output buffers before returning the pooled input.
            if output.frameLength == 0 { return nil }
        }
    }
}

final class Recorder: NSObject, SCStreamOutput, SCStreamDelegate {
    let queue = DispatchQueue(label: "com.helmmeet.audio", qos: .userInitiated)
    var engine: AVAudioEngine?
    var microphoneDevice: UInt32 = 0
    var micObserver: NSObjectProtocol?
    let micCallbacks = DispatchGroup()
    var stream: SCStream?
    var callback: AudioCallback?
    var observers: [NSObjectProtocol] = []
    private var converters: [Int32: PacketConverter] = [:]
    let target = AVAudioFormat(commonFormat: .pcmFormatFloat32, sampleRate: 16000, channels: 1, interleaved: false)!

    func start(_ mask: Int32, _ device: UInt32, _ output: @escaping AudioCallback) -> Int32 {
        guard callback == nil, mask > 0 && mask <= 3 else { return 4 }
        callback = output
        microphoneDevice = device
        if mask & 1 != 0 {
            if microphoneDevice == 0 {
                var address = AudioObjectPropertyAddress(mSelector: kAudioHardwarePropertyDefaultInputDevice, mScope: kAudioObjectPropertyScopeGlobal, mElement: kAudioObjectPropertyElementMain)
                var size = UInt32(MemoryLayout<UInt32>.size)
                guard AudioObjectGetPropertyData(AudioObjectID(kAudioObjectSystemObject), &address, 0, nil, &size, &microphoneDevice) == noErr, microphoneDevice != 0 else { stop(); return 2 }
            }
            guard requestMicrophonePermission() else { stop(); return 1 }
        }
        if mask & 2 != 0 {
            let sem = DispatchSemaphore(value: 0)
            var result: Int32 = 0
            Task {
                do {
                    let content = try await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: true)
                    guard let display = content.displays.first else { result = 3; sem.signal(); return }
                    let filter = SCContentFilter(display: display, excludingWindows: [])
                    let config = SCStreamConfiguration()
                    config.capturesAudio = true
                    config.excludesCurrentProcessAudio = true
                    config.sampleRate = 48000
                    config.channelCount = 2
                    config.width = 2; config.height = 2
                    let capture = SCStream(filter: filter, configuration: config, delegate: self)
                    try capture.addStreamOutput(self, type: .audio, sampleHandlerQueue: queue)
                    stream = capture
                } catch { result = 3 }
                sem.signal()
            }
            sem.wait()
            if result != 0 { stop(); return result }
        }
        if mask & 1 != 0 {
            let code = startMicrophone(microphoneDevice)
            if code != 0 { stop(); return code }
        }
        if let capture = stream {
            let sem = DispatchSemaphore(value: 0)
            var result: Int32 = 0
            Task { do { try await capture.startCapture() } catch { result = 3 }; sem.signal() }
            sem.wait()
            if result != 0 { stop(); return result }
        }
        observers.append(NSWorkspace.shared.notificationCenter.addObserver(forName: NSWorkspace.willSleepNotification, object: nil, queue: nil) { [weak self] _ in self?.callback?(nil, -1, 7, 0) })
        return 0
    }

    func requestMicrophonePermission() -> Bool {
        var granted = AVCaptureDevice.authorizationStatus(for: .audio) == .authorized
        if AVCaptureDevice.authorizationStatus(for: .audio) == .notDetermined {
            let sem = DispatchSemaphore(value: 0)
            AVCaptureDevice.requestAccess(for: .audio) { allowed in granted = allowed; sem.signal() }
            sem.wait()
        }
        return granted
    }

    func startMicrophone(_ device: UInt32) -> Int32 {
        guard engine == nil, let output = callback else { return engine == nil ? 4 : 0 }
        guard requestMicrophonePermission() else { return 1 }
        let audio = AVAudioEngine()
        let input = audio.inputNode
        if device > 0 {
            guard let unit = input.audioUnit else { return 2 }
            var chosen = device
            guard AudioUnitSetProperty(unit, kAudioOutputUnitProperty_CurrentDevice, kAudioUnitScope_Global, 0, &chosen, UInt32(MemoryLayout<UInt32>.size)) == noErr else { return 2 }
        }
        // CurrentDevice can leave the output bus with the old aggregate-device
        // format (e.g. 44.1 kHz while the selected mic is 48 kHz). The input bus
        // describes the hardware; an input node cannot resample at the tap.
        // Convert to 16 kHz only in deliver(), after receiving hardware frames.
        let format = input.inputFormat(forBus: 0)
        guard format.sampleRate.isFinite, format.sampleRate > 0, format.channelCount > 0 else { return 2 }
        let pool = BufferPool(format)
        input.installTap(onBus: 0, bufferSize: 2048, format: format) { [weak self] buffer, time in
            guard let self = self else { return }
            self.micCallbacks.enter()
            defer { self.micCallbacks.leave() }
            guard let copy = pool.take() else { output(nil, -2, 5, 0); return }
            guard buffer.format == format else {
                pool.put(copy); output(nil, -2, 6, 0); return
            }
            guard buffer.frameLength <= copy.frameCapacity else {
                pool.put(copy); output(nil, -2, 5, 0); return
            }
            copy.frameLength = buffer.frameLength
            let src = UnsafeMutableAudioBufferListPointer(buffer.mutableAudioBufferList)
            let dst = UnsafeMutableAudioBufferListPointer(copy.mutableAudioBufferList)
            for i in 0..<src.count {
                memcpy(dst[i].mData!, src[i].mData!, Int(src[i].mDataByteSize))
                dst[i].mDataByteSize = src[i].mDataByteSize
            }
            let ns = UInt64(AVAudioTime.seconds(forHostTime: time.hostTime) * 1_000_000_000)
            self.queue.async { self.deliver(copy, source: 0, ns: ns); pool.put(copy) }
        }
        audio.prepare()
        do { try audio.start() } catch { input.removeTap(onBus: 0); return 2 }
        engine = audio
        micObserver = NotificationCenter.default.addObserver(forName: .AVAudioEngineConfigurationChange, object: audio, queue: nil) { [weak self, weak audio] _ in
            guard let audio = audio else { return }
            // CurrentDevice's asynchronous startup notification may arrive after
            // start() even though this exact device/format is already running.
            // Do not turn a healthy input into a fault just because it notified.
            var actualDevice: UInt32 = 0
            var size = UInt32(MemoryLayout<UInt32>.size)
            let status = input.audioUnit.map {
                AudioUnitGetProperty($0, kAudioOutputUnitProperty_CurrentDevice, kAudioUnitScope_Global, 0, &actualDevice, &size)
            }
            if audio.isRunning, status == noErr, actualDevice == device,
               input.inputFormat(forBus: 0) == format { return }
            self?.callback?(nil, -2, 7, 0)
        }
        return 0
    }
    func stopMicrophone() {
        if let observer = micObserver { NotificationCenter.default.removeObserver(observer); micObserver = nil }
        if let audio = engine { audio.stop(); audio.inputNode.removeTap(onBus: 0); engine = nil }
        micCallbacks.wait()
        // Stop I/O before draining converted frames; nothing from the muted mic survives return.
        queue.sync { converters.removeValue(forKey: 0) }
    }
    func setMicrophone(_ enabled: Bool) -> Int32 {
        guard callback != nil else { return 4 }
        if enabled { return startMicrophone(microphoneDevice) }
        stopMicrophone(); return 0
    }

    func stopSystem() {
        if let capture = stream {
            let sem = DispatchSemaphore(value: 0)
            capture.stopCapture { _ in sem.signal() }
            sem.wait(); stream = nil
        }
        queue.sync { converters.removeValue(forKey: 1) }
    }
    func stopSource(_ source: Int32) { if source == 0 { stopMicrophone() } else { stopSystem() } }

    func stop() {
        for observer in observers { NotificationCenter.default.removeObserver(observer); NSWorkspace.shared.notificationCenter.removeObserver(observer) }
        observers.removeAll()
        stopMicrophone()
        stopSystem()
        queue.sync { converters.removeAll(); callback = nil }
    }

    func deliver(_ buffer: AVAudioPCMBuffer, source: Int32, ns: UInt64) {
        if converters[source] == nil || converters[source]!.converter.inputFormat != buffer.format {
            converters[source] = PacketConverter(buffer.format, target)
        }
        guard let converted = converters[source]?.convert(buffer) else {
            callback?(nil, source == 0 ? -2 : -3, 6, ns); return
        }
        if !converted.isEmpty {
            converted.withUnsafeBufferPointer { callback?($0.baseAddress, source, Int32($0.count), ns) }
        }
    }

    func stream(_ stream: SCStream, didOutputSampleBuffer sampleBuffer: CMSampleBuffer, of type: SCStreamOutputType) {
        guard type == .audio, sampleBuffer.isValid,
              let desc = sampleBuffer.formatDescription else { return }
        let format = AVAudioFormat(cmAudioFormatDescription: desc)
        let count = CMSampleBufferGetNumSamples(sampleBuffer)
        guard let pcm = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: AVAudioFrameCount(count)) else { return }
        pcm.frameLength = AVAudioFrameCount(count)
        let status = CMSampleBufferCopyPCMDataIntoAudioBufferList(sampleBuffer, at: 0, frameCount: Int32(count), into: pcm.mutableAudioBufferList)
        guard status == noErr else { callback?(nil, -3, 6, 0); return }
        let seconds = CMSampleBufferGetPresentationTimeStamp(sampleBuffer).seconds
        guard seconds.isFinite && seconds >= 0 else { return }
        deliver(pcm, source: 1, ns: UInt64(seconds * 1_000_000_000))
    }
    func stream(_ stream: SCStream, didStopWithError error: Error) { callback?(nil, -3, 7, 0) }
}

private let recorder = Recorder()
@_cdecl("hm_audio_abi_version") public func hmAudioAbiVersion() -> UInt32 { 2 }
@_cdecl("hm_audio_capture_available") public func hmAudioCaptureAvailable() -> Int32 { 1 }
@_cdecl("hm_audio_host_ns") public func hmAudioHostNs() -> UInt64 {
    UInt64(AVAudioTime.seconds(forHostTime: mach_absolute_time()) * 1_000_000_000)
}
@_cdecl("hm_audio_start") public func hmAudioStart(_ mask: Int32, _ device: UInt32, _ callback: @escaping AudioCallback) -> Int32 {
    recorder.start(mask, device, callback)
}
@_cdecl("hm_audio_stop") public func hmAudioStop() { recorder.stop() }
@_cdecl("hm_audio_input_device") public func hmAudioInputDevice() -> UInt32 { recorder.microphoneDevice }
@_cdecl("hm_audio_stop_source") public func hmAudioStopSource(_ source: Int32) { recorder.stopSource(source) }
@_cdecl("hm_audio_set_microphone_enabled") public func hmAudioSetMicrophoneEnabled(_ enabled: Int32) -> Int32 { recorder.setMicrophone(enabled != 0) }
// The explicit CLI microphone check has no WebView event loop. Keep AppKit's
// main-thread notifications running while its worker uses the normal preview.
@_cdecl("hm_audio_pump_events") public func hmAudioPumpEvents() {
    RunLoop.current.run(until: Date(timeIntervalSinceNow: 0.02))
}

@_cdecl("hm_audio_devices") public func hmAudioDevices() -> UnsafeMutablePointer<CChar>? {
    var address = AudioObjectPropertyAddress(mSelector: kAudioHardwarePropertyDevices, mScope: kAudioObjectPropertyScopeGlobal, mElement: kAudioObjectPropertyElementMain)
    var size: UInt32 = 0
    guard AudioObjectGetPropertyDataSize(AudioObjectID(kAudioObjectSystemObject), &address, 0, nil, &size) == noErr else { return nil }
    var devices = [AudioDeviceID](repeating: 0, count: Int(size) / MemoryLayout<AudioDeviceID>.size)
    let status = devices.withUnsafeMutableBytes { AudioObjectGetPropertyData(AudioObjectID(kAudioObjectSystemObject), &address, 0, nil, &size, $0.baseAddress!) }
    guard status == noErr else { return nil }
    var output: [[String: Any]] = []
    for device in devices {
        var streams = AudioObjectPropertyAddress(mSelector: kAudioDevicePropertyStreams, mScope: kAudioDevicePropertyScopeInput, mElement: kAudioObjectPropertyElementMain)
        var streamSize: UInt32 = 0
        if AudioObjectGetPropertyDataSize(device, &streams, 0, nil, &streamSize) != noErr || streamSize == 0 { continue }
        var nameAddress = AudioObjectPropertyAddress(mSelector: kAudioObjectPropertyName, mScope: kAudioObjectPropertyScopeGlobal, mElement: kAudioObjectPropertyElementMain)
        var name: CFString = "麦克风" as CFString
        var nameSize = UInt32(MemoryLayout<CFString>.size)
        _ = AudioObjectGetPropertyData(device, &nameAddress, 0, nil, &nameSize, &name)
        output.append(["id": device, "name": name as String])
    }
    guard let data = try? JSONSerialization.data(withJSONObject: output), let json = String(data: data, encoding: .utf8) else { return nil }
    return strdup(json)
}
@_cdecl("hm_audio_free") public func hmAudioFree(_ pointer: UnsafeMutablePointer<CChar>?) { free(pointer) }


// Read-only route probe, called on the Rust writer, never from the audio callback.
// Three UInt64 outputs: device, transport, nominal sample rate. No permission prompt.
@_cdecl("hm_audio_output_route") public func hmAudioOutputRoute(_ values: UnsafeMutablePointer<UInt64>?) -> Int32 {
    guard let values = values else { return -1 }
    var device: AudioDeviceID = 0
    var size = UInt32(MemoryLayout<AudioDeviceID>.size)
    var address = AudioObjectPropertyAddress(mSelector: kAudioHardwarePropertyDefaultOutputDevice, mScope: kAudioObjectPropertyScopeGlobal, mElement: kAudioObjectPropertyElementMain)
    guard AudioObjectGetPropertyData(AudioObjectID(kAudioObjectSystemObject), &address, 0, nil, &size, &device) == noErr, device != 0 else { return -1 }
    var transport: UInt32 = 0
    size = UInt32(MemoryLayout<UInt32>.size)
    address.mSelector = kAudioDevicePropertyTransportType
    guard AudioObjectGetPropertyData(device, &address, 0, nil, &size, &transport) == noErr else { return -1 }
    var rate: Float64 = 0
    size = UInt32(MemoryLayout<Float64>.size)
    address.mSelector = kAudioDevicePropertyNominalSampleRate
    guard AudioObjectGetPropertyData(device, &address, 0, nil, &size, &rate) == noErr, rate.isFinite, rate > 0, rate < 1000000 else { return -1 }
    values[0] = UInt64(device); values[1] = UInt64(transport); values[2] = UInt64(rate.rounded())
    return 0
}
