import XCTest
import BeckonPanelModel
import WLKit
@testable import BeckonCreatorMicro

final class StatusAdapterTests: XCTestCase {
    let source = SourceKey(socket: "/unused/herdr.sock")
    func configuration(_ ids: [String] = ["a"]) -> PanelConfiguration {
        var config = PanelConfiguration(source: source)
        for (slot, id) in ids.enumerated() { config.slots[slot] = NativeID(harness: "codex", sessionID: id) }
        return config
    }
    func snapshot(_ agents: [(String, String)], observedAt: Double = 100) throws -> AgentSnapshot {
        let entries: [[String: Any]] = agents.map { id, status in
            ["agent_session": ["agent": "codex", "kind": "id", "source": "herdr:codex", "value": id],
             "terminal_id": "t-\(id)", "pane_id": "p-\(id)", "agent_status": status]
        }
        let data = try JSONSerialization.data(withJSONObject: ["result": ["agents": entries]])
        return try AgentSnapshot.decode(data, source: source, observedAt: observedAt)
    }
    func off(_ slot: SlotLED, file: StaticString = #filePath, line: UInt = #line) {
        XCTAssertEqual(slot.thread.effect, .off, file: file, line: line)
        XCTAssertEqual(slot.thread.brightness, 0, file: file, line: line)
        XCTAssertNil(slot.presentation, file: file, line: line)
        XCTAssertNotNil(slot.unavailableReason, file: file, line: line)
    }
    func testWorkingLightsTopLeftAndUnassignedSlotsAreExplicitlyOff() throws {
        let frame = StatusAdapter.prepare(configuration: configuration(), snapshot: try snapshot([("a", "working")]), now: 101)
        XCTAssertEqual(frame.threads.map(\.id), [1, 0, 2, 3, 4, 5])
        XCTAssertEqual(frame.threads[0].color, 0x3BA0FF)
        XCTAssertEqual(frame.threads[0].effect, .breath)
        XCTAssertEqual(frame.slots[0].presentation?.label, "Working")
        XCTAssertNil(frame.slots[0].unavailableReason)
        frame.slots.dropFirst().forEach { off($0) }
    }
    func testObservationOrderNeverReassignsTheSixKeys() throws {
        let config = configuration(["a", "b", "c", "d", "e", "f"])
        let observations = [("a", "working"), ("b", "idle"), ("c", "done"), ("d", "blocked"), ("e", "unknown"), ("f", "new-status")]
        let first = StatusAdapter.prepare(configuration: config, snapshot: try snapshot(observations), now: 101)
        let reordered = StatusAdapter.prepare(configuration: config, snapshot: try snapshot(observations.reversed()), now: 101)
        XCTAssertEqual(first.threads, reordered.threads)
        XCTAssertEqual(first.threads.map(\.color), [0x3BA0FF, 0xFFFFFF, 0x22C55E, 0xFFB020, 0x808080, 0x808080])
        XCTAssertEqual(first.slots[2].presentation?.label, "Herdr-reported done/unread")
        XCTAssertEqual(first.slots[3].presentation?.label, "Blocked / attention")
        XCTAssertEqual(first.slots[5].presentation?.label, "Unknown")
    }
    func testStatusTransitionChangesOnlyTheAssignedKey() throws {
        let config = configuration(["a", "b"])
        let first = StatusAdapter.prepare(configuration: config, snapshot: try snapshot([("a", "working"), ("b", "idle")]), now: 101)
        let next = StatusAdapter.prepare(configuration: config, snapshot: try snapshot([("b", "idle"), ("a", "done")], observedAt: 102), now: 103)
        XCTAssertNotEqual(first.threads[0], next.threads[0])
        XCTAssertEqual(Array(first.threads.dropFirst()), Array(next.threads.dropFirst()))
        XCTAssertEqual(next.threads[0].color, 0x22C55E)
    }
    func testDisconnectedStaleFutureAndNonfiniteFramesAreOff() throws {
        let fresh = try snapshot([("a", "idle")])
        for now in [111, 99, Double.nan, Double.infinity] {
            StatusAdapter.prepare(configuration: configuration(), snapshot: fresh, now: now).slots.forEach { off($0) }
        }
        StatusAdapter.prepare(configuration: configuration(), snapshot: nil, now: 101).slots.forEach { off($0) }
        var nonfinite = fresh; nonfinite.observedAt = .infinity
        StatusAdapter.prepare(configuration: configuration(), snapshot: nonfinite, now: 101).slots.forEach { off($0) }
    }
    func testWrongSourceAndInvalidConfigurationFailClosed() throws {
        var wrong = try snapshot([("a", "idle")]); wrong.source.socket = "/different/herdr.sock"
        StatusAdapter.prepare(configuration: configuration(), snapshot: wrong, now: 101).slots.forEach { off($0) }
        var config = configuration(); config.slots.append(nil)
        StatusAdapter.prepare(configuration: config, snapshot: try snapshot([("a", "idle")]), now: 101).slots.forEach { off($0) }
        let duplicate = configuration(["a", "a"])
        StatusAdapter.prepare(configuration: duplicate, snapshot: try snapshot([("a", "idle")]), now: 101).slots.forEach { off($0) }
    }
    func testMissingReplacementDuplicateAndIncompleteIdentityDoNotBorrowAnotherAgent() throws {
        let config = configuration(["a", "b"])
        for observations in [[("replacement", "idle"), ("b", "working")], [("a", "idle"), ("a", "working"), ("b", "working")]] {
            let frame = StatusAdapter.prepare(configuration: config, snapshot: try snapshot(observations), now: 101)
            off(frame.slots[0])
            XCTAssertEqual(frame.threads[1].color, 0x3BA0FF)
        }
        var incomplete = try snapshot([("a", "idle"), ("b", "working")]); incomplete.agents[0].terminalID = nil
        let frame = StatusAdapter.prepare(configuration: config, snapshot: incomplete, now: 101)
        off(frame.slots[0]); XCTAssertEqual(frame.threads[1].effect, .breath)
    }
    func testCoordinatorRequiresUnchangedRegisteredBinding() throws {
        let board = BoardSelection(executable: "/bin/trellis", config: "/pin/config", registry: "/pin/boards", boardConfig: "/pin/board", alias: "development", uid: "board-id", ledger: "/pin/ledger")
        let paths = Data(#"{"ok":true,"data":{"config":"/pin/config","ledger":"/pin/ledger"}}"#.utf8)
        let context = Data(#"{"ok":true,"data":{"board":{"id":"board-id"},"selection":{"name":"development","registry":"/pin/boards","board_config":"/pin/board","db":"/pin/ledger"}}}"#.utf8)
        let agents = Data(#"{"ok":true,"data":{"registered_coordinators":[{"id":"chief","role":"Chief","link":{"provider":"herdr","stable_id":"a","target":{"session":"default","agent":"a"},"identity":{"agent_session":{"agent":"codex","kind":"id","source":"herdr:codex","value":"a"},"terminal_id":"t-a","pane_id":"p-a"}}}]}}"#.utf8)
        var directory = try CoordinatorSnapshot.decode(paths: paths, context: context, agents: agents, board: board)
        var config = configuration([]); config.coordinators[0] = try directory.assignment(id: "chief")
        let observed = try snapshot([("a", "idle")])
        XCTAssertEqual(StatusAdapter.prepare(configuration: config, snapshot: observed, directory: directory, now: 101).threads[0].color, 0xFFFFFF)
        off(StatusAdapter.prepare(configuration: config, snapshot: observed, now: 101).slots[0])
        directory.coordinators[0].link?.identity.paneID = "replacement"
        off(StatusAdapter.prepare(configuration: config, snapshot: observed, directory: directory, now: 101).slots[0])
    }
    func testWireIsSixBarePerKeyRecordsWithoutZoneOrSyncEffects() throws {
        let frame = StatusAdapter.prepare(configuration: configuration(), snapshot: try snapshot([("a", "working")]), now: 101)
        let array = try XCTUnwrap(JSONSerialization.jsonObject(with: frame.encodedParams()) as? [[String: Any]])
        XCTAssertEqual(array.count, 6)
        XCTAssertEqual(array.compactMap { $0["id"] as? Int }, [1, 0, 2, 3, 4, 5])
        XCTAssertEqual(array[0]["e"] as? Int, 4)
        for entry in array { XCTAssertTrue(Set(entry.keys).isSubset(of: ["id", "c", "b", "e", "s"])) }
        XCTAssertFalse(frame.focusAvailable)
        XCTAssertFalse(frame.liveWriterAvailable)
    }
}
