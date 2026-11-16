import XCTest
@testable import BeckonConfiguration

final class DevicePresentationTests: XCTestCase {
    @MainActor func testMissingSourceObservationCannotTurnCachedPaneIntoBindCandidate() async throws {
        var snapshot = try syntheticSnapshot()
        snapshot["panes"] = [["reference": ["source": "missing-source", "session": "scope", "pane_id": "pane"],
                              "scope_title": NSNull(), "title": "Cached task", "agent_status": "idle", "focused": false,
                              "availability": "live", "labels_stale": false, "age_ms": 0, "labels_age_ms": 0]]
        let model = ConfigurationModel(endpoint: "/synthetic.sock", transport: SyntheticRuntimeTransport(snapshot: snapshot))
        await model.connect()
        XCTAssertTrue(model.bindCandidates(at: Date()).isEmpty)
        model.disconnect()
    }

    func testDeviceSourceLayerAndLabelAgesRemainIndependent() throws {
        let now = Date(timeIntervalSince1970: 100)
        var json = try syntheticSnapshot(); json["age_ms"] = 1_000
        json["bindings"] = [["key": "key1", "configured": true,
                             "owner": ["source": "custom", "session": "scope", "pane_id": "pane"],
                             "availability": "live", "scope_title": "Actual workspace", "title": "Task",
                             "labels_stale": false, "age_ms": 0, "labels_age_ms": 14_000]]
        json["sources"] = [["id": "custom", "availability": "available", "observed_age_ms": 14_000,
                            "last_success_age_ms": 14_000, "last_error": NSNull()]]
        let snapshot = try decodeSyntheticSnapshot(json)
        let evidence = RuntimeEvidence(snapshot: snapshot, receivedAt: now, now: now.addingTimeInterval(0.5))
        XCTAssertEqual(evidence.age(13_000), 14_500)
        XCTAssertTrue(evidence.fresh(13_000)); XCTAssertFalse(evidence.fresh(14_000)); XCTAssertFalse(evidence.fresh(nil))
        let row = BindingLegendRow.rows(snapshot: snapshot, receivedAt: now, now: now.addingTimeInterval(0.5), retained: false)[0]
        XCTAssertEqual(row.availability, .unknown)
        XCTAssertTrue(row.labelsStale)
        XCTAssertTrue(row.summary.contains("Actual workspace"))
        XCTAssertTrue(row.summary.contains("labels stale"))
    }

    func testScopedActualErrorAndResolvedHistoryAreDifferentFromDeviceAbsence() throws {
        let now = Date(timeIntervalSince1970: 100)
        var json = try syntheticSnapshot()
        json["devices"] = [syntheticDevice(id: "device-A", connectivity: "connected", age: 0),
                           syntheticDevice(id: "device-B", connectivity: "disconnected", age: 0)]
        json["events"] = [syntheticDiagnostic()]
        var sample = try decodeSyntheticSnapshot(json)
        var health = HealthSummary.make(connected: true, stale: false, snapshot: sample, receivedAt: now, now: now)
        XCTAssertEqual(health.level, .fault)
        XCTAssertTrue(health.text.contains("device-B")); XCTAssertFalse(health.text.contains("device-A failed"))
        sample.events[0].active = false
        health = HealthSummary.make(connected: true, stale: false, snapshot: sample, receivedAt: now, now: now)
        XCTAssertEqual(health.level, .degraded)
        sample.events[0].active = true; sample.events[0].code = .device_unavailable
        XCTAssertNotEqual(HealthSummary.make(connected: true, stale: false, snapshot: sample, receivedAt: now, now: now).level, .fault)
    }

