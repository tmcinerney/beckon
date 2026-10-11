import SwiftUI
import AppKit
import BeckonPanelModel
import BeckonPanelUI

@main struct BeckonPanelApp: App {
    @NSApplicationDelegateAdaptor(PanelDelegate.self) var delegate
    @StateObject private var model: PanelModel
    private let demo: DemoSource?
    private let configNotice: String?
    private let writable: Bool
    init() {
        do {
            let bootstrap = try PanelBootstrap(arguments: Array(CommandLine.arguments.dropFirst()),
                home: FileManager.default.homeDirectoryForCurrentUser.path)
            demo = bootstrap.demo; configNotice = bootstrap.notice; writable = bootstrap.writable
            _model = StateObject(wrappedValue: bootstrap.model)
        } catch {
            FileHandle.standardError.write(Data("Panel unavailable: \(error.localizedDescription)\nUsage: beckon-control-panel [--demo] [--no-save] [--herdr-socket ABSOLUTE] [--config ABSOLUTE]\n".utf8))
            exit(2)
        }
    }
    var body: some Scene {
        WindowGroup("Beckon Status Panel", id: "panel") {
            PanelView(model: model, demo: demo, notice: configNotice, writable: writable)
                .onAppear {delegate.model = model}
        }.defaultSize(width: 940, height: 760)
        MenuBarExtra("Beckon Status Panel", systemImage: "circle.grid.2x2") {
            OpenPanelButton()
            Button("Quit test panel") {NSApplication.shared.terminate(nil)}
        }
    }
}
