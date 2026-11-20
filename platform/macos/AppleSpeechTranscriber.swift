import AVFoundation
import CoreMedia
import Foundation
import Speech

private struct SpeechFailure: LocalizedError {
    let errorDescription: String?

    init(_ message: String) {
        errorDescription = message
    }
}

@main
struct AppleSpeechTranscriber {
    static func main() async {
        do {
            guard #available(macOS 26.0, *) else {
                throw SpeechFailure("Apple SpeechTranscriber requires macOS 26 or later")
            }
            let args = Array(CommandLine.arguments.dropFirst())
            guard args.count == 2, args[0] == "--install" || args[0] == "--transcribe" else {
                throw SpeechFailure("Usage: selvedge-apple-speech --install <language>")
            }
            try await run(install: args[0] == "--install", language: args[1])
        } catch {
            try? send(["type": "error", "message": error.localizedDescription])
            exit(EXIT_FAILURE)
        }
    }

    @available(macOS 26.0, *)
    private static func run(install: Bool, language: String) async throws {
        guard SpeechTranscriber.isAvailable else {
            throw SpeechFailure("Apple SpeechTranscriber is unavailable on this device")
        }
        let identifier = switch language {
        case "auto": Locale.preferredLanguages.first ?? Locale.current.identifier
        case "zh": "zh-CN"
        case "en": "en-US"
        default: language
        }
        let requested = Locale(identifier: identifier)
        let locales = await SpeechTranscriber.supportedLocales
        let locale = await SpeechTranscriber.supportedLocale(equivalentTo: requested)
            ?? (requested.region == nil
                ? locales.first { $0.language.languageCode == requested.language.languageCode }
                : nil)
        guard let locale else {
            throw SpeechFailure("Apple SpeechTranscriber does not support \(requested.identifier)")
        }
        let module = SpeechTranscriber(locale: locale, preset: .transcription)
        if install {
            if let request = try await AssetInventory.assetInstallationRequest(supporting: [module]) {
                try await request.downloadAndInstall()
            }
        }
        guard await AssetInventory.status(forModules: [module]) == .installed else {
            throw SpeechFailure(
                "Apple speech model for \(locale.identifier) is unavailable. "
                + "Run selvedge-apple-speech --install \(locale.identifier) from the app bundle."
            )
        }
        if install {
            try send(["type": "installed", "locale": locale.identifier])
        } else {
            try await transcribe(module)
        }
    }

    @available(macOS 26.0, *)
    private static func transcribe(_ module: SpeechTranscriber) async throws {
        guard let format = await SpeechAnalyzer.bestAvailableAudioFormat(compatibleWith: [module]) else {
            throw SpeechFailure("Apple speech analyzer could not select a PCM format")
        }
        let analyzer = SpeechAnalyzer(modules: [module])
        try await analyzer.prepareToAnalyze(in: format)
        try send(["type": "ready"])

        let input = FileHandle.standardInput
        let header = input.readData(ofLength: MemoryLayout<UInt32>.size)
        if header.isEmpty { return }
        guard header.count == MemoryLayout<UInt32>.size else {
            throw SpeechFailure("Apple speech input has an incomplete sample rate")
        }
        let rate = header.withUnsafeBytes { UInt32(littleEndian: $0.loadUnaligned(as: UInt32.self)) }
        guard (8000...192000).contains(rate),
              let source = AVAudioFormat(standardFormatWithSampleRate: Double(rate), channels: 1)
        else {
            throw SpeechFailure("Apple speech input has an unsupported sample rate")
        }

        let (stream, continuation) = AsyncStream<AnalyzerInput>.makeStream()
        try await analyzer.start(inputSequence: stream)
        async let results: Void = sendResults(module)
        do {
            try feed(input, source: source, destination: format, continuation: continuation)
            continuation.finish()
            try await analyzer.finalizeAndFinishThroughEndOfInput()
            try await results
        } catch {
            continuation.finish()
            await analyzer.cancelAndFinishNow()
            throw error
        }
    }

    @available(macOS 26.0, *)
    private static func feed(
        _ file: FileHandle,
        source: AVAudioFormat,
        destination: AVAudioFormat,
        continuation: AsyncStream<AnalyzerInput>.Continuation
    ) throws {
        let converter = AVAudioConverter(from: source, to: destination)
        if source != destination && converter == nil {
            throw SpeechFailure("Apple speech input could not be converted to the analyzer format")
        }
        let framesPerRead: AVAudioFrameCount = max(1, AVAudioFrameCount(source.sampleRate / 10))
        let frameSize = MemoryLayout<Float>.size
        while true {
            let data = file.readData(ofLength: Int(framesPerRead) * frameSize)
            if data.isEmpty { break }
            guard data.count % frameSize == 0,
                  let pcm = AVAudioPCMBuffer(
                    pcmFormat: source, frameCapacity: AVAudioFrameCount(data.count / frameSize)
                  )
            else {
                throw SpeechFailure("Apple speech input has an incomplete PCM frame")
            }
            pcm.frameLength = pcm.frameCapacity
            data.copyBytes(to: UnsafeMutableRawBufferPointer(
                start: pcm.floatChannelData![0], count: data.count
            ))
            if let converter, source != destination {
                let capacity = ceil(Double(pcm.frameLength) * destination.sampleRate / source.sampleRate)
                if let converted = try convert(pcm, with: converter, capacity: AVAudioFrameCount(capacity) + 256) {
                    continuation.yield(AnalyzerInput(buffer: converted))
                }
            } else {
                continuation.yield(AnalyzerInput(buffer: pcm))
            }
        }
        if let converter, source != destination,
           let tail = try convert(nil, with: converter, capacity: framesPerRead + 256) {
            continuation.yield(AnalyzerInput(buffer: tail))
        }
    }

    private static func convert(
        _ input: AVAudioPCMBuffer?, with converter: AVAudioConverter, capacity: AVAudioFrameCount
    ) throws -> AVAudioPCMBuffer? {
        guard let output = AVAudioPCMBuffer(pcmFormat: converter.outputFormat, frameCapacity: capacity) else {
            throw SpeechFailure("Could not allocate Apple speech PCM output")
        }
        var supplied = false
        var error: NSError?
        let status = converter.convert(to: output, error: &error) { _, inputStatus in
            if let input, !supplied {
                supplied = true
                inputStatus.pointee = .haveData
                return input
            }
            inputStatus.pointee = input == nil ? .endOfStream : .noDataNow
            return nil
        }
        if status == .error {
            throw error ?? SpeechFailure("Apple speech PCM conversion failed")
        }
        return output.frameLength > 0 ? output : nil
    }

    @available(macOS 26.0, *)
    private static func sendResults(_ module: SpeechTranscriber) async throws {
        for try await result in module.results where result.isFinal {
            let text = String(result.text.characters).trimmingCharacters(in: .whitespacesAndNewlines)
            let start = result.range.start.seconds * 1000
            let end = result.range.end.seconds * 1000
            if !text.isEmpty && start.isFinite && end.isFinite && start >= 0 && end > start {
                try send(["type": "segment", "startMs": Int(start.rounded()),
                          "endMs": Int(end.rounded()), "text": text])
            }
        }
    }

    private static func send(_ message: [String: Any]) throws {
        var line = try JSONSerialization.data(withJSONObject: message)
        line.append(0x0A)
        try FileHandle.standardOutput.write(contentsOf: line)
    }
}