    func testUnknownDiagnosticAndMissingObservationNeverAppearHealthy() throws {
        let now = Date(timeIntervalSince1970: 100)
        var json = try syntheticSnapshot(); json["events"] = [syntheticDiagnostic(code: "future-error")]
        let sample = try decodeSyntheticSnapshot(json)
        XCTAssertEqual(sample.events[0].code, .unknown)
        XCTAssertEqual(sample.events[0].code.message, "Unsupported diagnostic")
        let summary = HealthSummary.make(connected: true, stale: false, snapshot: sample, receivedAt: now, now: now)
        XCTAssertEqual(summary.level, .degraded)
        XCTAssertTrue(summary.text.contains("unknown"))
        XCTAssertEqual(HealthSummary.make(connected: false, stale: true, snapshot: sample, receivedAt: now, now: now).level, .disconnected)
        XCTAssertEqual(HealthSummary.make(connected: true, stale: false, snapshot: nil, receivedAt: nil, now: now).level, .unavailable)
    }

    func testZeroAndLargeArbitraryLegendsAndOrphansUseConfiguredOrder() throws {
        let now = Date(timeIntervalSince1970: 100)
        var sample = try decodeSyntheticSnapshot()
        sample.configuration.bindings = []; sample.configuration.devices = []; sample.devices = []
        XCTAssertNoThrow(try sample.validate())
        XCTAssertEqual(BindingLegendRow.rows(snapshot: sample, receivedAt: now, now: now, retained: false), [])
        sample.configuration.bindings = (0..<47).map { "unusual-key-\($0)" }
        sample.bindings = [BindingLegendEntry(key: "orphan-key", configured: false,
                            owner: AgentIdentity(source: "custom-source", session: "opaque-scope", pane_id: "opaque-pane"),
                            availability: .dormant, scope_title: nil, title: "Cached title", labels_stale: true, age_ms: 25_000, labels_age_ms: 30_000)]
        let rows = BindingLegendRow.rows(snapshot: sample, receivedAt: now, now: now, retained: false)
        XCTAssertEqual(rows.count, 48); XCTAssertEqual(rows[0].key, "unusual-key-0"); XCTAssertFalse(rows[47].configured)
        XCTAssertTrue(rows[47].summary.contains("Workspace/session title unavailable [opaque-scope]"))
        XCTAssertTrue(rows[47].summary.contains("custom-source")); XCTAssertTrue(rows[47].summary.contains("Cached title"))
        sample.ledger_available = false
        XCTAssertTrue(BindingLegendRow.rows(snapshot: sample, receivedAt: now, now: now, retained: false).allSatisfy { $0.availability == .unknown })
    }

