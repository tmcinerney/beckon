import Foundation
import Combine

@MainActor public final class ConfigurationModel: ObservableObject {
    @Published public var endpoint: String
    @Published public private(set) var connectedEndpoint: String?
    @Published public private(set) var configuration: Configuration?
    @Published public private(set) var status: Status?
    @Published public private(set) var busy = false
    @Published public private(set) var error: String?
    @Published public private(set) var notice: String?
    @Published public private(set) var lastUpdated: Date?
    @Published public private(set) var stale = false
    private let transport: any Transport
    private let clock: () -> Date
    private var generation = UUID()
    private var operation: Task<Void, Never>?
    public static let freshnessInterval: TimeInterval = 15

    public init(endpoint: String = Endpoint.discover(), transport: any Transport = SocketTransport(),
                clock: @escaping () -> Date = Date.init) {
        self.endpoint = endpoint; self.transport = transport; self.clock = clock
    }
    public func isFresh(at now: Date) -> Bool {
        connectedEndpoint != nil && !stale && lastUpdated.map { now.timeIntervalSince($0) < Self.freshnessInterval } == true
    }
    public var canMutate: Bool { !busy && isFresh(at: clock()) && endpoint == connectedEndpoint }
    public var canRelease: Bool { canMutate && configuration?.capabilities?.binding_release_guard == true }
    public func health(at now: Date) -> String {
        guard connectedEndpoint != nil else { return configuration == nil ? "Disconnected" : "Disconnected · retained snapshot" }
        if busy { return "Contacting Beckon…" }
        return isFresh(at: now) ? "Connected · latest snapshot" : "Stale · refresh before editing"
    }
    public func disconnect() {
        generation = UUID(); operation?.cancel(); operation = nil
        connectedEndpoint = nil; busy = false; stale = configuration != nil
        notice = nil; error = nil
    }
    public func connect() async {
        disconnect()
        do { try Endpoint.validate(endpoint) } catch { self.error = error.localizedDescription; return }
        configuration = nil; status = nil; lastUpdated = nil
        connectedEndpoint = endpoint
        await refresh()
    }
    public func refresh() async {
        guard !busy, let path = connectedEndpoint else { return }
        await perform(path: path) { model, generation in
            try await model.load(path: path, generation: generation)
        }
    }
    private func load(path: String, generation: UUID) async throws {
        let config: Configuration = try await read(.configModel, path: path)
        try config.validate()
        guard self.generation == generation else { return }
        try Task.checkCancellation()
        let status: Status = try await read(.status, path: path)
        try status.validate()
        guard self.generation == generation else { return }
        // Both reads must succeed before publishing a new coherent client snapshot.
        configuration = config; self.status = status; lastUpdated = clock(); stale = false
    }
    private func read<T: Decodable>(_ request: Request, path: String) async throws -> T {
        let bytes = try await transport.send(request, to: path)
        return try JSONDecoder().decode(Reply<T>.self, from: bytes).value()
    }
    private func perform(path: String, work: @escaping @MainActor (ConfigurationModel, UUID) async throws -> Void) async {
        let run = generation
        busy = true; error = nil; notice = nil
        let task = Task { @MainActor [weak self] in
            guard let self else { return }
            do { try await work(self, run) }
            catch {
                guard self.generation == run else { return }
                self.error = error.localizedDescription; self.stale = true
            }
            if self.generation == run { self.busy = false }
        }
        operation = task
        await task.value
        if generation == run { operation = nil }
    }
    private func mutate(_ request: Request) async {
        guard canMutate, let path = connectedEndpoint else {
            error = "Connect and refresh this endpoint before making changes."; return
        }
        await perform(path: path) { model, generation in
            do {
                switch request {
                case .setTheme(let name):
                    let receipt: ThemeReceipt = try await model.read(request, path: path)
                    guard receipt.theme == name else { throw ClientError.invalid("Theme acknowledgement mismatch.") }
                case .bind(let key, let agent):
                    let receipt: BindReceipt = try await model.read(request, path: path)
                    guard receipt.key == key, receipt.session == agent.session, receipt.pane_id == agent.pane_id else {
                        throw ClientError.invalid("Binding acknowledgement mismatch.")
                    }
                case .releaseBinding(let key, _):
                    let receipt: ReleaseReceipt = try await model.read(request, path: path)
                    guard receipt.key == key else { throw ClientError.invalid("Release acknowledgement mismatch.") }
                default: throw ClientError.invalid("Not a configuration mutation.")
                }
            } catch let error as ClientError {
                if case .daemon = error { throw error }
                throw ClientError.unavailable("Change outcome unknown. Refresh before retrying. \(error.localizedDescription)")
            } catch {
                throw ClientError.unavailable("Change outcome unknown. Refresh before retrying. \(error.localizedDescription)")
            }
            guard model.generation == generation else { return }
            do { try await model.load(path: path, generation: generation) }
            catch { throw ClientError.unavailable("Daemon acknowledged the change, but verification failed. Refresh. \(error.localizedDescription)") }
            if model.generation == generation { model.notice = "Change acknowledged; snapshot refreshed." }
        }
    }
    public func setTheme(_ name: String) async {
        guard configuration?.display.themes[name] != nil else { error = "Select a configured theme."; return }
        await mutate(.setTheme(name))
    }
    @discardableResult public func setPresentation(_ change: PresentationChange, expectedRevision: String) async -> Bool {
        guard canMutate, let path = connectedEndpoint else {
            error = "Connect and refresh this endpoint before making changes."; return false
        }
        guard !expectedRevision.isEmpty, configuration?.revision == expectedRevision else {
            error = "Appearance draft is stale. Refresh and review it before saving."; return false
        }
        guard configuration?.supports(change) == true else {
            error = "This presentation change is not supported by the current configuration and capabilities."; return false
        }
        do { try change.validate() } catch { self.error = error.localizedDescription; return false }
        var saved = false
        await perform(path: path) { model, generation in
            let updated: Configuration
            do {
                updated = try await model.read(.setPresentation(expectedRevision: expectedRevision, change: change), path: path)
                try updated.validate()
                guard let revision = updated.revision, !revision.isEmpty else {
                    throw ClientError.invalid("Presentation acknowledgement is missing its revision.")
                }
            } catch let error as ClientError {
                if case .daemon = error { throw error }
                throw ClientError.unavailable("Presentation outcome unknown. Refresh before retrying. \(error.localizedDescription)")
            } catch {
                throw ClientError.unavailable("Presentation outcome unknown. Refresh before retrying. \(error.localizedDescription)")
            }
            guard model.generation == generation else { return }
            // AIDEV-NOTE: Use the daemon's canonical response, not the draft. This does not
            // refresh agent status, so preserve the binding snapshot's freshness deadline.
            model.configuration = updated
            model.notice = "Presentation saved on host; physical LED output is not confirmed."
            saved = true
        }
        return saved
    }
    public func bind(key: String, agent: AgentIdentity) async {
        guard configuration?.bindings.contains(key) == true,
              status?.panes.contains(where: { $0.id == agent }) == true else {
            error = "Select a configured key and an exact source-qualified agent."; return
        }
        guard status?.bindings.contains(where: { $0.key == key }) == false else {
            error = "Key \(key) is occupied. Release its owner explicitly first."; return
        }
        guard status?.bindings.contains(where: {
            $0.source == agent.source && $0.session == agent.session && $0.pane.pane_id == agent.pane_id
        }) == false else {
            error = "This agent already owns a key. Release that binding explicitly before moving it."; return
        }
        await mutate(.bind(key: key, agent: agent))
    }
    public func release(key: String, reference: AgentIdentity, endpoint: String) async {
        guard canRelease else { error = "Guarded release is unavailable. Connect to a supporting daemon and refresh."; return }
        guard endpoint == connectedEndpoint, endpoint == self.endpoint else {
            error = "Release selection belongs to a different endpoint. Select its owner again."; return
        }
        guard configuration?.bindings.contains(key) == true,
              status?.bindings.contains(where: { $0.key == key && $0.reference == reference }) == true else {
            error = "Binding owner changed or is not observed. Refresh and select its owner again."; return
        }
        await mutate(.releaseBinding(key: key, reference: reference))
    }
}
