import XCTest
@testable import BeckonPanelModel

final class ModelTests: XCTestCase {
    let source = SourceKey(socket: "/unused/herdr.sock")
    func wire(_ entries: String) -> Data { Data(#"{"id":"read","result":{"agents":["#.utf8) + Data(entries.utf8) + Data("]}}".utf8) }
    let a = #"{"agent_session":{"agent":"codex","kind":"id","source":"herdr:codex","value":"session-a"},"terminal_id":"t-a","pane_id":"p-a","agent_status":"working","revision":3,"completion_seq":2}"#
    let b = #"{"agent_session":{"agent":"claude","kind":"id","source":"herdr:claude","value":"session-b"},"terminal_id":"t-b","pane_id":"p-b","agent_status":"done","revision":4}"#

    func testPersonalLabelsRoundTripWithoutAssigningRoles() throws {
        let legacy = try JSONEncoder().encode(PanelConfiguration(source: source))
        let old = try JSONDecoder().decode(PanelConfiguration.self, from: legacy)
        try old.validate()
        var object = try XCTUnwrap(JSONSerialization.jsonObject(with: legacy) as? [String: Any])
        let labels = ["Chief of Staff", "Project Coordinator", "Releases", "Selectable agent 4", "Selectable agent 5", "Selectable agent 6"]
        object["slotLabels"] = labels
        let config = try JSONDecoder().decode(PanelConfiguration.self, from: JSONSerialization.data(withJSONObject: object))
        try config.validate()
        let encoded = try XCTUnwrap(JSONSerialization.jsonObject(with: JSONEncoder().encode(config)) as? [String: Any])
        XCTAssertEqual(encoded["slotLabels"] as? [String], labels)
        XCTAssertTrue(config.slots.allSatisfy { $0 == nil })
        XCTAssertTrue(config.coordinators.allSatisfy { $0 == nil })
    }
    func testMalformedPersonalLabelsAreRejected() throws {
        var object = try XCTUnwrap(JSONSerialization.jsonObject(with: JSONEncoder().encode(PanelConfiguration(source: source))) as? [String: Any])
        for labels in [Array(repeating: "Role", count: 5), [" "] + Array(repeating: "Role", count: 5), ["Role\nOther"] + Array(repeating: "Role", count: 5), [String(repeating: "x", count: 129)] + Array(repeating: "Role", count: 5)] {
            object["slotLabels"] = labels
            let config = try JSONDecoder().decode(PanelConfiguration.self, from: JSONSerialization.data(withJSONObject: object))
            XCTAssertThrowsError(try config.validate())
        }
    }
    func testActualEnvelopeAndNativeIdentitySurviveReorder() throws {
        let first = try AgentSnapshot.decode(wire(a + "," + b), source: source, observedAt: 10)
        XCTAssertEqual(first.agents.count, 2)
        let native = NativeID(harness: "codex", sessionID: "session-a")
        XCTAssertEqual(try first.resolve(native, now: 11).agentStatus, "working")
        let reordered = try AgentSnapshot.decode(wire(b + "," + a), source: source, observedAt: 12)
        XCTAssertEqual(try reordered.resolve(native, now: 13).paneID, "p-a")
    }
    func testMalformedDuplicateAndStaleSourcesNeverBecomeIdle() throws {
        XCTAssertThrowsError(try AgentSnapshot.decode(Data("{}".utf8), source: source, observedAt: 10))
        let snapshot = try AgentSnapshot.decode(wire(a + "," + a), source: source, observedAt: 10)
        let native = NativeID(harness: "codex", sessionID: "session-a")
        XCTAssertThrowsError(try snapshot.resolve(native, now: 11))
        XCTAssertThrowsError(try snapshot.resolve(native, now: 21))
        XCTAssertThrowsError(try snapshot.resolve(native, now: 9))
    }
    func testConfigurationRoundTripAndCorruptFilePreserved() throws {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: dir) }
        let store = ConfigStore(url: dir.appendingPathComponent("panel.json"))
        var config = PanelConfiguration(source: source)
        config.slots[0] = NativeID(harness: "codex", sessionID: "session-a")
        try store.save(config)
        XCTAssertEqual(try store.load(defaultSource: source), config)
        let corrupt = Data("not json".utf8)
        try corrupt.write(to: store.url)
        XCTAssertThrowsError(try store.load(defaultSource: source))
        XCTAssertEqual(try Data(contentsOf: store.url), corrupt)
    }
    func testFailedAtomicWriteAndUnsupportedExistingFileArePreserved() throws {
        let dir=FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at:dir,withIntermediateDirectories:true)
        defer {try? FileManager.default.setAttributes([.posixPermissions:0o755],ofItemAtPath:dir.path);try? FileManager.default.removeItem(at:dir)}
        let store=ConfigStore(url:dir.appendingPathComponent("panel.json"))
        let config=PanelConfiguration(source:source)
        try store.save(config)
        let prior=try Data(contentsOf:store.url)
        try FileManager.default.setAttributes([.posixPermissions:0o555],ofItemAtPath:dir.path)
        XCTAssertThrowsError(try store.save(config))
        XCTAssertEqual(try Data(contentsOf:store.url),prior)
        try FileManager.default.setAttributes([.posixPermissions:0o755],ofItemAtPath:dir.path)
        var future=config;future.version=99
        let unsupported=try JSONEncoder().encode(future)
        try unsupported.write(to:store.url)
        XCTAssertThrowsError(try store.save(config))
        XCTAssertEqual(try Data(contentsOf:store.url),unsupported)
    }
    func testInvalidConfigDoesNotReplacePriorFile() throws {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: dir) }
        let store = ConfigStore(url: dir.appendingPathComponent("panel.json"))
        let config = PanelConfiguration(source: source)
        try store.save(config)
        let prior = try Data(contentsOf: store.url)
        var bad = config; bad.version = 2
        XCTAssertThrowsError(try store.save(bad))
        XCTAssertEqual(try Data(contentsOf: store.url), prior)
    }
}
