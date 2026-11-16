import XCTest
@testable import BeckonConfiguration

actor ManualRefreshScheduler: ClientRefreshScheduler {
    private var pending: [UUID: CheckedContinuation<Void, Error>] = [:]
    private(set) var intervals: [TimeInterval] = []
    func wait(seconds: TimeInterval) async throws {
        let id = UUID()
        try await withTaskCancellationHandler(operation: {
            try Task.checkCancellation()
            try await withCheckedThrowingContinuation { pending[id] = $0; intervals.append(seconds) }
            try Task.checkCancellation()
        }, onCancel: { Task { await self.cancel(id) } })
    }
    private func cancel(_ id: UUID) { pending.removeValue(forKey: id)?.resume(throwing: CancellationError()) }
    func advance() { let work = pending; pending.removeAll(); for continuation in work.values { continuation.resume() } }
    var pendingCount: Int { pending.count }
}
@MainActor func settleTasks() async { for _ in 0..<100 { await Task.yield() } }

final class PollingTests: XCTestCase {
    @MainActor func testReleasingSharedModelCancelsItsScheduledPoll() async throws {
        let scheduler = ManualRefreshScheduler()
        var model: ConfigurationModel? = ConfigurationModel(endpoint: "/synthetic.sock",
            transport: SyntheticRuntimeTransport(snapshot: try syntheticSnapshot()), scheduler: scheduler)
        await model?.connect(); await settleTasks()
        weak var released = model
        model = nil; await settleTasks()
        XCTAssertNil(released)
        let pending = await scheduler.pendingCount
        XCTAssertEqual(pending, 0)
    }

    @MainActor func testRuntimeProtocolErrorDisablesOnlyRuntimeWithoutUnsafeFallback() async throws {
        let transport = RuntimeProtocolErrorTransport(), scheduler = ManualRefreshScheduler()
        let model = ConfigurationModel(endpoint: "/synthetic.sock", transport: transport, scheduler: scheduler)
        await model.connect(); await settleTasks()
        await scheduler.advance(); await settleTasks(); await scheduler.advance(); await settleTasks()
        let requests = await transport.requests
        XCTAssertEqual(requests.filter { $0 == .runtimeSnapshot }.count, 1)
        XCTAssertFalse(requests.contains(.status))
        XCTAssertNotNil(model.configuration)
        XCTAssertNil(model.runtime)
        XCTAssertEqual(model.healthSummary(at: Date()).level, .unavailable)
        model.disconnect()
    }

    @MainActor func testPollingDoesNotRenewVerificationDeadlineOrEraseUnknownWriteOutcome() async throws {
        var now = Date(timeIntervalSince1970: 100)
        let transport = SyntheticRuntimeTransport(snapshot: try syntheticSnapshot()), scheduler = ManualRefreshScheduler()
        let model = ConfigurationModel(endpoint: "/synthetic.sock", transport: transport, clock: { now }, scheduler: scheduler)
        await model.connect(); await settleTasks()
        await model.setTheme("day") // Synthetic transport deliberately returns a malformed write receipt.
        let writeError = model.error
        XCTAssertTrue(writeError?.contains("outcome unknown") == true)
        now.addTimeInterval(16)
        await transport.replace(try syntheticSnapshot(sequence: 8)); await scheduler.advance(); await settleTasks()
        XCTAssertEqual(model.runtime?.sequence, 8)
        XCTAssertEqual(model.error, writeError)
        XCTAssertFalse(model.canMutate)
        XCTAssertEqual(model.lastUpdated, Date(timeIntervalSince1970: 100))
        let requests = await transport.requests
        XCTAssertEqual(requests.filter { if case .setTheme = $0 { return true }; return false }.count, 1)
        model.disconnect()
    }

    @MainActor func testSameCachedSequenceCannotRefreshItsEvidenceAge() async throws {
        var now = Date(timeIntervalSince1970: 100)
        let transport = SyntheticRuntimeTransport(snapshot: try syntheticSnapshot()), scheduler = ManualRefreshScheduler()
        let model = ConfigurationModel(endpoint: "/synthetic.sock", transport: transport, clock: { now }, scheduler: scheduler)
        await model.connect(); await settleTasks(); now.addTimeInterval(16)
        await scheduler.advance(); await settleTasks()
        XCTAssertEqual(model.runtimeReceivedAt, Date(timeIntervalSince1970: 100))
        XCTAssertFalse(model.canMutate)
        XCTAssertEqual(model.healthSummary(at: now).level, .stale)
        model.disconnect()
    }