    func testLayerAndPhysicalNamesComeFromDescriptorsNotAssumedKeyPositions() throws {
        let descriptor = MemberDescriptor(address: "odd:custom/900", name: nil,
                                          layer: LayerDescriptor(name: "Custom layer", profile: 7, position: 42))
        XCTAssertEqual(descriptor.label, "odd:custom/900 · Custom layer")
        XCTAssertTrue(LayerDescriptor(name: nil, profile: 7, position: 42).label.contains("name unavailable"))
        let action = try JSONDecoder().decode(ActionDefinition.self, from: Data(#"{"kind":"tap-hold-chords","tap_keys":["enter"],"hold_keys":["shift","enter"],"hold_ms":417}"#.utf8))
        XCTAssertTrue(action.legend.contains("417 ms")); XCTAssertTrue(action.legend.contains("No tap on held release"))
    }

    @MainActor func testFreshOwnerWithOldWorkspaceLabelIsExplicitlyStaleInCandidatePicker() async throws {
        var snapshot = try syntheticSnapshot()
        snapshot["panes"] = [["reference": ["source": "arbitrary-source", "session": "scope-id", "pane_id": "pane-id"],
                              "scope_title": "Old workspace", "title": "Current task", "agent_status": "idle", "focused": false,
                              "availability": "live", "labels_stale": false, "age_ms": 0, "labels_age_ms": 20_000]]
        snapshot["sources"] = [["id": "arbitrary-source", "availability": "available", "observed_age_ms": 0, "last_success_age_ms": 0, "last_error": NSNull()]]
        let fixture = SyntheticRuntimeTransport(snapshot: snapshot)
        let model = ConfigurationModel(endpoint: "/synthetic.sock", transport: fixture)
        await model.connect()
        let candidate = try XCTUnwrap(model.bindCandidates(at: Date()).first)
        XCTAssertTrue(candidate.title.contains("labels stale"))
        XCTAssertEqual(candidate.id, AgentIdentity(source: "arbitrary-source", session: "scope-id", pane_id: "pane-id"))
        model.disconnect()
    }

    @MainActor func testDisconnectedCachedFreeLegendIsUnknownNotAvailableToBind() async throws {
        var snapshot = try syntheticSnapshot()
        snapshot["bindings"] = [["key": "key1", "configured": true, "owner": NSNull(), "availability": "free",
                                 "scope_title": NSNull(), "title": NSNull(), "labels_stale": false,
                                 "age_ms": NSNull(), "labels_age_ms": NSNull()]]
        let fixture = SyntheticRuntimeTransport(snapshot: snapshot)
        let model = ConfigurationModel(endpoint: "/synthetic.sock", transport: fixture)
        await model.connect(); model.disconnect()
        XCTAssertEqual(model.bindingLegend(at: Date()).first?.availability, .unknown)
        XCTAssertTrue(model.bindingLegend(at: Date()).first?.summary.contains("unknown") == true)
    }

    @MainActor func testDormantLedgerOwnerBlocksBindingInsteadOfAppearingFree() async throws {
        var snapshot = try syntheticSnapshot()
        snapshot["bindings"] = [["key": "key1", "configured": true,
                                 "owner": ["source": "rex", "session": "opaque-scope", "pane_id": "same"],
                                 "availability": "dormant", "scope_title": NSNull(), "title": "Cached task",
                                 "labels_stale": true, "age_ms": 20_000, "labels_age_ms": 20_000]]
        snapshot["panes"] = [["reference": ["source": "herdr", "session": "work", "pane_id": "same"],
                              "scope_title": "Real workspace", "title": "Live candidate", "agent_status": "idle", "focused": false,
                              "availability": "live", "labels_stale": false, "age_ms": 0, "labels_age_ms": 0]]
        snapshot["sources"] = [["id": "herdr", "availability": "available", "observed_age_ms": 0, "last_success_age_ms": 0, "last_error": NSNull()]]
        let fixture = SyntheticRuntimeTransport(snapshot: snapshot)
        let model = ConfigurationModel(endpoint: "/synthetic.sock", transport: fixture)
        await model.connect()
        await model.bind(key: "key1", agent: AgentIdentity(source: "herdr", session: "work", pane_id: "same"))
        XCTAssertTrue(model.error?.contains("occupied") == true)
        let requests = await fixture.requests
        XCTAssertEqual(requests.count, 2)
        model.disconnect()
    }

    @MainActor func testMixedDeviceAvailabilityIsNeutralNotAnotherDevicesFault() async throws {
        let now = Date(timeIntervalSince1970: 100)
        var snapshot = try syntheticSnapshot()
        snapshot["devices"] = [syntheticDevice(connectivity: "connected", age: 0),
                               syntheticDevice(id: "arbitrary-optional", connectivity: "disconnected", age: 0)]
        var config = snapshot["configuration"] as! [String: Any]
        var definitions = config["devices"] as! [[String: Any]]
        var second = definitions[0]; second["id"] = "arbitrary-optional"; definitions.append(second)
        config["devices"] = definitions; snapshot["configuration"] = config
        let fixture = SyntheticRuntimeTransport(snapshot: snapshot)
        let model = ConfigurationModel(endpoint: "/synthetic.sock", transport: fixture, clock: { now })
        await model.connect()
        XCTAssertTrue(model.health(at: now).contains("1 connected · 1 disconnected · 0 unknown"))
        XCTAssertFalse(model.health(at: now).contains("Creator"))
        model.disconnect()
    }
}
