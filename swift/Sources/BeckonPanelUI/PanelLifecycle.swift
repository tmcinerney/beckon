import SwiftUI
import AppKit
import BeckonPanelModel

@MainActor public final class PanelDelegate: NSObject, NSApplicationDelegate {
    public var model: PanelModel?
    public override init() {super.init()}
    public func applicationShouldTerminate(_ sender: NSApplication) -> NSApplication.TerminateReply {
        guard let model else {return .terminateNow}
        Task {
            do {
                try await model.shutdownAndWait()
                sender.reply(toApplicationShouldTerminate: true)
            } catch {
                // AIDEV-NOTE: Keep supervision until metadata cleanup is explicitly reconciled.
                sender.reply(toApplicationShouldTerminate: false)
            }
        }
        return .terminateLater
    }
}

public struct OpenPanelButton: View {
    @Environment(\.openWindow) private var openWindow
    public init() {}
    public var body: some View {Button("Open status panel") {openWindow(id: "panel")}}
}
