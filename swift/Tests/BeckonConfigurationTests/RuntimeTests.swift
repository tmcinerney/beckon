import XCTest
@testable import BeckonConfiguration

// SYNTHETIC consumer-development inputs from contracts.md v1, NOT Rust serializer fixtures.
final class RuntimeTests: XCTestCase {
    @MainActor func testMissingConfiguredDeviceCannotPublishAHealthyRuntimeSample() async throws {
        var snapshot = try syntheticSnapshot(); snapshot["devices"] = []
        let fixture = SyntheticRuntimeTransport(snapshot: snapshot)
        let model = ConfigurationModel(endpoint: "/synthetic.sock", transport: fixture)
        await model.connect()
        XCTAssertNil(model.runtime)
        XCTAssertFalse(model.canMutate)
        XCTAssertNotNil(model.runtimeError)
        model.disconnect()
    }

    @MainActor func testPreWriteCachedSampleCannotVerifyAnAcknowledgedChange() async throws {
        let fixture = try CachedReadbackTransport()
        let model = ConfigurationModel(endpoint: "/synthetic.sock", transport: fixture)
        await model.connect(); await model.setTheme("day")
        XCTAssertNil(model.notice)
        XCTAssertTrue(model.error?.contains("verification failed") == true)
        XCTAssertFalse(model.canMutate)
        model.disconnect()
    }

    @MainActor func testFreshAuthoritativeFreeKeyCanBindExactRuntimeCandidate() async throws {
        let fixture = try SyntheticBindingTransport()
        let model = ConfigurationModel(endpoint: "/synthetic.sock", transport: fixture)
        await model.connect(); await model.bind(key: "key1", agent: identity)
        let writes = await fixture.writes
        XCTAssertEqual(writes, [.bind(key: "key1", agent: identity)])
        XCTAssertEqual(model.bindingLegend(at: Date()).first?.owner, identity)
        XCTAssertNotNil(model.notice)
        model.disconnect()
    }

    @MainActor func testRuntimeReleaseRemainsOwnerAndEndpointGuarded() async throws {
        let fixture = try SyntheticBindingTransport()
        let model = ConfigurationModel(endpoint: "/synthetic.sock", transport: fixture)
        await model.connect(); await model.bind(key: "key1", agent: identity)
        await model.release(key: "key1", reference: identity, endpoint: "/different.sock")
        await model.release(key: "key1", reference: AgentIdentity(source: "other", session: "work", pane_id: "same"), endpoint: "/synthetic.sock")
        let before = await fixture.writes
        XCTAssertEqual(before.count, 1)
        await model.release(key: "key1", reference: identity, endpoint: "/synthetic.sock")
        let after = await fixture.writes
        XCTAssertEqual(after.last, .releaseBinding(key: "key1", reference: identity))
        XCTAssertEqual(model.bindingLegend(at: Date()).first?.availability, .free)
        XCTAssertNotNil(model.notice)
        model.disconnect()
    }

    func testLargeDeviceCountsAndDiagnosticCapacityAreBoundedData() throws {
        var sample = try decodeSyntheticSnapshot()
        let original = sample.configuration.devices[0], health = sample.devices[0]
        sample.configuration.devices = (0..<19).map { index in var copy = original; copy.id = "custom-pad-\(index)"; return copy }
        sample.devices = (0..<19).map { index in var copy = health; copy.id = "custom-pad-\(index)"; return copy }
        let event = try JSONDecoder().decode(DiagnosticEvent.self, from: JSONSerialization.data(withJSONObject: syntheticDiagnostic()))
        sample.events = (0..<64).map { index in var copy = event; copy.id = UInt64(index); return copy }
        XCTAssertNoThrow(try sample.validate())
        sample.events.append(event)
        XCTAssertThrowsError(try sample.validate())
    }

