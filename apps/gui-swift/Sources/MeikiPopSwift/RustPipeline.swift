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
    case show(entries: [LookupEntry], kanji: LookupKanji?)
    case hide
    case error(String)
}

private struct WireEvent: Decodable {
    let kind: String
    let entries: [LookupEntry]?
    let kanji: LookupKanji?
    let message: String?

    private enum CodingKeys: String, CodingKey {
        case kind = "type"
        case entries
        case kanji
        case message
    }
}

private struct PipelineStartError: LocalizedError {
    let message: String

    var errorDescription: String? { message }
}

@MainActor
final class RustPipeline {
    private let decoder: JSONDecoder
    private var handle: OpaquePointer?

    init(dictionaryPath: String) throws {
        var errorPointer: UnsafeMutablePointer<CChar>?
        handle = dictionaryPath.withCString { path in
            meikipop_pipeline_start(path, &errorPointer)
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

    func setPopupVisible(_ visible: Bool) {
        guard let handle else { return }
        meikipop_pipeline_set_popup_visible(handle, visible)
    }
}
