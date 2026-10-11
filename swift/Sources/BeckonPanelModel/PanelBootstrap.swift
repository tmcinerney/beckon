import Foundation

/// Shared startup for the normal panel and optional, always nonpersistent previews.
@MainActor public struct PanelBootstrap {
    public let model: PanelModel
    public let demo: DemoSource?
    public let notice: String?
    public let writable: Bool

    public init(arguments: [String], home: String, allowPersistence: Bool = true) throws {
        let options = try PanelOptions(arguments: arguments, home: home)
        let source = SourceKey(socket: options.socket)
        var configuration = PanelConfiguration(source: source)
        var configNotice: String?
        var store: ConfigStore?
        var demoReader: DemoSource?
        if options.demo {
            demoReader = DemoSource()
            configuration.coordinators[0] = try DemoSource.directorySnapshot().assignment(id: "demo-coordinator")
            configuration.slots[1] = NativeID(harness: "claude", sessionID: "demo-b")
        } else {
            let candidate = ConfigStore(url: URL(fileURLWithPath: options.config))
            do {
                configuration = try candidate.load(defaultSource: source)
                if allowPersistence && !options.noSave {store = candidate}
            } catch {
                configNotice = "Configuration unavailable; original file preserved. \(error.localizedDescription)"
            }
            if arguments.contains("--herdr-socket") {configuration.source = source}
        }
        model = PanelModel(configuration: configuration, source: demoReader ?? SocketAgentSource(), store: store)
        if options.demo {try model.useDemoMetadata(DemoSource.directorySnapshot())}
        demo = demoReader
        notice = configNotice
        writable = configNotice == nil
    }
}
