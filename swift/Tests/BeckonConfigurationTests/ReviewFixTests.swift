import XCTest
@testable import BeckonConfiguration

// SYNTHETIC consumer-only fixtures. No Rust wire compatibility or live endpoint is implied.
final class MutationIntentReviewTests: XCTestCase {
    @MainActor func testQueuedIntentCannotAdoptSamePathReplacementGeneration() async throws {
        try await checkReplacementIntent(endpoint: "/synthetic-A.sock")
    }

    @MainActor func testQueuedIntentCannotAdoptDifferentPathWithIdenticalTargetsAndRevision() async throws {
        try await checkReplacementIntent(endpoint: "/synthetic-B.sock")
    }

    @MainActor private func checkReplacementIntent(endpoint: String) async throws {
        for action in QueuedIntentFixture.Action.allCases {
            let fixture = try QueuedIntentFixture(action: action), scheduler = ManualRefreshScheduler()
            let model = ConfigurationModel(endpoint: "/synthetic-A.sock", transport: fixture,
                                           clock: { Date(timeIntervalSince1970: 100) }, scheduler: scheduler)
            await model.connect(); await settleTasks()
            fixture.suspendNextPoll = true
            await scheduler.advance(); await fixture.waitForSuspension()
            let queued = Task(priority: .background) { await fixture.invoke(action, on: model) }
            await settleTasks()
            model.endpoint = endpoint
            let reconnect = Task(priority: .high) { await model.connect() }
            await settleTasks()
            XCTAssertTrue(fixture.writes.isEmpty)
            fixture.completePoll()
            await reconnect.value; await queued.value
            XCTAssertEqual(model.connectedEndpoint, endpoint)
            XCTAssertTrue(fixture.writes.isEmpty, "Old queued \(action) must not dispatch on the replacement generation")
            await fixture.invoke(action, on: model)
            XCTAssertEqual(fixture.writes.count, 1, "A new-generation \(action) intent should still dispatch")
            XCTAssertEqual(fixture.writes.first?.endpoint, endpoint)
            XCTAssertNotNil(model.notice, "The newly initiated \(action) must complete successfully")
            model.disconnect()
        }
    }
}

