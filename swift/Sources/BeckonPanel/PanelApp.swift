import SwiftUI
import AppKit
import BeckonPanelModel

@MainActor final class PanelDelegate:NSObject,NSApplicationDelegate {
    var model:PanelModel?
    func applicationShouldTerminate(_ sender:NSApplication)->NSApplication.TerminateReply {
        guard let model else {return .terminateNow}
        Task {
            do {
                try await model.shutdownAndWait()
                sender.reply(toApplicationShouldTerminate:true)
            } catch {
                // Retain the process owner/observer and show the explicit reconciliation route.
                sender.reply(toApplicationShouldTerminate:false)
            }
        }
        return .terminateLater
    }
}
@main struct BeckonPanelApp:App {
    @NSApplicationDelegateAdaptor(PanelDelegate.self) var delegate
    @StateObject private var model:PanelModel
    private let demo:DemoSource?
    private let configNotice:String?
    private let writable:Bool
    init() {
        do {
            let args=Array(CommandLine.arguments.dropFirst())
            let options=try PanelOptions(arguments:args,home:FileManager.default.homeDirectoryForCurrentUser.path)
            let source=SourceKey(socket:options.socket)
            var config=PanelConfiguration(source:source)
            var notice:String?
            var store:ConfigStore?
            var demoReader:DemoSource?
            if options.demo {
                let d=DemoSource();demoReader=d
                config.coordinators[0]=try DemoSource.directorySnapshot().assignment(id:"demo-coordinator")
                config.slots[1]=NativeID(harness:"claude",sessionID:"demo-b")
            } else {
                let candidate=ConfigStore(url:URL(fileURLWithPath:options.config))
                do {config=try candidate.load(defaultSource:source);if !options.noSave {store=candidate}}
                catch {notice="Configuration unavailable; original file preserved. \(error.localizedDescription)"}
                if args.contains("--herdr-socket") {config.source=source}
            }
            demo=demoReader;configNotice=notice;writable=notice==nil
            let panel=PanelModel(configuration:config,source:demoReader ?? SocketAgentSource(),store:store)
            if options.demo {try panel.useDemoMetadata(DemoSource.directorySnapshot())}
            _model=StateObject(wrappedValue:panel)
        } catch {
            FileHandle.standardError.write(Data("Panel unavailable: \(error.localizedDescription)\nUsage: beckon-control-panel [--demo] [--no-save] [--herdr-socket ABSOLUTE] [--config ABSOLUTE]\n".utf8))
            exit(2)
        }
    }
    var body:some Scene {
        WindowGroup("Beckon Status Panel",id:"panel") {
            PanelView(model:model,demo:demo,notice:configNotice,writable:writable)
                .onAppear {delegate.model=model;model.startObserving()}
                .onDisappear {model.stopObserving()}
        }.defaultSize(width:940,height:760)
        MenuBarExtra("Beckon Status Panel",systemImage:"circle.grid.2x2") {
            OpenPanelButton()
            Button("Quit test panel") {NSApplication.shared.terminate(nil)}
        }
    }
}
struct OpenPanelButton:View {
    @Environment(\.openWindow) private var openWindow
    var body:some View {Button("Open status panel") {openWindow(id:"panel")}}
}
