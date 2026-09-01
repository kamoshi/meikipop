import CMeikiPopFFI
import Foundation

struct CaptureDisplay: Decodable, Equatable, Identifiable {
    let id: UInt32
    let top: Int
    let left: Int
    let width: Int
    let height: Int

    var selectionID: String {
        ScanAreaSelection.display(id)
    }

    func title(number: Int) -> String {
        "Screen \(number) (\(width)×\(height))"
    }
}

enum DisplayDiscovery {
    static func load() throws -> [CaptureDisplay] {
        var errorPointer: UnsafeMutablePointer<CChar>?
        guard let jsonPointer = meikipop_displays_json(&errorPointer) else {
            let message = errorPointer.map { String(cString: $0) }
                ?? "Rust returned no display discovery error"
            if let errorPointer {
                meikipop_string_free(errorPointer)
            }
            throw DisplayDiscoveryError(message: message)
        }
        defer { meikipop_string_free(jsonPointer) }

        let data = Data(bytes: jsonPointer, count: strlen(jsonPointer))
        return try JSONDecoder().decode([CaptureDisplay].self, from: data)
    }
}

private struct DisplayDiscoveryError: LocalizedError {
    let message: String
    var errorDescription: String? { message }
}
