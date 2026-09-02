import AppKit
import Combine
import SwiftUI

@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate {
    let settings = AppSettings()
    private let popup = CursorPopupController()
    private var pipeline: RustPipeline?
    private var eventTimer: Timer?
    private var settingsObserver: AnyCancellable?

    func applicationDidFinishLaunching(_ notification: Notification) {
        NSApplication.shared.setActivationPolicy(.accessory)
        startPipeline()
    }

    func applicationWillTerminate(_ notification: Notification) {
        eventTimer?.invalidate()
        eventTimer = nil
        settingsObserver = nil
        pipeline = nil
    }

    private func startPipeline() {
        do {
            let provider = settings.configuration.general.ocrProvider
            pipeline = try RustPipeline(
                dictionaryPath: Self.dictionaryPath,
                ocrProvider: provider
            )
            observeConfiguration(initialProvider: provider)
        } catch {
            popup.showError("MeikiPop could not start\n\n\(error.localizedDescription)")
            return
        }

        let timer = Timer(timeInterval: 0.02, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated {
                self?.processPipelineEvents()
            }
        }
        RunLoop.main.add(timer, forMode: .common)
        eventTimer = timer
    }

    private func processPipelineEvents() {
        guard let pipeline else { return }

        while let event = pipeline.poll() {
            switch event {
            case .captureReady:
                // screencapturekit 1.x temporarily promotes accessory apps so
                // macOS can present its system picker. Return to tray-only mode
                // once the user has selected a screen.
                NSApplication.shared.setActivationPolicy(.accessory)
            case let .ocrProviders(providers, activeProvider, error):
                settings.updateOCRProviders(
                    providers,
                    activeProvider: activeProvider
                )
                if let error {
                    NSLog("Could not change OCR provider: %@", error)
                }
            case let .show(entries, kanji):
                guard !popup.isPointerInside else {
                    continue
                }
                popup.show(entries: entries, kanji: kanji)
            case .hide:
                popup.requestHide()
            case let .error(message):
                NSApplication.shared.setActivationPolicy(.accessory)
                popup.showError(message)
            }
        }

        // Keep capture and hit-testing synchronized with the panel's actual
        // lifetime, including the delayed hide period.
        pipeline.setPopupBounds(popup.captureBounds)
    }

    private func observeConfiguration(initialProvider: String) {
        var requestedProvider = initialProvider
        settingsObserver = settings.$configuration
            .map(\.general.ocrProvider)
            .removeDuplicates()
            .sink { [weak self] provider in
                guard provider != requestedProvider else { return }
                requestedProvider = provider
                self?.pipeline?.updateConfiguration(ocrProvider: provider)
            }
    }

    private static var dictionaryPath: String {
        FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent(".local/share/meikipop/dictionary.pkl")
            .path
    }
}

@main
@MainActor
struct MeikiPopSwiftApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var delegate

    var body: some Scene {
        TrayMenu(settings: delegate.settings)

        Settings {
            SettingsView(settings: delegate.settings)
        }
    }
}