    func testActionUnknownKindIsInspectOnlyAndKnownHoldRequiresExplicitPositiveTiming() throws {
        let future = Data(#"{"kind":"future-action","payload":{"opaque":true}}"#.utf8)
        let action = try JSONDecoder().decode(ActionDefinition.self, from: future)
        XCTAssertTrue(action.legend.contains("inspect-only"))
        XCTAssertEqual(try JSONDecoder().decode(JSONValue.self, from: JSONEncoder().encode(action)),
                       try JSONDecoder().decode(JSONValue.self, from: future))
        for json in [#"{"kind":"tap-hold-chords","tap_keys":["enter"],"hold_keys":["shift","enter"]}"#,
                     #"{"kind":"tap-hold-chords","tap_keys":["enter"],"hold_keys":["shift","enter"],"hold_ms":0}"#,
                     #"{"kind":"tap-hold-chords","tap_keys":[],"hold_keys":["enter"],"hold_ms":300}"#,
                     #"{"kind":"tap-hold-chords","tap_keys":["enter"],"hold_keys":["enter"],"hold_ms":18446744073709551616}"#] {
            XCTAssertThrowsError(try JSONDecoder().decode(ActionDefinition.self, from: Data(json.utf8)))
        }
    }

    func testV2ReplacementSendsAllSixStatesAndExplicitNullClears() throws {
        let effects = Feedback(pending: Treatment(color: "#FFFFFF", brightness: 0.12, motion: .steady),
                               error: TimedFeedback(treatment: Treatment(color: "#FFB000", brightness: 0.5, motion: .steady), duration_ms: 650))
        let encoded = try JSONDecoder().decode(JSONValue.self, from: JSONEncoder().encode(Request.setPresentation(
            expectedRevision: "reviewed-r", change: .deviceEffectV2(device: "custom-device", effect: "custom-effect", effects: effects))))
        let expected = Data(##"{"op":"set_presentation","expected_revision":"reviewed-r","change":{"kind":"device_effect_v2","device":"custom-device","effect":"custom-effect","effects":{"idle":null,"held":null,"latched":null,"pending":{"color":"#FFFFFF","brightness":0.12,"motion":"steady"},"hold_fired":null,"error":{"treatment":{"color":"#FFB000","brightness":0.5,"motion":"steady"},"duration_ms":650}}}}"##.utf8)
        XCTAssertEqual(encoded, try JSONDecoder().decode(JSONValue.self, from: expected))
        XCTAssertThrowsError(try Feedback(error: TimedFeedback(treatment: effects.pending!, duration_ms: 0)).validate())
    }

    func testExactRuntimeRequestAndInvalidVersionRevisionDuplicatesAndEventBound() throws {
        XCTAssertEqual(try JSONDecoder().decode(JSONValue.self, from: JSONEncoder().encode(Request.runtimeSnapshot)),
                       .object(["op": .string("runtime_snapshot"), "version": .number(1)]))
        var sample = try decodeSyntheticSnapshot()
        sample.version = 2; XCTAssertThrowsError(try sample.validate())
        sample = try decodeSyntheticSnapshot(); sample.configuration_revision = "wrong"; XCTAssertThrowsError(try sample.validate())
        sample = try decodeSyntheticSnapshot(); sample.devices.append(sample.devices[0]); XCTAssertThrowsError(try sample.validate())
        var json = try syntheticSnapshot(); json.removeValue(forKey: "configuration")
        XCTAssertThrowsError(try JSONDecoder().decode(RuntimeSnapshot.self, from: JSONSerialization.data(withJSONObject: json)))
    }

    @MainActor func testInvalidRuntimeRetainsLastGoodSampleAndBlocksWrites() async throws {
        let fixture = SyntheticRuntimeTransport(snapshot: try syntheticSnapshot())
        let model = ConfigurationModel(endpoint: "/synthetic.sock", transport: fixture)
        await model.connect()
        var invalid = try syntheticSnapshot(sequence: 8); invalid["configuration_revision"] = "wrong"
        await fixture.replace(invalid); await model.refresh()
        XCTAssertEqual(model.runtime?.sequence, 7)
        XCTAssertEqual(model.configuration?.revision, "atomic-r2")
        XCTAssertFalse(model.canMutate)
        XCTAssertNotNil(model.runtimeError)
        model.disconnect()
    }

    func testPartialOrFutureCapabilitiesDoNotBreakReadOnlyConfigurationDisplay() throws {
        var json = try JSONSerialization.jsonObject(with: Data(configJSON.utf8)) as! [String: Any]
        json["capabilities"] = ["future_capability": true]
        let config = try JSONDecoder().decode(Configuration.self, from: JSONSerialization.data(withJSONObject: json))
        XCTAssertFalse(config.supportsPresentation("device_effect_v2"))
        XCTAssertNotEqual(config.capabilities?.status_read_only, true)
        XCTAssertNoThrow(try config.validate())
    }

    @MainActor func testRuntimeReadbackFailureCannotAnnounceVerifiedMutation() async throws {
        let fixture = try RuntimeReadbackFailureTransport()
        let model = ConfigurationModel(endpoint: "/synthetic.sock", transport: fixture)
        await model.connect(); await model.setTheme("day")
        XCTAssertNil(model.notice)
        XCTAssertTrue(model.error?.contains("verification failed") == true)
        XCTAssertEqual(model.configuration?.display.theme, "night")
        XCTAssertFalse(model.canMutate)
        model.disconnect()
    }

    @MainActor func testAlreadyStaleRuntimeCannotEnableMutationFromItsNewSocketReceipt() async throws {
        var snapshot = try syntheticSnapshot(); snapshot["age_ms"] = 16_000
        let fixture = SyntheticRuntimeTransport(snapshot: snapshot)
        let model = ConfigurationModel(endpoint: "/synthetic.sock", transport: fixture)
        await model.connect()
        XCTAssertFalse(model.canMutate)
        XCTAssertEqual(model.healthSummary(at: Date()).level, .stale)
        model.disconnect()
    }

    func testFutureFeedbackStateIsPreservedAndCannotBeWrittenAsLegacy() throws {
        let bytes = Data(##"{"idle":{"color":"#FFFFFF","brightness":0.2,"motion":"steady"},"held":null,"latched":null,"future_phase":{"vendor":true}}"##.utf8)
        let effect = try JSONDecoder().decode(Feedback.self, from: bytes)
        XCTAssertEqual(try JSONDecoder().decode(JSONValue.self, from: JSONEncoder().encode(effect)),
                       try JSONDecoder().decode(JSONValue.self, from: bytes))
        XCTAssertThrowsError(try effect.validate())
    }

    @MainActor func testLegacyEffectWriterCannotSendExtendedFeedback() async throws {
        let fixture = try PresentationFixtureForRuntime()
        let model = ConfigurationModel(endpoint: "/synthetic.sock", transport: fixture)
        await model.connect()
        let effect = Feedback(pending: Treatment(color: "#FFFFFF", brightness: 0.12, motion: .steady))
        let saved = await model.setPresentation(.deviceEffect(device: "desk", effect: "red", effects: effect), expectedRevision: "r1")
        XCTAssertFalse(saved)
        let writes = await fixture.writes
        XCTAssertEqual(writes, 0)
        model.disconnect()
    }

    @MainActor func testLowerSequenceCannotReplaceCurrentConfiguration() async throws {
        let fixture = SyntheticRuntimeTransport(snapshot: try syntheticSnapshot(sequence: 9))
        let model = ConfigurationModel(endpoint: "/synthetic-never-live.sock", transport: fixture)
        await model.connect()
        var older = try syntheticSnapshot(sequence: 8)
        older["configuration"] = try syntheticConfiguration(revision: "old-revision")
        older["configuration_revision"] = "old-revision"
        await fixture.replace(older); await model.refresh()
        XCTAssertEqual(model.runtime?.sequence, 9)
        XCTAssertEqual(model.configuration?.revision, "atomic-r2")
        XCTAssertTrue(model.stale)
        model.disconnect()
    }

    @MainActor func testRuntimePublishesConfigurationFromOneSampleNotBootstrapOrStatus() async throws {
        let fixture = SyntheticRuntimeTransport(snapshot: try syntheticSnapshot())
        let model = ConfigurationModel(endpoint: "/synthetic-never-live.sock", transport: fixture)
        await model.connect()
        XCTAssertEqual(model.configuration?.revision, "atomic-r2")
        XCTAssertNil(model.error)
        let requests = await fixture.requests
        XCTAssertFalse(requests.contains(.status))
        XCTAssertEqual(requests.count, 2)
        model.disconnect()
    }

    @MainActor func testOldDaemonConnectAndReconnectNeverCallReconcilingStatus() async {
        let fixture = MockTransport(); await fixture.setLegacy()
        let model = ConfigurationModel(endpoint: "/synthetic-never-live.sock", transport: fixture)
        await model.connect(); await model.connect(); await model.refresh()
        XCTAssertNotNil(model.configuration)
        XCTAssertNil(model.status)
        let requests = await fixture.requests
        XCTAssertEqual(requests, [.configModel, .configModel, .configModel])
    }

    func testExtendedFeedbackSurvivesRoundTripWithoutTruncation() throws {
        let bytes = Data(##"{"idle":null,"held":null,"latched":null,"pending":{"color":"#FFFFFF","brightness":0.12,"motion":"steady"},"hold_fired":{"color":"#3388FF","brightness":0.45,"motion":"steady"},"error":{"treatment":{"color":"#FFB000","brightness":0.5,"motion":"steady"},"duration_ms":650}}"##.utf8)
        let feedback = try JSONDecoder().decode(Feedback.self, from: bytes)
        XCTAssertEqual(try JSONDecoder().decode(JSONValue.self, from: JSONEncoder().encode(feedback)),
                       try JSONDecoder().decode(JSONValue.self, from: bytes))
        XCTAssertNoThrow(try feedback.validate())
    }

    func testTapHoldActionDoesNotRequireLegacyChordFields() throws {
        let action = try JSONDecoder().decode(ActionDefinition.self, from: Data(
            #"{"kind":"tap-hold-chords","tap_keys":["enter"],"hold_keys":["shift","enter"],"hold_ms":300}"#.utf8))
        XCTAssertEqual(action.kind, "tap-hold-chords")
        let encoded = try JSONDecoder().decode(JSONValue.self, from: JSONEncoder().encode(action))
        XCTAssertEqual(encoded, .object(["kind": .string("tap-hold-chords"), "tap_keys": .array([.string("enter")]),
                                        "hold_keys": .array([.string("shift"), .string("enter")]), "hold_ms": .number(300)]))
    }
}

actor CachedReadbackTransport: Transport {
    private let snapshot: [String: Any]
    init() throws { snapshot = try syntheticSnapshot() }
    func send(_ request: Request, to endpoint: String) async throws -> Data {
        switch request {
        case .configModel: return try syntheticReply(snapshot["configuration"] as! [String: Any])
        case .runtimeSnapshot: return try syntheticReply(snapshot)
        case .setTheme(let name): return ok("{\"theme\":\"\(name)\"}")
        default: throw ClientError.invalid("Unexpected synthetic request")
        }
    }
}

actor SyntheticBindingTransport: Transport {
    private var snapshot: [String: Any]
    private(set) var writes: [Request] = []
    init() throws {
        snapshot = try syntheticSnapshot()
        snapshot["bindings"] = [["key": "key1", "configured": true, "owner": NSNull(), "availability": "free",
                                 "scope_title": NSNull(), "title": NSNull(), "labels_stale": false,
                                 "age_ms": NSNull(), "labels_age_ms": NSNull()]]
        snapshot["panes"] = [["reference": ["source": "rex", "session": "work", "pane_id": "same"],
                              "scope_title": "Actual scope", "title": "Actual task", "agent_status": "working", "focused": false,
                              "availability": "live", "labels_stale": false, "age_ms": 0, "labels_age_ms": 0]]
        snapshot["sources"] = [["id": "rex", "availability": "available", "observed_age_ms": 0, "last_success_age_ms": 0, "last_error": NSNull()]]
    }
    func send(_ request: Request, to endpoint: String) async throws -> Data {
        switch request {
        case .configModel: return try syntheticReply(snapshot["configuration"] as! [String: Any])
        case .runtimeSnapshot: return try syntheticReply(snapshot)
        case .bind(let key, let owner):
            writes.append(request)
            snapshot["sequence"] = 8
            snapshot["bindings"] = [["key": key, "configured": true,
                                     "owner": ["source": owner.source, "session": owner.session, "pane_id": owner.pane_id],
                                     "availability": "live", "scope_title": "Actual scope", "title": "Actual task", "labels_stale": false,
                                     "age_ms": 0, "labels_age_ms": 0]]
            return ok("{\"key\":\"\(key)\",\"session\":\"\(owner.session)\",\"pane_id\":\"\(owner.pane_id)\",\"changed\":true}")
        case .releaseBinding(let key, _):
            writes.append(request); snapshot["sequence"] = 9
            snapshot["bindings"] = [["key": key, "configured": true, "owner": NSNull(), "availability": "free",
                                     "scope_title": NSNull(), "title": NSNull(), "labels_stale": false,
                                     "age_ms": NSNull(), "labels_age_ms": NSNull()]]
            return ok("{\"key\":\"\(key)\",\"changed\":true}")
        default: throw ClientError.invalid("Unexpected synthetic binding request")
        }
    }
}

actor RuntimeReadbackFailureTransport: Transport {
    private let snapshot: [String: Any]
    private var written = false
    init() throws { snapshot = try syntheticSnapshot() }
    func send(_ request: Request, to endpoint: String) async throws -> Data {
        switch request {
        case .configModel: return try syntheticReply(snapshot["configuration"] as! [String: Any])
        case .runtimeSnapshot:
            if written { throw ClientError.timeout }
            return try syntheticReply(snapshot)
        case .setTheme(let theme): written = true; return ok("{\"theme\":\"\(theme)\"}")
        default: throw ClientError.invalid("Unexpected synthetic request")
        }
    }
}

// SYNTHETIC configuration transport: counts explicit writes, never accesses a daemon.
actor PresentationFixtureForRuntime: Transport {
    private var config: Configuration
    private(set) var writes = 0
    init() throws {
        config = try JSONDecoder().decode(Configuration.self, from: Data(configJSON.utf8)); config.revision = "r1"
        config.capabilities = try JSONDecoder().decode(Capabilities.self, from: Data(capabilitiesJSON.utf8))
    }
    func send(_ request: Request, to endpoint: String) async throws -> Data {
        if case .setPresentation = request { writes += 1 }
        if request == .status { return ok(statusJSON) }
        return ok(String(decoding: try JSONEncoder().encode(config), as: UTF8.self))
    }
}