@MainActor private final class QueuedIntentFixture: Transport {
    enum Action: CaseIterable { case theme, appearance, bind, release }
    struct Write { let request: Request; let endpoint: String }
    private var snapshot: [String: Any]
    private var poll: CheckedContinuation<Void, Never>?
    private var suspensionObserver: CheckedContinuation<Void, Never>?
    var suspendNextPoll = false
    private(set) var writes: [Write] = []
    init(action: Action) throws {
        snapshot = try syntheticSnapshot()
        snapshot["bindings"] = [["key": "key1", "configured": true,
                                 "owner": action == .release ? ["source": "rex", "session": "work", "pane_id": "same"] as Any : NSNull(),
                                 "availability": action == .release ? "live" : "free", "scope_title": NSNull(), "title": NSNull(),
                                 "labels_stale": false, "age_ms": 0, "labels_age_ms": 0]]
        snapshot["panes"] = [["reference": ["source": "rex", "session": "work", "pane_id": "same"],
                              "scope_title": "Scope", "title": "Task", "agent_status": "idle", "focused": false,
                              "availability": "live", "labels_stale": false, "age_ms": 0, "labels_age_ms": 0]]
        snapshot["sources"] = [["id": "rex", "availability": "available", "observed_age_ms": 0,
                                "last_success_age_ms": 0, "last_error": NSNull()]]
    }
    func invoke(_ action: Action, on model: ConfigurationModel) async {
        switch action {
        case .theme: await model.setTheme("day")
        case .appearance:
            _ = await model.setPresentation(.deviceEffectV2(device: "desk", effect: "red",
                effects: Feedback(held: Treatment(color: "#FFFFFF", brightness: 0.2, motion: .steady))), expectedRevision: "atomic-r2")
        case .bind: await model.bind(key: "key1", agent: identity)
        case .release: await model.release(key: "key1", reference: identity, endpoint: model.endpoint)
        }
    }
    func waitForSuspension() async {
        if poll == nil { await withCheckedContinuation { suspensionObserver = $0 } }
    }
    func completePoll() { poll?.resume(); poll = nil }
    func send(_ request: Request, to endpoint: String) async throws -> Data {
        switch request {
        case .configModel: return try syntheticReply(snapshot["configuration"] as! [String: Any])
        case .runtimeSnapshot:
            if suspendNextPoll {
                suspendNextPoll = false
                await withCheckedContinuation { continuation in
                    poll = continuation; suspensionObserver?.resume(); suspensionObserver = nil
                }
            }
            return try syntheticReply(snapshot)
        default:
            writes.append(Write(request: request, endpoint: endpoint))
            snapshot["sequence"] = 8
            switch request {
            case .setTheme(let name):
                var config = snapshot["configuration"] as! [String: Any], display = (snapshot["configuration"] as! [String: Any])["display"] as! [String: Any]
                display["theme"] = name; config["display"] = display; snapshot["configuration"] = config
                return ok("{\"theme\":\"\(name)\"}")
            case .setPresentation(_, let change):
                var config = try JSONDecoder().decode(Configuration.self, from: JSONSerialization.data(withJSONObject: snapshot["configuration"]!))
                if case .deviceEffectV2(_, let effect, let value) = change { config.devices[0].definition.effects[effect] = value }
                config.revision = "appearance-r3"
                snapshot["configuration"] = try JSONSerialization.jsonObject(with: JSONEncoder().encode(config))
                snapshot["configuration_revision"] = "appearance-r3"
                return ok(String(decoding: try JSONEncoder().encode(config), as: UTF8.self))
            case .bind(let key, let owner):
                snapshot["bindings"] = [["key": key, "configured": true, "owner": ["source": owner.source, "session": owner.session, "pane_id": owner.pane_id],
                                         "availability": "live", "scope_title": "Scope", "title": "Task", "labels_stale": false, "age_ms": 0, "labels_age_ms": 0]]
                return ok("{\"key\":\"\(key)\",\"session\":\"\(owner.session)\",\"pane_id\":\"\(owner.pane_id)\",\"changed\":true}")
            case .releaseBinding(let key, _):
                snapshot["bindings"] = [["key": key, "configured": true, "owner": NSNull(), "availability": "free",
                                         "scope_title": NSNull(), "title": NSNull(), "labels_stale": false, "age_ms": NSNull(), "labels_age_ms": NSNull()]]
                return ok("{\"key\":\"\(key)\",\"changed\":true}")
            default: throw ClientError.invalid("Unexpected synthetic intent request")
            }
        }
    }
}

final class LegendSourceReviewTests: XCTestCase {
    func testMissingSourceRetainsOccupiedOwnerButIsUnknownWithStaleLabels() throws {
        let row = try legendRow(source: nil)
        XCTAssertEqual(row.availability, .unknown)
        XCTAssertTrue(row.labelsStale)
        XCTAssertEqual(row.owner, identity)
        XCTAssertFalse(row.summary.contains("Free"))
    }

    func testUnknownUnavailableFreshAndStaleSourceControlsPreserveOwner() throws {
        for (availability, age, expected) in [("unknown", 0, OwnerAvailability.unknown), ("unavailable", 0, .dormant),
                                              ("available", 0, .live), ("available", 16_000, .unknown)] {
            let row = try legendRow(source: SourceHealth(id: "rex", availability: SourceAvailability(rawValue: availability)!,
                                                        observed_age_ms: UInt64(age), last_success_age_ms: UInt64(age), last_error: nil))
            XCTAssertEqual(row.availability, expected)
            XCTAssertEqual(row.labelsStale, expected != .live)
            XCTAssertEqual(row.owner, identity)
            XCTAssertNotEqual(row.availability, .free)
        }
    }

    private func legendRow(source: SourceHealth?) throws -> BindingLegendRow {
        var sample = try decodeSyntheticSnapshot()
        sample.bindings = [BindingLegendEntry(key: "key1", configured: true, owner: identity, availability: .live,
                            scope_title: "Workspace", title: "Task", labels_stale: false, age_ms: 0, labels_age_ms: 0)]
        sample.sources = source.map { [$0] } ?? []
        let now = Date(timeIntervalSince1970: 100)
        return BindingLegendRow.rows(snapshot: sample, receivedAt: now, now: now, retained: false)[0]
    }
}

