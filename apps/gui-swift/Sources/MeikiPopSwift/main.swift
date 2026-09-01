import AppKit

@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate {
    private var statusItem: NSStatusItem?

    func applicationDidFinishLaunching(_ notification: Notification) {
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
