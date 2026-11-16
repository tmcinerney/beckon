import Foundation
import Combine

@MainActor public final class ConfigurationModel: ObservableObject {
    @Published public var endpoint: String
    @Published public private(set) var connectedEndpoint: String?
    @Published public private(set) var configuration: Configuration?
    @Published public private(set) var status: Status?
    @Published public private(set) var runtime: RuntimeSnapshot?
    @Published public private(set) var runtimeReceivedAt: Date?
    @Published public private(set) var runtimeError: String?
    @Published public private(set) var busy = false
    @Published public private(set) var error: String?
    @Published public private(set) var notice: String?
    @Published public private(set) var lastUpdated: Date?
    @Published public private(set) var stale = false
    private let transport: any Transport
    private let clock: () -> Date
    private let scheduler: any ClientRefreshScheduler
    private var generation = UUID()
    private var operation: Task<Void, Never>?
    private var automaticOperation = false
    private var polling: Task<Void, Never>?
    private var drainBarrier: Task<Void, Never>?
    private var awaitingConfigurationRevision: String?
    private var writeSampleFence: (instance: String, sequence: UInt64)?
    private var runtimeExpectedMutation: Request?
    private var runtimeDisabled = false
    private struct MutationIntent {
        let generation: UUID
        let endpoint: String?
    }
    private func mutationIntent() -> MutationIntent { MutationIntent(generation: generation, endpoint: connectedEndpoint) }
    private func matches(_ intent: MutationIntent) -> Bool {
        intent.generation == generation && intent.endpoint != nil
            && intent.endpoint == connectedEndpoint && intent.endpoint == endpoint
    }
    public static let freshnessInterval: TimeInterval = 15
    /// Presentation sampling cadence; unrelated to declarative device gesture timing.
    public static let clientRefreshInterval: TimeInterval = 2