final class MutationPostconditionReviewTests: XCTestCase {
    @MainActor func testUntypedDaemonErrorCannotDiscardExpectedMutationBeforeReadback() async throws {
        let fixture = try PostconditionFixture(mode: .themeDaemonError)
        let model = ConfigurationModel(endpoint: "/synthetic-postcondition.sock", transport: fixture,
                                       clock: { Date(timeIntervalSince1970: 100) })
        await model.connect(); await fixture.invoke(on: model)
        XCTAssertNil(model.notice); XCTAssertNotNil(model.error)
        await model.refresh()
        XCTAssertFalse(model.canMutate, "An untyped error is not proof the write had no effect; retain its expected postcondition")
        XCTAssertNotNil(model.error)
        XCTAssertEqual(fixture.writes, 1)
        fixture.publishExpectedOutcome(); await model.refresh()
        XCTAssertTrue(model.canMutate)
        XCTAssertEqual(fixture.writes, 1)
        model.disconnect()
    }

    @MainActor func testNewerPreWriteThemeSampleDoesNotVerifyRequestedTheme() async throws { try await check(.theme) }
    @MainActor func testNewThemeConfigModelCannotVerifyContraryAtomicRuntime() async throws { try await check(.themeWithNewConfigModel) }
    @MainActor func testLossOfCoherentRuntimeReadbackCannotAnnounceVerifiedSuccess() async throws { try await check(.themeWithoutRuntimeReadback) }
    @MainActor func testBindSamePaneAndScopeWithWrongSourceDoesNotVerifyExactOwner() async throws { try await check(.bindWrongSource) }
    @MainActor func testReleaseStillPresentExpectedOwnerIsUnverified() async throws { try await check(.releaseRetained) }
    @MainActor func testReleaseReplacementOwnerIsExplicitConflictNotSuccess() async throws { try await check(.releaseReplaced) }
    @MainActor func testReleaseUnknownLedgerCannotProveOwnerRemoval() async throws { try await check(.releaseUnknownLedger) }

    @MainActor private func check(_ mode: PostconditionFixture.Mode) async throws {
        let fixture = try PostconditionFixture(mode: mode)
        let model = ConfigurationModel(endpoint: "/synthetic-postcondition.sock", transport: fixture,
                                       clock: { Date(timeIntervalSince1970: 100) })
        await model.connect(); await fixture.invoke(on: model)
        XCTAssertNil(model.notice, "A newer sequence with the wrong postcondition is not verified success")
        XCTAssertTrue(model.error?.contains("verification failed") == true)
        XCTAssertFalse(model.canMutate)
        if mode == .releaseReplaced {
            XCTAssertTrue(model.error?.contains("owner changed") == true)
            XCTAssertTrue(model.error?.contains("herdr") == true)
        }
        await model.refresh()
        XCTAssertFalse(model.canMutate, "Explicit Refresh must retain the fence while the requested outcome is still unverified")
        XCTAssertEqual(fixture.writes, 1, "Readback/refresh must never replay the action")
        fixture.publishExpectedOutcome()
        await model.refresh()
        XCTAssertTrue(model.canMutate, "A coherent matching state may resolve the explicit verification fence")
        XCTAssertNil(model.error)
        XCTAssertEqual(fixture.writes, 1)
        model.disconnect()
    }

    @MainActor func testMatchingThemeBindAndReleaseStatesStillVerifyWithoutReplay() async throws {
        for mode in [PostconditionFixture.Mode.theme, .bindWrongSource, .releaseRetained] {
            let fixture = try PostconditionFixture(mode: mode, matchImmediately: true)
            let model = ConfigurationModel(endpoint: "/synthetic-postcondition.sock", transport: fixture,
                                           clock: { Date(timeIntervalSince1970: 100) })
            await model.connect(); await fixture.invoke(on: model)
            XCTAssertNotNil(model.notice)
            XCTAssertNil(model.error)
            XCTAssertTrue(model.canMutate)
            XCTAssertEqual(fixture.writes, 1)
            model.disconnect()
        }
    }
}

