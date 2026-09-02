import AppKit
import SwiftUI

private final class CursorPopupModel: ObservableObject {
    @Published var entries: [LookupEntry] = []
    @Published var kanji: LookupKanji?
    @Published var errorMessage: String?
}

private struct PopupContentHeightPreferenceKey: PreferenceKey {
    static var defaultValue: CGFloat = 0

    static func reduce(value: inout CGFloat, nextValue: () -> CGFloat) {
        value = max(value, nextValue())
    }
}

private struct CursorPopupView: View {
    @ObservedObject var model: CursorPopupModel
    let onContentHeightChange: (CGFloat) -> Void

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                if let errorMessage = model.errorMessage {
                    errorView(errorMessage)
                } else {
                    ForEach(Array(model.entries.enumerated()), id: \.offset) { index, entry in
                        if index > 0 {
                            Divider()
                        }
                        LookupEntryView(entry: entry)
                    }

                    if let kanji = model.kanji {
                        Divider()
                        KanjiView(kanji: kanji)
                    }
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(18)
            .background {
                GeometryReader { geometry in
                    Color.clear.preference(
                        key: PopupContentHeightPreferenceKey.self,
                        value: geometry.size.height
                    )
                }
            }
        }
        .onPreferenceChange(PopupContentHeightPreferenceKey.self, perform: onContentHeightChange)
        .background(.regularMaterial)
        .clipShape(RoundedRectangle(cornerRadius: 12))
    }

    private func errorView(_ message: String) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            Label("MeikiPop error", systemImage: "exclamationmark.triangle.fill")
                .font(.headline)
                .foregroundStyle(.red)
            Text(message)
                .textSelection(.enabled)
        }
    }
}

private struct LookupEntryView: View {
    let entry: LookupEntry

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(alignment: .firstTextBaseline) {
                Text(entry.writtenForm ?? entry.reading)
                    .font(.title2.weight(.semibold))

                Spacer(minLength: 12)

                if entry.freq > 0 && entry.freq < 999_999 {
                    Text("freq. \(entry.freq)")
                        .font(.caption.monospacedDigit())
                        .foregroundStyle(.secondary)
                        .padding(.horizontal, 7)
                        .padding(.vertical, 3)
                        .background(.quaternary, in: Capsule())
                }
            }

            if entry.writtenForm != nil && !entry.reading.isEmpty {
                Text(entry.reading)
                    .font(.headline)
                    .foregroundStyle(.secondary)
            }

            let deconjugation = entry.deconjugationProcess
                .filter { !$0.isEmpty }
                .joined(separator: " ← ")
            if !deconjugation.isEmpty {
                Label(deconjugation, systemImage: "arrow.uturn.backward")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }

            VStack(alignment: .leading, spacing: 10) {
                ForEach(Array(entry.senses.enumerated()), id: \.offset) { index, sense in
                    SenseView(
                        number: entry.senses.count > 1 ? index + 1 : nil,
                        sense: sense
                    )
                }
            }
        }
    }
}

private struct SenseView: View {
    let number: Int?
    let sense: LookupSense