    public init(endpoint: String = Endpoint.discover(), transport: any Transport = SocketTransport(),
                clock: @escaping () -> Date = Date.init, scheduler: any ClientRefreshScheduler = ContinuousRefreshScheduler()) {
        self.endpoint = endpoint; self.transport = transport; self.clock = clock; self.scheduler = scheduler
    }
    deinit { polling?.cancel(); operation?.cancel() }
    public func isFresh(at now: Date) -> Bool {
        guard connectedEndpoint != nil, !stale,
              lastUpdated.map({ (0..<Self.freshnessInterval).contains(now.timeIntervalSince($0)) }) == true else { return false }
        if let runtime, let runtimeReceivedAt {
            return runtime.configuration_revision == configuration?.revision
                && RuntimeEvidence(snapshot: runtime, receivedAt: runtimeReceivedAt, now: now).fresh()
        }
        return true
    }
    public var canMutate: Bool { !busy && writeSampleFence == nil && isFresh(at: clock()) && endpoint == connectedEndpoint }
    public var canRelease: Bool {
        canMutate && configuration?.capabilities?.binding_release_guard == true
            && (runtime == nil || runtime?.ledger_available == true)
    }
    public func health(at now: Date) -> String {
        healthSummary(at: now).text
    }
    public func healthSummary(at now: Date) -> HealthSummary {
        HealthSummary.make(connected: connectedEndpoint != nil, stale: stale, snapshot: runtime, receivedAt: runtimeReceivedAt, now: now)
    }
    public func bindingLegend(at now: Date) -> [BindingLegendRow] {
        guard let runtime, let runtimeReceivedAt else { return [] }
        return BindingLegendRow.rows(snapshot: runtime, receivedAt: runtimeReceivedAt, now: now,
                                     retained: connectedEndpoint == nil || stale || runtime.configuration_revision != configuration?.revision)
    }
    public func bindCandidates(at now: Date) -> [Agent] {
        guard let runtime, let runtimeReceivedAt else { return connectedEndpoint == nil || stale ? [] : status?.panes ?? [] }
        guard connectedEndpoint != nil, !stale else { return [] }
        let evidence = RuntimeEvidence(snapshot: runtime, receivedAt: runtimeReceivedAt, now: now)
        let sources = Dictionary(uniqueKeysWithValues: runtime.sources.map { ($0.id, $0) })
        return runtime.panes.filter { pane in
            guard let source = sources[pane.reference.source] else { return false }
            return pane.availability == .live && evidence.fresh(pane.age_ms)
                && source.availability == .available && evidence.fresh(source.observed_age_ms)
        }.map { pane in
            Agent(source: pane.reference.source, session: pane.reference.session, pane_id: pane.reference.pane_id,
                  title: "\(pane.scope_title ?? "Workspace/session title unavailable") · \(pane.title ?? "Block/pane title unavailable")\(pane.labels_stale || !evidence.fresh(pane.labels_age_ms) ? " · labels stale" : "")",
                  binding: "", agent_status: pane.agent_status, focused: pane.focused)
        }
    }
    public func disconnect() {
        generation = UUID()
        let oldPoll = polling, oldOperation = operation, priorDrain = drainBarrier
        oldPoll?.cancel(); oldOperation?.cancel(); polling = nil; operation = nil; automaticOperation = false
        drainBarrier = Task {
            await priorDrain?.value
            await oldOperation?.value
            await oldPoll?.value
        }
        connectedEndpoint = nil; busy = false; stale = configuration != nil
        notice = nil; error = nil
    }
    public func connect() async {
        disconnect()
        let run = generation
        await drainBarrier?.value
        guard generation == run else { return }
        do { try Endpoint.validate(endpoint) } catch { self.error = error.localizedDescription; return }
        configuration = nil; status = nil; lastUpdated = nil; runtime = nil; runtimeReceivedAt = nil; awaitingConfigurationRevision = nil
        writeSampleFence = nil
        runtimeExpectedMutation = nil
        runtimeDisabled = false
        connectedEndpoint = endpoint
        await refresh()
        guard generation == run else { return }
        startPolling()
    }
    private func startPolling() {
        guard polling == nil, connectedEndpoint != nil else { return }
        let run = generation, scheduler = self.scheduler
        polling = Task { @MainActor [weak self] in
            while !Task.isCancelled {
                do { try await scheduler.wait(seconds: Self.clientRefreshInterval) }
                catch { return }
                guard !Task.isCancelled else { return }
                await self?.pollOnce(generation: run)
                if self == nil { return }
            }
        }
    }
    private func pollOnce(generation run: UUID) async {
        guard generation == run, !busy, let path = connectedEndpoint else { return }
        await perform(path: path, automatic: true) { model, generation in
            if !model.runtimeDisabled, model.configuration?.capabilities?.runtime_snapshot_versions?.contains(1) == true {
                let sample: RuntimeSnapshot = try await model.read(.runtimeSnapshot, path: path)
                try sample.validate()
                try Task.checkCancellation()
                guard model.generation == generation else { return }
                try model.accept(sample, explicit: false)
            } else {
                let config: Configuration = try await model.read(.configModel, path: path)
                try config.validate()
                try Task.checkCancellation()
                guard model.generation == generation else { return }
                model.configuration = config
            }
        }
    }
    public func refresh() async {
        await drainAutomaticRead()
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
        if !runtimeDisabled, config.capabilities?.runtime_snapshot_versions?.contains(1) == true {
            // Bootstrap is display-only. A runtime failure cannot enable unsafe legacy status fallback.
            if runtime == nil { configuration = config }
            do {
                let sample: RuntimeSnapshot = try await read(.runtimeSnapshot, path: path)
                try sample.validate()
                guard self.generation == generation else { return }
                try accept(sample, explicit: true)
                lastUpdated = runtimeReceivedAt
            } catch {
                guard self.generation == generation else { return }
                runtimeError = "Runtime unavailable: \(error.localizedDescription)"; stale = true
                if case ClientError.daemon = error { runtimeDisabled = true }
                throw error
            }
            return
        }
        if runtimeExpectedMutation != nil {
            throw ClientError.unavailable("Coherent runtime readback is unavailable; the requested change remains unverified. Reconnect and review before retrying.")
        }
        if runtimeDisabled {
            configuration = config; status = nil; stale = true
            return
        }
        runtime = nil; runtimeReceivedAt = nil; runtimeError = "Runtime diagnostics unavailable on this daemon."
        var status: Status?
        // AIDEV-NOTE: Old status() reconciles/writes. Capability absence must be config-only.
        if config.capabilities?.status_read_only == true {
            status = try await read(.status, path: path)
            try status?.validate()
        }
        guard self.generation == generation else { return }
        // This legacy projection is not a daemon-side transaction or a complete ledger.
        configuration = config; self.status = status; lastUpdated = clock(); stale = false
    }
    private func accept(_ sample: RuntimeSnapshot, explicit: Bool) throws {
        if explicit, let fence = writeSampleFence,
           sample.instance_id != fence.instance || sample.sequence <= fence.sequence {
            throw ClientError.unavailable("Cached runtime sample predates the change. A newer sample must verify its outcome; refresh before retrying.")
        }
        if explicit { try verifyExpectedMutation(in: sample) }
        if let awaitingConfigurationRevision, sample.configuration_revision != awaitingConfigurationRevision {
            throw ClientError.unavailable("Runtime has not observed the acknowledged appearance revision. Refresh before editing.")
        }
        if let runtime, runtime.instance_id != sample.instance_id, !explicit { stale = true; lastUpdated = nil }
        if let runtime, runtime.instance_id == sample.instance_id {
            guard sample.sequence >= runtime.sequence else { throw ClientError.invalid("Out-of-order runtime sample rejected.") }
            // A cached sample cannot gain evidence merely because its socket reply was received again.
            if sample.sequence == runtime.sequence {
                if explicit { stale = false; runtimeError = nil; writeSampleFence = nil; runtimeExpectedMutation = nil }
                return
            }
        }
        configuration = sample.configuration; runtime = sample; runtimeReceivedAt = clock()
        awaitingConfigurationRevision = nil
        status = nil; runtimeError = nil
        if explicit { stale = false; writeSampleFence = nil; runtimeExpectedMutation = nil }
    }
    private func verifyExpectedMutation(in sample: RuntimeSnapshot) throws {
        guard let expected = runtimeExpectedMutation else { return }
        let now = clock()
        guard RuntimeEvidence(snapshot: sample, receivedAt: now, now: now).fresh() else {
            throw ClientError.unavailable("A stale runtime sample cannot verify the requested change. Refresh before retrying.")
        }
        // AIDEV-NOTE: A sequence newer than the client's last read may still predate the write.
        // Require the requested host state, not ordering alone; ordinary polls never resolve this fence.
        switch expected {
        case .setTheme(let name):
            guard sample.configuration.display.theme == name else {
                throw ClientError.unavailable("Requested active theme is not observed in the coherent runtime sample. Refresh before retrying.")
            }
        case .bind(let key, let reference):
            guard sample.ledger_available, let row = sample.bindings.first(where: { $0.key == key }), row.owner == reference else {
                throw ClientError.unavailable("Exact source-qualified binding owner is not verified by the runtime ledger. Refresh and review ownership before retrying.")
            }
        case .releaseBinding(let key, let reference):
            guard sample.ledger_available, let row = sample.bindings.first(where: { $0.key == key }) else {
                throw ClientError.unavailable("Binding ownership is unavailable; release cannot be verified. Refresh before retrying.")
            }
            if let owner = row.owner, owner != reference {
                throw ClientError.unavailable("Binding owner changed during release: \(owner.description). Review this conflict; release is not verified.")
            }
            guard row.owner == nil, row.availability == .free else {
                throw ClientError.unavailable("Expected binding owner removal is not verified by the runtime ledger. Refresh before retrying.")
            }
        default: throw ClientError.invalid("Unsupported mutation verification.")
        }
    }
    private func read<T: Decodable>(_ request: Request, path: String) async throws -> T {
        let bytes = try await transport.send(request, to: path)
        return try JSONDecoder().decode(Reply<T>.self, from: bytes).value()
    }
    private func perform(path: String, automatic: Bool = false, intent: MutationIntent? = nil,
                         work: @escaping @MainActor (ConfigurationModel, UUID) async throws -> Void) async {
        if let intent, !matches(intent) || intent.endpoint != path { return }
        guard operation == nil else { return }
        let run = generation
        busy = true; automaticOperation = automatic
        if !automatic { error = nil; notice = nil }
        let task = Task { @MainActor [weak self] in
            guard let self else { return }
            do {
                try Task.checkCancellation()
                guard self.generation == run else { return }
                try await work(self, run)
            }
            catch {
                guard self.generation == run else { return }
                if !Task.isCancelled {
                    if automatic {
                        self.runtimeError = "Runtime refresh failed: \(error.localizedDescription)"
                        if case ClientError.daemon = error { self.runtimeDisabled = true }
                    }
                    else { self.error = error.localizedDescription }
                    self.stale = true
                }
            }
            if self.generation == run { self.busy = false; self.operation = nil; self.automaticOperation = false }
        }
        operation = task
        await task.value
    }
    private func drainAutomaticRead() async {
        if automaticOperation, let operation {
            operation.cancel()
            await operation.value
            // A cancellation-oblivious transport may finish, but no new request starts before it drains.
        }
    }
    private func mutate(_ request: Request, intent: MutationIntent) async {
        guard matches(intent) else { return }
        await drainAutomaticRead()
        guard matches(intent) else { return }
        guard canMutate, let path = connectedEndpoint else {
            error = "Connect and refresh this endpoint before making changes."; return
        }
        await perform(path: path, intent: intent) { model, generation in
            // AIDEV-NOTE: Reply fences are not intent fences. Never retarget a queued user write after a drain.
            guard model.matches(intent) else { return }
            if let runtime = model.runtime {
                model.writeSampleFence = (runtime.instance_id, runtime.sequence)
                model.runtimeExpectedMutation = request
            }
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
        let intent = mutationIntent()
        guard configuration?.display.themes[name] != nil else { error = "Select a configured theme."; return }
        await mutate(.setTheme(name), intent: intent)
    }
    @discardableResult public func setPresentation(_ change: PresentationChange, expectedRevision: String) async -> Bool {
        let intent = mutationIntent()
        await drainAutomaticRead()
        guard matches(intent) else { return false }
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
        await perform(path: path, intent: intent) { model, generation in
            guard model.matches(intent) else { return }
            if let runtime = model.runtime { model.writeSampleFence = (runtime.instance_id, runtime.sequence) }
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
            if model.runtime != nil { model.awaitingConfigurationRevision = updated.revision }
            model.notice = "Presentation saved on host; physical LED output is not confirmed."
            saved = true
        }
        return saved && matches(intent)
    }
    public func bind(key: String, agent: AgentIdentity) async {
        let intent = mutationIntent()
        await drainAutomaticRead()
        guard matches(intent) else { return }
        guard configuration?.bindings.contains(key) == true,
               bindCandidates(at: clock()).contains(where: { $0.id == agent }) else {
            error = "Select a configured key and an exact source-qualified agent."; return
        }
        if runtime != nil {
            let rows = bindingLegend(at: clock())
            guard rows.first(where: { $0.key == key })?.availability == .free else {
                error = "Key \(key) is occupied or ownership is unknown. Refresh and release its exact owner explicitly first."; return
            }
            guard !rows.contains(where: { $0.owner == agent }) else {
                error = "This agent already owns a key. Release that binding explicitly before moving it."; return
            }
        }
        if runtime == nil {
            guard status?.bindings.contains(where: { $0.key == key }) == false else {
                error = "Key \(key) is occupied. Release its owner explicitly first."; return
            }
            guard status?.bindings.contains(where: {
                $0.source == agent.source && $0.session == agent.session && $0.pane.pane_id == agent.pane_id
            }) == false else {
                error = "This agent already owns a key. Release that binding explicitly before moving it."; return
            }
        }
        await mutate(.bind(key: key, agent: agent), intent: intent)
    }
    public func release(key: String, reference: AgentIdentity, endpoint: String) async {
        let intent = mutationIntent()
        await drainAutomaticRead()
        guard matches(intent) else { return }
        guard canRelease else { error = "Guarded release is unavailable. Connect to a supporting daemon and refresh."; return }
        guard endpoint == connectedEndpoint, endpoint == self.endpoint else {
            error = "Release selection belongs to a different endpoint. Select its owner again."; return
        }
        let runtimeOwner = runtime?.ledger_available == true && bindingLegend(at: clock()).contains { $0.key == key && $0.owner == reference }
        guard configuration?.bindings.contains(key) == true,
               runtimeOwner || status?.bindings.contains(where: { $0.key == key && $0.reference == reference }) == true else {
            error = "Binding owner changed or is not observed. Refresh and select its owner again."; return
        }
        await mutate(.releaseBinding(key: key, reference: reference), intent: intent)
    }
}