@MainActor private final class PostconditionFixture: Transport {
    enum Mode { case theme, themeWithNewConfigModel, themeWithoutRuntimeReadback, themeDaemonError, bindWrongSource, releaseRetained, releaseReplaced, releaseUnknownLedger }
    private let mode: Mode
    private let matchImmediately: Bool
    private var snapshot: [String: Any]
    private var expectedPublished = false
    private(set) var writes = 0
    init(mode: Mode, matchImmediately: Bool = false) throws {
        self.mode = mode; self.matchImmediately = matchImmediately; snapshot = try syntheticSnapshot()
        setOwner([.releaseRetained, .releaseReplaced, .releaseUnknownLedger].contains(mode) ? identity : nil)
        snapshot["panes"] = [["reference": ["source": "rex", "session": "work", "pane_id": "same"],
                              "scope_title": "Same scope", "title": "Same task", "agent_status": "idle", "focused": false,
                              "availability": "live", "labels_stale": false, "age_ms": 0, "labels_age_ms": 0]]
        snapshot["sources"] = ["rex", "herdr"].map { source in
            ["id": source, "availability": "available", "observed_age_ms": 0, "last_success_age_ms": 0, "last_error": NSNull()] as [String: Any]
        }
    }
    private func setOwner(_ owner: AgentIdentity?) {
        snapshot["bindings"] = [["key": "key1", "configured": true,
                                 "owner": owner.map { ["source": $0.source, "session": $0.session, "pane_id": $0.pane_id] as Any } ?? NSNull(),
                                 "availability": owner == nil ? "free" : "live", "scope_title": "Same scope", "title": "Same task",
                                 "labels_stale": false, "age_ms": 0, "labels_age_ms": 0]]
    }
    func invoke(on model: ConfigurationModel) async {
        switch mode {
        case .theme, .themeWithNewConfigModel, .themeWithoutRuntimeReadback, .themeDaemonError: await model.setTheme("day")
        case .bindWrongSource: await model.bind(key: "key1", agent: identity)
        default: await model.release(key: "key1", reference: identity, endpoint: model.endpoint)
        }
    }
    func publishExpectedOutcome() {
        expectedPublished = true
        snapshot["sequence"] = 9; snapshot["ledger_available"] = true
        switch mode {
        case .theme, .themeWithNewConfigModel, .themeWithoutRuntimeReadback, .themeDaemonError:
            var config = snapshot["configuration"] as! [String: Any], display = (snapshot["configuration"] as! [String: Any])["display"] as! [String: Any]
            display["theme"] = "day"; config["display"] = display; snapshot["configuration"] = config
        case .bindWrongSource: setOwner(identity)
        default: setOwner(nil)
        }
    }
    func send(_ request: Request, to endpoint: String) async throws -> Data {
        switch request {
        case .configModel:
            var config = snapshot["configuration"] as! [String: Any]
            if writes > 0, mode == .themeWithNewConfigModel {
                var display = config["display"] as! [String: Any]; display["theme"] = "day"
                config["display"] = display; config["revision"] = "new-config-model-revision"
            }
            if writes > 0, mode == .themeWithoutRuntimeReadback, !expectedPublished {
                var caps = config["capabilities"] as! [String: Any]
                caps["runtime_snapshot_versions"] = []; caps["status_read_only"] = false; config["capabilities"] = caps
            }
            return try syntheticReply(config)
        case .runtimeSnapshot: return try syntheticReply(snapshot)
        case .setTheme(let name):
            prepareContraryReadback()
            if mode == .themeDaemonError { return Data(#"{"type":"error","message":"Synthetic partial failure; outcome is not typed"}"#.utf8) }
            return ok("{\"theme\":\"\(name)\"}")
        case .bind(let key, let owner):
            prepareContraryReadback()
            return ok("{\"key\":\"\(key)\",\"session\":\"\(owner.session)\",\"pane_id\":\"\(owner.pane_id)\",\"changed\":true}")
        case .releaseBinding(let key, _):
            prepareContraryReadback()
            return ok("{\"key\":\"\(key)\",\"changed\":true}")
        default: throw ClientError.invalid("Unexpected synthetic postcondition request")
        }
    }
    private func prepareContraryReadback() {
        writes += 1; snapshot["sequence"] = 8
        switch mode {
        case .bindWrongSource, .releaseReplaced:
            setOwner(AgentIdentity(source: "herdr", session: "work", pane_id: "same"))
            var panes = snapshot["panes"] as! [[String: Any]]
            var other = panes[0]; other["reference"] = ["source": "herdr", "session": "work", "pane_id": "same"]
            panes.append(other); snapshot["panes"] = panes
        case .releaseUnknownLedger:
            snapshot["ledger_available"] = false
            snapshot["bindings"] = [["key": "key1", "configured": true, "owner": NSNull(), "availability": "unknown",
                                     "scope_title": NSNull(), "title": NSNull(), "labels_stale": true, "age_ms": NSNull(), "labels_age_ms": NSNull()]]
        default: break
        }
        if matchImmediately { publishExpectedOutcome() }
    }
}
