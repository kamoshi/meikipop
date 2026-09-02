import Foundation

struct AppConfiguration: Codable, Equatable {
    var schemaVersion = 1
    var general = General()
    var popupContent = PopupContent()
    var popupAppearance = PopupAppearance()

    struct General: Codable, Equatable {
        var hotkey: Hotkey = .shift
        var ocrProvider = "meikiocr"
        var googleLensCompression = false
        var maxLookupLength = 25
        var onlyScanOnMouseMove = true
        var scanInterval = 0.5
        var showPopupWithoutHotkey = true
        var positionMode: PositionMode = .visualNovel
        var compactMode = true
        var scanMode: ScanMode = .auto
    }

    struct PopupContent: Codable, Equatable {
        var showAllGlosses = false
        var showDeconjugation = false
        var showPartOfSpeech = false
        var showTags = false
        var showFrequency = false
        var showKanjiEntries = true
        var showExamples = true
        var showComponents = true
    }

    struct PopupAppearance: Codable, Equatable {
        var theme: Theme = .nazeka
        var backgroundOpacity = 245.0
        var fontFamily = "System Default"
        var headerFontSize = 18
        var definitionFontSize = 14
        var colors = Colors()
    }

    struct Colors: Codable, Equatable {
        var background = "#2E2E2E"
        var foreground = "#F0F0F0"
        var highlightWord = "#88D8FF"
        var highlightReading = "#90EE90"
    }
}

enum Hotkey: String, Codable, CaseIterable, Identifiable {
    case control = "ctrl"
    case shift
    case option = "alt"
    case controlShift = "ctrl+shift"
    case controlOption = "ctrl+alt"
    case shiftOption = "shift+alt"
    case controlShiftOption = "ctrl+shift+alt"

    var id: Self { self }

    var title: String {
        switch self {
        case .control: "Control"
        case .shift: "Shift"
        case .option: "Option"
        case .controlShift: "Control + Shift"
        case .controlOption: "Control + Option"
        case .shiftOption: "Shift + Option"
        case .controlShiftOption: "Control + Shift + Option"
        }
    }
}

struct OCRProviderOption: Decodable, Identifiable, Equatable {
    let id: String
    let name: String
}

enum PositionMode: String, Codable, CaseIterable, Identifiable {
    case flipBoth = "flip_both"
    case flipVertically = "flip_vertically"
    case flipHorizontally = "flip_horizontally"
    case visualNovel = "visual_novel"

    var id: Self { self }

    var title: String {
        switch self {
        case .flipBoth: "Flip Both"
        case .flipVertically: "Flip Vertically"
        case .flipHorizontally: "Flip Horizontally"
        case .visualNovel: "Visual Novel Mode"
        }
    }
}

enum ScanMode: String, Codable, CaseIterable, Identifiable {
    case manual
    case auto

    var id: Self { self }
    var title: String { rawValue.capitalized }
}

enum Theme: String, Codable, CaseIterable, Identifiable {
    case nazeka
    case celestialIndigo = "celestial_indigo"
    case neutralSlate = "neutral_slate"
    case academic
    case custom

    var id: Self { self }

    var title: String {
        switch self {
        case .nazeka: "Nazeka"
        case .celestialIndigo: "Celestial Indigo"
        case .neutralSlate: "Neutral Slate"
        case .academic: "Academic"
        case .custom: "Custom"
        }
    }
}

@MainActor
final class AppSettings: NSObject, ObservableObject {
    @Published var configuration: AppConfiguration
    @Published private(set) var availableOCRProviders: [OCRProviderOption] = []

    private var savedConfiguration: AppConfiguration
    private let fileURL: URL

    init(fileManager: FileManager = .default) {
        let applicationSupport = fileManager.urls(
            for: .applicationSupportDirectory,
            in: .userDomainMask
        ).first!
        fileURL = applicationSupport
            .appendingPathComponent("MeikiPop", isDirectory: true)
            .appendingPathComponent("config.json")

        let loaded = Self.load(from: fileURL) ?? AppConfiguration()
        configuration = loaded
        savedConfiguration = loaded
        super.init()
    }

    func save() throws {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
        encoder.keyEncodingStrategy = .convertToSnakeCase

        let data = try encoder.encode(configuration)
        try FileManager.default.createDirectory(
            at: fileURL.deletingLastPathComponent(),
            withIntermediateDirectories: true
        )
        try data.write(to: fileURL, options: .atomic)
        savedConfiguration = configuration
    }

    func discardChanges() {
        configuration = savedConfiguration
    }

    func updateOCRProviders(
        _ providers: [OCRProviderOption],
        activeProvider: String
    ) {
        availableOCRProviders = providers
        if configuration.general.ocrProvider != activeProvider {
            configuration.general.ocrProvider = activeProvider
        }
    }

    private static func load(from fileURL: URL) -> AppConfiguration? {
        guard FileManager.default.fileExists(atPath: fileURL.path) else {
            return nil
        }

        do {
            let decoder = JSONDecoder()
            decoder.keyDecodingStrategy = .convertFromSnakeCase
            return try decoder.decode(AppConfiguration.self, from: Data(contentsOf: fileURL))
        } catch {
            NSLog("Could not load MeikiPop settings: %@", error.localizedDescription)
            return nil
        }
    }
}
