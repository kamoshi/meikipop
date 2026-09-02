import CMeikiPopFFI
import Foundation

struct LookupSense: Decodable {
    let glosses: [String]
    let pos: [String]
    let tags: [String]
}

struct LookupEntry: Decodable {
    let writtenForm: String?
    let reading: String
    let senses: [LookupSense]
    let freq: Int64
    let deconjugationProcess: [String]
}

struct LookupKanji: Decodable {
    let character: String
    let meanings: [String]
    let readings: [String]
}

enum RustPipelineEvent {
    case captureReady
    case ocrProviders(
        providers: [OCRProviderOption],
        activeProvider: String,
        error: String?
    )
    case show(entries: [LookupEntry], kanji: LookupKanji?)
    case hide
    case error(String)
}

struct PopupCaptureBounds {
    let left: Int32
    let top: Int32
    let width: UInt32
    let height: UInt32
}

private struct WireEvent: Decodable {
    let kind: String
    let entries: [LookupEntry]?
    let kanji: LookupKanji?
    let message: String?
    let providers: [OCRProviderOption]?
    let activeProvider: String?
    let error: String?

    private enum CodingKeys: String, CodingKey {
        case kind = "type"
        case entries
        case kanji
        case message
        case providers
        case activeProvider
        case error
    }
}

private struct PipelineStartError: LocalizedError {
    let message: String

    var errorDescription: String? { message }
}

private struct RustCoreConfiguration: Encodable {
    let ocrProvider: String
}

@MainActor
final class RustPipeline {
    private let decoder: JSONDecoder
    private var handle: OpaquePointer?

    init(dictionaryPath: String, ocrProvider: String) throws {
        meikipop_logging_init()

        var errorPointer: UnsafeMutablePointer<CChar>?
        let configJSON = try Self.encodeConfiguration(ocrProvider: ocrProvider)
        handle = dictionaryPath.withCString { path in
            configJSON.withCString { config in
                meikipop_pipeline_start(path, config, &errorPointer)
            }
        }

        guard handle != nil else {
            let message = errorPointer.map { String(cString: $0) }
                ?? "Rust returned no startup error"
            if let errorPointer {
                meikipop_string_free(errorPointer)
            }
            throw PipelineStartError(message: message)
        }

        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        self.decoder = decoder
    }

    deinit {
        if let handle {
            meikipop_pipeline_destroy(handle)
        }
    }

    func poll() -> RustPipelineEvent? {
        guard let handle, let eventPointer = meikipop_pipeline_poll(handle) else {
            return nil
        }
        defer { meikipop_string_free(eventPointer) }

        do {
            let data = Data(bytes: eventPointer, count: strlen(eventPointer))
            let event = try decoder.decode(WireEvent.self, from: data)
            switch event.kind {
            case "capture_ready":
                return .captureReady
            case "ocr_providers":
                guard let activeProvider = event.activeProvider else {
                    return .error("Native OCR provider event omitted its active provider")
                }
                return .ocrProviders(
                    providers: event.providers ?? [],
                    activeProvider: activeProvider,
                    error: event.error
                )
            case "show":
                return .show(entries: event.entries ?? [], kanji: event.kanji)
            case "hide":
                return .hide
            case "error":
                return .error(event.message ?? "Unknown native error")
            default:
                return .error("Unknown native event: \(event.kind)")
            }
        } catch {
            return .error("Could not decode native event: \(error.localizedDescription)")
        }
    }

    func updateConfiguration(ocrProvider: String) {
        guard let handle else { return }
        guard let configJSON = try? Self.encodeConfiguration(ocrProvider: ocrProvider) else {
            NSLog("Could not encode Rust core configuration")
            return
        }
        let queued = configJSON.withCString {
            meikipop_pipeline_update_config(handle, $0)
        }
        if !queued {
            NSLog("Could not queue Rust core configuration")
        }
    }

    private static func encodeConfiguration(ocrProvider: String) throws -> String {
        let encoder = JSONEncoder()
        encoder.keyEncodingStrategy = .convertToSnakeCase
        let data = try encoder.encode(RustCoreConfiguration(ocrProvider: ocrProvider))
        guard let json = String(data: data, encoding: .utf8) else {
            throw PipelineStartError(message: "Could not encode Rust core configuration as UTF-8")
        }
        return json
    }

    func setPopupBounds(_ bounds: PopupCaptureBounds?) {
        guard let handle else { return }
        meikipop_pipeline_set_popup_bounds(
            handle,
            bounds != nil,
            bounds?.left ?? 0,
            bounds?.top ?? 0,
            bounds?.width ?? 0,
            bounds?.height ?? 0
        )
    }
}
