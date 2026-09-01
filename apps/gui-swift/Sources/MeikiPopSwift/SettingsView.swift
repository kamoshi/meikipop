import AppKit
import SwiftUI

struct SettingsView: View {
    @ObservedObject var settings: AppSettings
    @Environment(\.dismiss) private var dismiss
    @State private var saveError: String?

    var body: some View {
        VStack(spacing: 0) {
            TabView {
                GeneralSettingsView(settings: settings)
                    .tabItem {
                        Label("General", systemImage: "gearshape")
                    }

                PopupContentSettingsView(settings: settings)
                    .tabItem {
                        Label("Popup Content", systemImage: "list.bullet.rectangle")
                    }

                PopupAppearanceSettingsView(settings: settings)
                    .tabItem {
                        Label("Popup Appearance", systemImage: "paintpalette")
                    }
            }

            Divider()

            HStack {
                Spacer()
                Button("Cancel") {
                    settings.discardChanges()
                    dismiss()
                }
                .keyboardShortcut(.cancelAction)

                Button("Save") {
                    do {
                        try settings.save()
                        dismiss()
                    } catch {
                        saveError = error.localizedDescription
                    }
                }
                .keyboardShortcut(.defaultAction)
            }
            .padding()
        }
        .frame(width: 520, height: 620)
        .onDisappear {
            settings.discardChanges()
        }
        .alert("Could Not Save Settings", isPresented: saveErrorIsPresented) {
            Button("OK") {}
        } message: {
            Text(saveError ?? "Unknown error")
        }
    }

    private var saveErrorIsPresented: Binding<Bool> {
        Binding(
            get: { saveError != nil },
            set: { if !$0 { saveError = nil } }
        )
    }
}

private struct GeneralSettingsView: View {
    @ObservedObject var settings: AppSettings

    var body: some View {
        Form {
            Section("Core Settings") {
                Picker("Hotkey", selection: $settings.configuration.general.hotkey) {
                    ForEach(Hotkey.allCases) { hotkey in
                        Text(hotkey.title).tag(hotkey)
                    }
                }

                Picker("OCR Provider", selection: $settings.configuration.general.ocrProvider) {
                    ForEach(OCRProvider.allCases) { provider in
                        Text(provider.title).tag(provider)
                    }
                }

                Toggle(
                    "Google Lens Compression",
                    isOn: $settings.configuration.general.googleLensCompression
                )
                .help("Compress screenshots before sending them to Google Lens.")

                LabeledContent("Max Lookup Length") {
                    Stepper(
                        "\(settings.configuration.general.maxLookupLength)",
                        value: $settings.configuration.general.maxLookupLength,
                        in: 5...100
                    )
                }
            }

            Section("Auto Scan Mode") {
                Picker("Scan Mode", selection: $settings.configuration.general.scanMode) {
                    ForEach(ScanMode.allCases) { mode in
                        Text(mode.title).tag(mode)
                    }
                }

                Picker("Scan Area", selection: $settings.configuration.general.scanArea) {
                    Text("Custom Region").tag(ScanAreaSelection.customRegion)
                    ForEach(Array(settings.displays.enumerated()), id: \.element.id) { index, display in
                        Text(display.title(number: index + 1)).tag(display.selectionID)
                    }
                }

                Toggle(
                    "Only Scan on Mouse Move",
                    isOn: $settings.configuration.general.onlyScanOnMouseMove
                )

                LabeledContent("Scan Interval (Cooldown)") {
                    Stepper(
                        settings.configuration.general.scanInterval.formatted(
                            .number.precision(.fractionLength(1))
                        ) + " s",
                        value: $settings.configuration.general.scanInterval,
                        in: 0.0...60.0,
                        step: 0.1
                    )
                }

                Toggle(
                    "Show Popup without Hotkey",
                    isOn: $settings.configuration.general.showPopupWithoutHotkey
                )
            }

            Section("Popup Behavior") {
                Picker("Position Mode", selection: $settings.configuration.general.positionMode) {
                    ForEach(PositionMode.allCases) { mode in
                        Text(mode.title).tag(mode)
                    }
                }

                Toggle("Compact Mode", isOn: $settings.configuration.general.compactMode)
            }
        }
        .formStyle(.grouped)
    }
}

private struct PopupContentSettingsView: View {
    @ObservedObject var settings: AppSettings

