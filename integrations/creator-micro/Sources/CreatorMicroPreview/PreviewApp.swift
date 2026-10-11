import SwiftUI
import AppKit
import BeckonPanelModel
import BeckonPanelUI
import BeckonCreatorMicro

@main struct CreatorMicroPreviewApp: App {
    @NSApplicationDelegateAdaptor(PanelDelegate.self) var delegate
    @StateObject private var model: PanelModel
    private let demo: DemoSource?
    private let notice: String?
    private let writable: Bool
    init() {
        do {
            // AIDEV-NOTE: This preview can load panel settings but never owns a configuration writer.
            let bootstrap = try PanelBootstrap(arguments: Array(CommandLine.arguments.dropFirst()),
                home: FileManager.default.homeDirectoryForCurrentUser.path, allowPersistence: false)
            _model = StateObject(wrappedValue: bootstrap.model)
            demo = bootstrap.demo; notice = bootstrap.notice; writable = bootstrap.writable
        } catch {
            FileHandle.standardError.write(Data("Preview unavailable: \(error.localizedDescription)\nUsage: beckon-creator-micro-preview [--demo] [--no-save] [--herdr-socket ABSOLUTE] [--config ABSOLUTE]\nSaving, hardware and focus are unavailable.\n".utf8))
            exit(2)
        }
    }
    var body: some Scene {
        WindowGroup("Creator Micro · Status Preview", id: "panel") {
            PanelView(model: model, demo: demo, notice: notice, writable: writable,
                supplement: AnyView(CreatorMicroStatusView(model: model)))
                .frame(minWidth: 920, minHeight: 640)
                .onAppear {delegate.model = model; model.startObserving()}
                .onDisappear {model.stopObserving()}
        }.defaultSize(width: 1100, height: 840)
        MenuBarExtra("Creator Micro Preview", systemImage: "circle.grid.2x2") {
            OpenPanelButton()
            Button("Quit preview") {NSApplication.shared.terminate(nil)}
        }
    }
}
