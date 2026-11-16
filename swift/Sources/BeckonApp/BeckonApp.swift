import SwiftUI
import AppKit
import BeckonConfiguration
import BeckonConfigurationUI

private enum BeckonWindow {
    static let id = "beckon-main"
}

private enum BeckonWindowToken: String, Codable, Hashable {
    case main
}

@MainActor private enum ForegroundDiagnostics {
    static func log(_ event: String) {
        guard ProcessInfo.processInfo.environment["BECKON_FOREGROUND_DIAGNOSTICS"] == "1" else { return }
        let app = NSApplication.shared
        let policy: String
        switch app.activationPolicy() {
        case .regular: policy = "regular"
        case .accessory: policy = "accessory"
        case .prohibited: policy = "prohibited"
        @unknown default: policy = "unknown"
        }
        let line = "[DEBUG-beckon-foreground-20261116] event=\(event) policy=\(policy) isActive=\(app.isActive)\n"
        FileHandle.standardError.write(Data(line.utf8))
    }
}

@MainActor private final class BeckonAppDelegate: NSObject, NSApplicationDelegate {
    func applicationWillFinishLaunching(_ notification: Notification) {
        ForegroundDiagnostics.log("will-finish-before-policy")
        // AIDEV-NOTE: The bare SwiftPM executable was natively observed with prohibited activation policy.
        let accepted = NSApplication.shared.setActivationPolicy(.regular)
        ForegroundDiagnostics.log("set-regular-accepted-\(accepted)")
        if !accepted {
            FileHandle.standardError.write(Data("Beckon: regular application activation policy was rejected.\n".utf8))
        }
    }
    func applicationDidFinishLaunching(_ notification: Notification) { ForegroundDiagnostics.log("did-finish-launching") }
    func applicationDidBecomeActive(_ notification: Notification) { ForegroundDiagnostics.log("did-become-active") }
    func applicationDidResignActive(_ notification: Notification) { ForegroundDiagnostics.log("did-resign-active") }
}

@main struct BeckonApp: App {
    @NSApplicationDelegateAdaptor(BeckonAppDelegate.self) private var appDelegate
    @StateObject private var model = ConfigurationModel()
    @State private var menuInserted = true
    var body: some Scene {
        // AIDEV-NOTE: WindowGroup preserves app lifetime; explicit same-value opens reuse the companion.
        WindowGroup("Beckon", id: BeckonWindow.id, for: BeckonWindowToken.self) { _ in
            TabView {
                DeviceStatusView(model: model)
                    .tabItem { Label("Devices", systemImage: "keyboard") }
                ConfigurationView(model: model)
                    .tabItem { Label("Configuration", systemImage: "slider.horizontal.3") }
            }.frame(minWidth: 720, minHeight: 540)
                .onAppear { ForegroundDiagnostics.log("main-scene-appeared") }
        } defaultValue: {
            BeckonWindowToken.main
        }
        .defaultSize(width: 960, height: 800)
        .commands { CommandGroup(replacing: .newItem) { } }
        MenuBarExtra(isInserted: $menuInserted) { BeckonMenuContent(model: model) } label: { MenuHealthLabel(model: model) }
            .menuBarExtraStyle(.window)
    }
}

@MainActor private struct BeckonMenuContent: View {
    @ObservedObject var model: ConfigurationModel
    @Environment(\.openWindow) private var openWindow
    var body: some View {
        MenuHealthView(model: model, openMainWindow: {
            ForegroundDiagnostics.log("open-request-before")
            openWindow(id: BeckonWindow.id, value: BeckonWindowToken.main)
            let app = NSApplication.shared
            if #available(macOS 14, *) {
                app.activate()
            } else {
                app.activate(ignoringOtherApps: true)
            }
            ForegroundDiagnostics.log("open-request-after-activation")
        })
    }
}
