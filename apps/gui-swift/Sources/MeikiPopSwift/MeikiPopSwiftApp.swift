import AppKit

@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate {
    private let popup = CursorPopupController()
    private var pipeline: RustPipeline?
    private var eventTimer: Timer?
    private var statusItem: NSStatusItem?

    func applicationDidFinishLaunching(_ notification: Notification) {
        installStatusItem()
        startPipeline()
    }

    func applicationWillTerminate(_ notification: Notification) {
        eventTimer?.invalidate()
        eventTimer = nil
        pipeline = nil
    }

    private func installStatusItem() {
        let statusItem = NSStatusBar.system.statusItem(withLength: NSStatusItem.squareLength)
        if let button = statusItem.button {
            button.image = NSImage(
                systemSymbolName: "character.book.closed",
                accessibilityDescription: "MeikiPop"
            )
            button.toolTip = "MeikiPop"
        }

        let menu = NSMenu()
        let quitItem = NSMenuItem(
            title: "Quit MeikiPop",
            action: #selector(quit),
            keyEquivalent: "q"
        )
        quitItem.target = self
        menu.addItem(quitItem)
        statusItem.menu = menu

        self.statusItem = statusItem
    }

    private func startPipeline() {
        do {
            pipeline = try RustPipeline(dictionaryPath: Self.dictionaryPath)
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
            case let .show(entries, kanji):
                popup.show(entries: entries, kanji: kanji)
                pipeline.setPopupVisible(true)
            case .hide:
                popup.requestHide()
            case let .error(message):
                popup.showError(message)
            }
        }

        // Keep native capture paused while the pointer is interacting with the
        // popup. This also lets a later pointer move produce a fresh hide event.
        if popup.isPointerInside {
            pipeline.setPopupVisible(true)
        }
    }

    private static var dictionaryPath: String {
        FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent(".local/share/meikipop/dictionary.pkl")
            .path
    }

    @objc private func quit() {
        NSApplication.shared.terminate(nil)
    }
}

@main
@MainActor
struct MeikiPopSwiftApp {
    static func main() {
        let application = NSApplication.shared
        let delegate = AppDelegate()

        application.delegate = delegate
        application.setActivationPolicy(.accessory)
        application.run()
    }
}