    var body: some View {
        Form {
            Section("Vocab Entry Content") {
                Toggle(
                    "Show All Glosses",
                    isOn: $settings.configuration.popupContent.showAllGlosses
                )
                Toggle(
                    "Show Deconjugation",
                    isOn: $settings.configuration.popupContent.showDeconjugation
                )
                Toggle(
                    "Show Part of Speech",
                    isOn: $settings.configuration.popupContent.showPartOfSpeech
                )
                Toggle("Show Tags", isOn: $settings.configuration.popupContent.showTags)
                Toggle("Show Frequency", isOn: $settings.configuration.popupContent.showFrequency)
            }

            Section("Kanji Entry Content") {
                Toggle(
                    "Show Kanji Entries",
                    isOn: $settings.configuration.popupContent.showKanjiEntries
                )
                Toggle("Show Examples", isOn: $settings.configuration.popupContent.showExamples)
                Toggle("Show Components", isOn: $settings.configuration.popupContent.showComponents)
            }
        }
        .formStyle(.grouped)
    }
}

private struct PopupAppearanceSettingsView: View {
    @ObservedObject var settings: AppSettings

    var body: some View {
        Form {
            Section("Theme") {
                Picker("Preset", selection: $settings.configuration.popupAppearance.theme) {
                    ForEach(Theme.allCases) { theme in
                        Text(theme.title).tag(theme)
                    }
                }

                LabeledContent("Background Opacity") {
                    HStack {
                        Slider(
                            value: $settings.configuration.popupAppearance.backgroundOpacity,
                            in: 50...255
                        )
                        .frame(width: 180)
                        Text("\(Int(settings.configuration.popupAppearance.backgroundOpacity))")
                            .monospacedDigit()
                            .frame(width: 28, alignment: .trailing)
                    }
                }
            }

            Section("Typography") {
                Picker(
                    "Font Family",
                    selection: $settings.configuration.popupAppearance.fontFamily
                ) {
                    Text("System Default").tag("System Default")
                    Text("Hiragino Sans").tag("Hiragino Sans")
                    Text("Yu Gothic").tag("Yu Gothic")
                    Text("Noto Sans JP").tag("Noto Sans JP")
                }

                LabeledContent("Font Size (Header)") {
                    Stepper(
                        "\(settings.configuration.popupAppearance.headerFontSize)",
                        value: $settings.configuration.popupAppearance.headerFontSize,
                        in: 8...72
                    )
                }

                LabeledContent("Font Size (Definitions)") {
                    Stepper(
                        "\(settings.configuration.popupAppearance.definitionFontSize)",
                        value: $settings.configuration.popupAppearance.definitionFontSize,
                        in: 8...72
                    )
                }
            }

            Section("Colors") {
                ColorPicker("Background", selection: colorBinding(\.background))
                ColorPicker("Foreground", selection: colorBinding(\.foreground))
                ColorPicker("Highlight Word", selection: colorBinding(\.highlightWord))
                ColorPicker("Highlight Reading", selection: colorBinding(\.highlightReading))
            }
        }
        .formStyle(.grouped)
    }

    private func colorBinding(
        _ keyPath: WritableKeyPath<AppConfiguration.Colors, String>
    ) -> Binding<Color> {
        Binding(
            get: {
                Color(hex: settings.configuration.popupAppearance.colors[keyPath: keyPath])
            },
            set: { color in
                settings.configuration.popupAppearance.colors[keyPath: keyPath] = color.hexString
            }
        )
    }
}

private extension Color {
    init(hex: String) {
        let value = hex.trimmingCharacters(in: CharacterSet(charactersIn: "#"))
        guard value.count == 6, let rgb = UInt64(value, radix: 16) else {
            self = .black
            return
        }

        self.init(
            red: Double((rgb >> 16) & 0xFF) / 255,
            green: Double((rgb >> 8) & 0xFF) / 255,
            blue: Double(rgb & 0xFF) / 255
        )
    }

    var hexString: String {
        guard let color = NSColor(self).usingColorSpace(.sRGB) else {
            return "#000000"
        }

        return String(
            format: "#%02X%02X%02X",
            Int((color.redComponent * 255).rounded()),
            Int((color.greenComponent * 255).rounded()),
            Int((color.blueComponent * 255).rounded())
        )
    }
}
