//
//  ShellTerminalView.swift
//  FlightDeckRemote
//
//  The renderer for the minimal shell terminal (PRD §5.4), a `UIViewRepresentable`
//  wrapper around SwiftTerm's `TerminalView`.
//
//  Why SwiftTerm (vs. a custom SGR parser): SwiftTerm's `TerminalView` gives us
//  ANSI colour interpretation, scrollback, and long-press text selection/copy
//  "for free", which is exactly the v1 minimal-terminal surface (stream
//  stdout with basic ANSI colours + scrollback + copy). We feed streamed
//  `ShellOutput` chunks via `feed(text:)` (the chunk `data` is already a
//  UTF-8 string with embedded ANSI escapes), and its `TerminalViewDelegate`
//  hands us keyboard input via `send(source:data:)` → `shell_input`. We
//  deliberately suppress SwiftTerm's *own* input accessory bar (a `FDTerminalView`
//  override returns `nil`) because PRD §5.4 mandates OUR key bar (`ShellKeyBar`)
//  with the sticky-`Ctrl` semantics. Full-screen/cursor-addressed programs are
//  explicitly out of v1 scope — `TerminalView` tolerates them, we just don't
//  build affordances for them.
//
//  The wrapper is thin: all state lives in `ShellSessionModel`. `updateUIView`
//  feeds only the *new* tail of `model.orderedOutput` (a coordinator cursor),
//  and reports the fitted `cols`/`rows` back to the model once, so `shell_open`
//  can request a geometry that matches the phone screen.
//
//  Font size (PRD §5.4 font-size control): `fontSize` is owned by
//  `ShellView` (persisted via `@AppStorage`, cycled by the toolbar's "font"
//  button) and threaded down here. `updateUIView` re-applies it to the live
//  `TerminalView.font` whenever it changes; SwiftTerm's `font` setter
//  recomputes cell metrics and re-fits `cols`/`rows` itself (surfaced back to
//  the model via `sizeChanged`), so a size change reflows like a real resize.
//

import SwiftUI
import SwiftTerm

/// Imperative handle so `ShellView` can invoke copy on the live terminal
/// (SwiftTerm also offers Copy via the standard long-press edit menu; this is
/// the explicit toolbar affordance PRD §5.4 asks for).
@MainActor
final class ShellTerminalController {
    fileprivate weak var terminalView: TerminalView?

    /// Copy the current selection to the pasteboard. Returns the copied text,
    /// or `nil` when nothing is selected.
    @discardableResult
    func copySelection() -> String? {
        guard let text = terminalView?.getSelection(), !text.isEmpty else { return nil }
        UIPasteboard.general.string = text
        terminalView?.selectNone()
        return text
    }
}

struct ShellTerminalRenderer: UIViewRepresentable {
    let model: ShellSessionModel
    let controller: ShellTerminalController
    var fontSize: CGFloat = ShellFontSize.default.rawValue

    func makeCoordinator() -> Coordinator { Coordinator(model: model) }

    func makeUIView(context: Context) -> TerminalView {
        let terminal = TerminalView(frame: .zero, font: terminalFont(ofSize: fontSize))
        terminal.terminalDelegate = context.coordinator
        terminal.backgroundColor = UIColor(Theme.bgField)
        terminal.isOpaque = true
        // Collapse the terminal into a single accessibility element. SwiftTerm
        // otherwise exposes a very large per-cell accessibility tree that makes
        // XCUITest snapshotting pathologically slow (and unrelated queries time
        // out). One opaque element keyed "shell-terminal" is all the UI tests
        // need, and VoiceOver still reads the buffer via the element's value.
        terminal.isAccessibilityElement = true
        terminal.accessibilityIdentifier = "shell-terminal"
        // Suppress SwiftTerm's built-in input accessory bar — PRD §5.4 requires
        // OUR key bar (`ShellKeyBar`) with sticky-`Ctrl`; showing both would
        // fight for the space above the keyboard and diverge on semantics.
        terminal.inputAccessoryView = nil
        controller.terminalView = terminal
        // Feed whatever we already have (fixture seed / reconnect replay).
        context.coordinator.feed(terminal, chunks: model.orderedOutput)
        return terminal
    }

    func updateUIView(_ terminal: TerminalView, context: Context) {
        // Report fitted geometry once (before the user opens the shell).
        let t = terminal.getTerminal()
        model.setGeometry(cols: UInt16(clamping: t.cols), rows: UInt16(clamping: t.rows))
        // Re-apply the font only on an actual change — SwiftTerm's `font`
        // setter always resets cell metrics, so setting it every render would
        // needlessly reflow the terminal.
        if abs(terminal.font.pointSize - fontSize) > 0.01 {
            terminal.font = terminalFont(ofSize: fontSize)
        }
        // Feed only the new tail of ordered output.
        context.coordinator.feed(terminal, chunks: model.orderedOutput)
    }

    /// The renderer's monospace font at a given point size, falling back to
    /// the system monospace face when the bundled Geist Mono isn't available.
    private func terminalFont(ofSize size: CGFloat) -> UIFont {
        UIFont(name: "GeistMono-Regular", size: size)
            ?? .monospacedSystemFont(ofSize: size, weight: .regular)
    }

    @MainActor
    final class Coordinator: NSObject, TerminalViewDelegate {
        private let model: ShellSessionModel
        private var fedCount = 0

        init(model: ShellSessionModel) { self.model = model }

        /// Feed any chunks past the cursor. Idempotent per render.
        func feed(_ terminal: TerminalView, chunks: [String]) {
            guard fedCount < chunks.count else { return }
            for chunk in chunks[fedCount...] {
                terminal.feed(text: chunk)
            }
            fedCount = chunks.count
        }

        // MARK: TerminalViewDelegate

        func send(source: TerminalView, data: ArraySlice<UInt8>) {
            model.handleKeyboardInput(Array(data))
        }

        func sizeChanged(source: TerminalView, newCols: Int, newRows: Int) {
            model.setGeometry(cols: UInt16(clamping: newCols), rows: UInt16(clamping: newRows))
        }

        func setTerminalTitle(source: TerminalView, title: String) {}
        func hostCurrentDirectoryUpdate(source: TerminalView, directory: String?) {}
        func scrolled(source: TerminalView, position: Double) {}
        func requestOpenLink(source: TerminalView, link: String, params: [String: String]) {}
        func bell(source: TerminalView) {}
        func clipboardCopy(source: TerminalView, content: Data) {}
        func clipboardRead(source: TerminalView) -> Data? { nil }
        func iTermContent(source: TerminalView, content: ArraySlice<UInt8>) {}
        func rangeChanged(source: TerminalView, startY: Int, endY: Int) {}
    }
}