    @MainActor func testLegacyPollingIsConfigOnlyAndHasNoStatusOrAutomaticWrites() async {
        let transport = MockTransport(), scheduler = ManualRefreshScheduler()
        await transport.setLegacy()
        let model = ConfigurationModel(endpoint: "/synthetic.sock", transport: transport, scheduler: scheduler)
        await model.connect(); await settleTasks(); await scheduler.advance(); await settleTasks()
        let requests = await transport.requests
        XCTAssertEqual(requests, [.configModel, .configModel])
        XCTAssertNil(model.status); XCTAssertNil(model.runtime)
        XCTAssertEqual(model.healthSummary(at: Date()).level, .unavailable)
        model.disconnect()
    }

    @MainActor func testSlowPollHasNoOverlapOrCatchUpAndManualRefreshDrainsIt() async throws {
        let transport = try SuspendedRuntimeTransport(), scheduler = ManualRefreshScheduler()
        let model = ConfigurationModel(endpoint: "/synthetic.sock", transport: transport, scheduler: scheduler)
        await model.connect(); await settleTasks()
        await transport.suspendNextRead(); await scheduler.advance(); await settleTasks()
        await scheduler.advance(); await scheduler.advance(); await settleTasks()
        let before = await transport.requests
        XCTAssertEqual(before.count, 3)
        let refresh = Task { await model.refresh() }; await settleTasks()
        let draining = await transport.requests
        XCTAssertEqual(draining.count, 3)
        await transport.completeRead(); await refresh.value; await settleTasks()
        let concurrency = await transport.maximumConcurrency
        XCTAssertEqual(concurrency, 1)
        model.disconnect()
    }

    @MainActor func testDaemonInstanceChangeRequiresExplicitVerificationBeforeAnotherWrite() async throws {
        let transport = SyntheticRuntimeTransport(snapshot: try syntheticSnapshot()), scheduler = ManualRefreshScheduler()
        let model = ConfigurationModel(endpoint: "/synthetic.sock", transport: transport, scheduler: scheduler)
        await model.connect(); await settleTasks()
        await transport.replace(try syntheticSnapshot(sequence: 1, instance: "new-synthetic-boot"))
        await scheduler.advance(); await settleTasks()
        XCTAssertEqual(model.runtime?.instance_id, "new-synthetic-boot")
        XCTAssertFalse(model.canMutate)
        model.disconnect()
    }

    @MainActor func testPollingCannotRollBackAcknowledgedAppearanceOrAdvanceDraftRevision() async throws {
        let transport = try SyntheticPresentationTransport(), scheduler = ManualRefreshScheduler()
        let model = ConfigurationModel(endpoint: "/synthetic.sock", transport: transport, scheduler: scheduler)
        await model.connect(); await settleTasks()
        var draft = AppearanceDraft(Feedback(held: Treatment(color: "#FFFFFF", brightness: 0.3, motion: .steady)), revision: "atomic-r2")
        draft.value.pending = Treatment(color: "#FFFFFF", brightness: 0.12, motion: .steady)
        let saved = await model.setPresentation(.deviceEffectV2(device: "desk", effect: "red", effects: draft.value), expectedRevision: "atomic-r2")
        XCTAssertTrue(saved)
        await transport.publishOldRevisionAtNewSequence(); await scheduler.advance(); await settleTasks()
        XCTAssertEqual(model.configuration?.revision, "saved-r3")
        XCTAssertFalse(model.canMutate)
        XCTAssertEqual(draft.revision, "atomic-r2")
        XCTAssertFalse(draft.matches(model.configuration?.revision))
        model.disconnect()
    }

    @MainActor func testReconnectDrainsOldGenerationEvenWhenTransportIgnoresCancellation() async throws {
        let transport = try SuspendedRuntimeTransport(), scheduler = ManualRefreshScheduler()
        let model = ConfigurationModel(endpoint: "/synthetic.sock", transport: transport, scheduler: scheduler)
        await model.connect(); await settleTasks()
        await transport.suspendNextRead(); await scheduler.advance(); await settleTasks()
        let reconnect = Task { await model.connect() }
        await settleTasks()
        let drainingRequests = await transport.requests
        XCTAssertEqual(drainingRequests.count, 3, "Reconnect must not issue a request while the old transport is draining")
        await transport.completeRead(); await reconnect.value; await settleTasks()
        let maximum = await transport.maximumConcurrency
        XCTAssertEqual(maximum, 1)
        XCTAssertEqual(model.runtime?.instance_id, "synthetic-boot")
        model.disconnect()
    }

