import SwiftUI
import AppKit
import BeckonPanelModel
import BeckonPanelUI
import BeckonCreatorMicro

private enum DemoAppearance: String, CaseIterable, Hashable {
    case system = "System", light = "Light", dark = "Dark"
    var colorScheme: ColorScheme? {
        switch self {
        case .system: return nil
        case .light: return .light
        case .dark: return .dark
        }
    }
}

@main struct CreatorMicroPreviewApp: App {
    @NSApplicationDelegateAdaptor(PanelDelegate.self) var delegate
    @StateObject private var model: PanelModel
    private let demo: DemoSource?
    private let notice: String?
    private let writable: Bool
    private let inputPilot: Bool
    init() {
        do {
            let arguments = Array(CommandLine.arguments.dropFirst())
            inputPilot = try InputPilotLaunch.isRequested(arguments: arguments)
            // AIDEV-NOTE: Input mode never mounts PanelView or starts its observer. Its unused
            // lifecycle model uses only a demo source, with no socket or config-file reader.
            let bootstrap = try PanelBootstrap(arguments: inputPilot ? ["--demo", "--no-save"] : arguments,
                home: FileManager.default.homeDirectoryForCurrentUser.path, allowPersistence: false)
            _model = StateObject(wrappedValue: bootstrap.model)
            demo = bootstrap.demo; notice = bootstrap.notice; writable = bootstrap.writable
        } catch {
            FileHandle.standardError.write(Data("Preview unavailable: \(error.localizedDescription)\nUsage: beckon-creator-micro-preview [--input-pilot] OR [--demo] [--no-save] [--herdr-socket ABSOLUTE] [--config ABSOLUTE]\nSaving, hardware and focus are unavailable.\n".utf8))
            exit(2)
        }
    }
    var body: some Scene {
        WindowGroup("Creator Micro · Status Preview", id: "panel") {
            if inputPilot {
                InputPilotView()
            } else {
                CreatorMicroPreviewWindow(model: model, demo: demo, notice: notice, writable: writable)
                    .onAppear {delegate.model = model}
            }
        }.defaultSize(width: 1100, height: 840)
        MenuBarExtra("Creator Micro Preview", systemImage: "circle.grid.2x2") {
            OpenPanelButton()
            Button("Quit preview") {NSApplication.shared.terminate(nil)}
        }
    }
}

@MainActor private struct CreatorMicroPreviewWindow: View {
    @ObservedObject var model: PanelModel
    let demo: DemoSource?
    let notice: String?
    let writable: Bool
    // AIDEV-NOTE: Each WindowGroup window owns its appearance; agent observation remains shared.
    @State private var demoAppearance = DemoAppearance.system

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            if demo != nil {
                HStack(spacing: 12) {
                    Text("Demo appearance").font(.caption)
                    Picker("Demo appearance", selection: $demoAppearance) {
                        ForEach(DemoAppearance.allCases, id: \.self) {appearance in
                            Text(appearance.rawValue).tag(appearance)
                        }
                    }.labelsHidden().pickerStyle(.segmented).frame(width: 280)
                    Text("This window only · stays in memory").font(.caption).foregroundStyle(.secondary)
                    Spacer()
                }.padding(.horizontal, 20).padding(.top, 12)
            }
            PanelView(model: model, demo: demo, notice: notice, writable: writable,
                supplement: AnyView(CreatorMicroStatusView(model: model)))
        }
        .frame(minWidth: 920, minHeight: 640)
        .preferredColorScheme(demo == nil ? nil : demoAppearance.colorScheme)
    }
}
