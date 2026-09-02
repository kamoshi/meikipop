import AppKit
import SwiftUI

struct TrayMenu: Scene {
    @ObservedObject var settings: AppSettings
    @State private var isPaused = false

    var body: some Scene {
        MenuBarExtra("MeikiPop", systemImage: "character.book.closed") {
            SettingsLink {
                Text("Settings")
            }

            Divider()

            Menu("OCR Provider") {
                Picker("OCR Provider", selection: persistedBinding(\.ocrProvider)) {
                    ForEach(settings.availableOCRProviders) { provider in
                        Text(provider.name).tag(provider.id)
                    }
                }
                .pickerStyle(.inline)
                .labelsHidden()
                .disabled(settings.availableOCRProviders.isEmpty)
            }

            Divider()

            Menu("Scan mode") {
                Picker("Scan mode", selection: persistedBinding(\.scanMode)) {
                    ForEach(ScanMode.allCases) { mode in
                        Text(mode.title).tag(mode)
                    }
                }
                .pickerStyle(.inline)
                .labelsHidden()
            }

            Divider()

            Toggle("Pause meikipop", isOn: $isPaused)

            Divider()

            Button("Quit") {
                NSApplication.shared.terminate(nil)
            }
            .keyboardShortcut("q")
        }
        .menuBarExtraStyle(.menu)
    }

    private func persistedBinding<Value>(
        _ keyPath: WritableKeyPath<AppConfiguration.General, Value>
    ) -> Binding<Value> {
        Binding(
            get: { settings.configuration.general[keyPath: keyPath] },
            set: { value in
                settings.configuration.general[keyPath: keyPath] = value
                do {
                    try settings.save()
                } catch {
                    NSLog("Could not save MeikiPop tray setting: %@", error.localizedDescription)
                }
            }
        )
    }
}