    @MainActor func testMutationDrainsPollingReadBeforeDispatchWithoutDroppingTheUsersWrite() async throws {
        let transport = try SuspendedRuntimeTransport()
        let scheduler = ManualRefreshScheduler()
        let model = ConfigurationModel(endpoint: "/synthetic.sock", transport: transport, scheduler: scheduler)
        await model.connect(); await settleTasks()
        await transport.suspendNextRead(); await scheduler.advance(); await settleTasks()
        let write = Task { await model.setTheme("day") }
        await settleTasks()
        let before = await transport.writeCount
        XCTAssertEqual(before, 0)
        await transport.completeRead(); await write.value; await settleTasks()
        let after = await transport.writeCount, concurrency = await transport.maximumConcurrency
        XCTAssertEqual(after, 1, "The explicit write must wait, not be dropped because a poll is busy")
        XCTAssertEqual(concurrency, 1)
        XCTAssertEqual(model.configuration?.display.theme, "day")
        model.disconnect()
    }

    @MainActor func testOnlyExplicitConnectStartsOneReadOnlyPollingLoop() async throws {
        let transport = SyntheticRuntimeTransport(snapshot: try syntheticSnapshot())
        let scheduler = ManualRefreshScheduler()
        let model = ConfigurationModel(endpoint: "/synthetic.sock", transport: transport, scheduler: scheduler)
        await settleTasks()
        let initial = await transport.requests
        XCTAssertEqual(initial, [])
        await model.connect(); await settleTasks()
        _ = model.health(at: Date()); _ = model.health(at: Date())
        await scheduler.advance(); await settleTasks()
        let requests = await transport.requests
        XCTAssertEqual(requests.count, 3, "Bootstrap + atomic runtime + one scheduled read")
        XCTAssertFalse(requests.contains(.status))
        let intervals = await scheduler.intervals
        XCTAssertEqual(intervals.first, 2)
        let pending = await scheduler.pendingCount
        XCTAssertEqual(pending, 1)
        model.disconnect(); await settleTasks()
        await scheduler.advance(); await settleTasks()
        let final = await transport.requests
        XCTAssertEqual(final.count, requests.count)
    }
}

actor RuntimeProtocolErrorTransport: Transport {
    private(set) var requests: [Request] = []
    func send(_ request: Request, to endpoint: String) async throws -> Data {
        requests.append(request)
        if request == .configModel { return try syntheticReply(syntheticConfiguration()) }
        return Data(#"{"type":"error","message":"synthetic protocol rejection; do not parse this string"}"#.utf8)
    }
}

actor SyntheticPresentationTransport: Transport {
    private var snapshot: [String: Any]
    private var saved: Configuration?
    init() throws { snapshot = try syntheticSnapshot() }
    func publishOldRevisionAtNewSequence() { snapshot["sequence"] = 8 }
    func send(_ request: Request, to endpoint: String) async throws -> Data {
        switch request {
        case .configModel: return try syntheticReply(snapshot["configuration"] as! [String: Any])
        case .runtimeSnapshot: return try syntheticReply(snapshot)
        case .setPresentation(_, let change):
            var config = try JSONDecoder().decode(Configuration.self, from: JSONSerialization.data(withJSONObject: snapshot["configuration"]!))
            if case .deviceEffectV2(_, let effect, let effects) = change { config.devices[0].definition.effects[effect] = effects }
            config.revision = "saved-r3"; saved = config
            return ok(String(decoding: try JSONEncoder().encode(config), as: UTF8.self))
        default: throw ClientError.invalid("Unexpected synthetic request")
        }
    }
}

// SYNTHETIC suspended transport deliberately ignores cancellation until the test drains it.
actor SuspendedRuntimeTransport: Transport {
    private var snapshot: [String: Any]
    private var suspension: CheckedContinuation<Void, Never>?
    private var shouldSuspend = false
    private var active = 0
    private(set) var maximumConcurrency = 0
    private(set) var writeCount = 0
    private(set) var requests: [Request] = []
    init() throws { snapshot = try syntheticSnapshot() }
    func suspendNextRead() { shouldSuspend = true }
    func completeRead() { suspension?.resume(); suspension = nil }
    func send(_ request: Request, to endpoint: String) async throws -> Data {
        requests.append(request); active += 1; maximumConcurrency = max(maximumConcurrency, active)
        defer { active -= 1 }
        switch request {
        case .configModel: return try syntheticReply(snapshot["configuration"] as! [String: Any])
        case .runtimeSnapshot:
            if shouldSuspend {
                shouldSuspend = false
                await withCheckedContinuation { suspension = $0 }
            }
            return try syntheticReply(snapshot)
        case .setTheme(let theme):
            writeCount += 1
            var config = snapshot["configuration"] as! [String: Any]
            var display = config["display"] as! [String: Any]; display["theme"] = theme; config["display"] = display
            snapshot["configuration"] = config; snapshot["sequence"] = 8
            return ok("{\"theme\":\"\(theme)\"}")
        default: throw ClientError.invalid("Unexpected synthetic request")
        }
    }
}