    var body: some View {
        HStack(alignment: .top, spacing: 8) {
            if let number {
                Text("\(number).")
                    .font(.callout.monospacedDigit())
                    .foregroundStyle(.secondary)
                    .frame(minWidth: 20, alignment: .trailing)
            }

            VStack(alignment: .leading, spacing: 4) {
                if !metadata.isEmpty {
                    Text(metadata)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }

                Text(sense.glosses.joined(separator: "; "))
                    .font(.body)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
    }

    private var metadata: String {
        var parts: [String] = []
        if !sense.pos.isEmpty {
            parts.append(sense.pos.joined(separator: ", "))
        }
        if !sense.tags.isEmpty {
            parts.append(sense.tags.map { "[\($0)]" }.joined(separator: " "))
        }
        return parts.joined(separator: "  ·  ")
    }
}

private struct KanjiView: View {
    let kanji: LookupKanji

    var body: some View {
        HStack(alignment: .top, spacing: 14) {
            Text(kanji.character)
                .font(.system(size: 38, weight: .medium))

            VStack(alignment: .leading, spacing: 5) {
                Text("Kanji")
                    .font(.caption.weight(.semibold))
                    .foregroundStyle(.secondary)
                    .textCase(.uppercase)

                if !kanji.readings.isEmpty {
                    Text(kanji.readings.joined(separator: ", "))
                        .font(.headline)
                }

                if !kanji.meanings.isEmpty {
                    Text(kanji.meanings.joined(separator: "; "))
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
        }
    }
}

@MainActor
final class CursorPopupController {
    private let popupWidth: CGFloat = 440
    private let minimumPopupHeight: CGFloat = 96
    private let maximumPopupHeight: CGFloat = 360
    private let model: CursorPopupModel
    private let panel: NSPanel
    private var pendingHide: DispatchWorkItem?

    var isPointerInside: Bool {
        panel.isVisible && NSMouseInRect(NSEvent.mouseLocation, panel.frame, false)
    }

    var captureBounds: PopupCaptureBounds? {
        guard panel.isVisible, let primaryScreen = NSScreen.screens.first else { return nil }
        let frame = panel.frame
        return PopupCaptureBounds(
            left: Int32(clamping: Int(frame.minX.rounded())),
            top: Int32(clamping: Int((primaryScreen.frame.maxY - frame.maxY).rounded())),
            width: UInt32(clamping: Int(frame.width.rounded())),
            height: UInt32(clamping: Int(frame.height.rounded()))
        )
    }

    init() {
        let model = CursorPopupModel()
        let panel = NSPanel(
            contentRect: NSRect(
                origin: .zero,
                size: NSSize(width: popupWidth, height: maximumPopupHeight)
            ),
            styleMask: [.borderless, .nonactivatingPanel],
            backing: .buffered,
            defer: false
        )

        self.model = model
        self.panel = panel

        panel.contentView = NSHostingView(
            rootView: CursorPopupView(model: model) { [weak self] contentHeight in
                Task { @MainActor in
                    self?.resizePanel(toFit: contentHeight)
                }
            }
        )
        panel.level = .popUpMenu
        panel.isFloatingPanel = true
        panel.isOpaque = false
        panel.backgroundColor = .clear
        panel.hasShadow = true
        panel.hidesOnDeactivate = false
        panel.ignoresMouseEvents = false
        panel.becomesKeyOnlyIfNeeded = true
        panel.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary]
    }

    func show(entries: [LookupEntry], kanji: LookupKanji?) {
        pendingHide?.cancel()
        model.entries = entries
        model.kanji = kanji
        model.errorMessage = nil
        showPanel()
    }

    func showError(_ message: String) {
        pendingHide?.cancel()
        model.entries = []
        model.kanji = nil
        model.errorMessage = message
        showPanel()
    }

    func requestHide() {
        pendingHide?.cancel()
        let work = DispatchWorkItem { [weak self] in
            MainActor.assumeIsolated {
                guard let self, !self.isPointerInside else { return }
                self.hide()
            }
        }
        pendingHide = work
        DispatchQueue.main.asyncAfter(deadline: .now() + .milliseconds(200), execute: work)
    }

    func hide() {
        pendingHide?.cancel()
        pendingHide = nil
        panel.orderOut(nil)
    }

    private func showPanel() {
        if !isPointerInside {
            panel.setFrameOrigin(originNextToCursor(for: panel.frame.size))
        }
        panel.orderFrontRegardless()
    }

    private func resizePanel(toFit contentHeight: CGFloat) {
        guard contentHeight.isFinite, contentHeight > 0 else { return }

        let height = min(max(ceil(contentHeight), minimumPopupHeight), maximumPopupHeight)
        guard abs(panel.frame.height - height) >= 1 else { return }

        let size = NSSize(width: popupWidth, height: height)
        let origin: NSPoint
        if panel.isVisible && !isPointerInside {
            origin = originNextToCursor(for: size)
        } else {
            // Preserve the top edge while resizing an already displayed panel,
            // so it does not jump away from the content the user is reading.
            origin = NSPoint(x: panel.frame.minX, y: panel.frame.maxY - height)
        }
        panel.setFrame(NSRect(origin: origin, size: size), display: panel.isVisible)
    }

    private func originNextToCursor(for popupSize: NSSize) -> NSPoint {
        let cursor = NSEvent.mouseLocation
        let screen = NSScreen.screens.first {
            NSMouseInRect(cursor, $0.frame, false)
        } ?? NSScreen.main

        guard let visibleFrame = screen?.visibleFrame else {
            return NSPoint(x: cursor.x + 12, y: cursor.y - popupSize.height - 12)
        }

        let desiredX = cursor.x + 12
        let desiredY = cursor.y - popupSize.height - 12
        let maximumX = visibleFrame.maxX - popupSize.width
        let maximumY = visibleFrame.maxY - popupSize.height

        return NSPoint(
            x: min(max(desiredX, visibleFrame.minX), maximumX),
            y: min(max(desiredY, visibleFrame.minY), maximumY)
        )
    }
}
