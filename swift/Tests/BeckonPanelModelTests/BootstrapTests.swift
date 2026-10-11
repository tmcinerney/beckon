import XCTest
@testable import BeckonPanelModel

@MainActor final class BootstrapTests: XCTestCase {
    func temporaryDirectory() throws -> URL {
        let url = URL(fileURLWithPath: "/tmp/beckon-" + String(UUID().uuidString.prefix(8)))
        try FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
        return url
    }
    func testPreviewLoadsAssignmentsButEditsNeverPersist() throws {
        let dir = try temporaryDirectory(); defer {try? FileManager.default.removeItem(at: dir)}
        let url = dir.appendingPathComponent("panel.json")
        var config = PanelConfiguration(source: SourceKey(socket: "/unused/existing.sock"))
        config.slots[0] = NativeID(harness: "codex", sessionID: "assigned")
        config.slotLabels = ["Review", "Research", "Support", "Build", "Design", "Test"]
        try ConfigStore(url: url).save(config)
        let bytes = try Data(contentsOf: url)
        let bootstrap = try PanelBootstrap(arguments: ["--config", url.path], home: dir.path, allowPersistence: false)
        XCTAssertEqual(bootstrap.model.configuration, config)
        XCTAssertNil(bootstrap.model.store)
        var edit = bootstrap.model.configuration; edit.slots[0] = nil
        try bootstrap.model.edit(edit, expectedGeneration: bootstrap.model.generation)
        XCTAssertNil(bootstrap.model.configuration.slots[0])
        XCTAssertEqual(try Data(contentsOf: url), bytes)
    }
    func testNormalPanelRetainsPersistenceAndNoSaveOverride() throws {
        let dir = try temporaryDirectory(); defer {try? FileManager.default.removeItem(at: dir)}
        let normal = try PanelBootstrap(arguments: [], home: dir.path)
        XCTAssertNotNil(normal.model.store)
        XCTAssertFalse(FileManager.default.fileExists(atPath: dir.appendingPathComponent(".config").path))
        let noSave = try PanelBootstrap(arguments: ["--no-save"], home: dir.path)
        XCTAssertNil(noSave.model.store)
    }
    func testDemoIsNonpersistentAndUsesExistingCoordinatorBinding() async throws {
        let bootstrap = try PanelBootstrap(arguments: ["--demo"], home: "/unused", allowPersistence: false)
        XCTAssertNotNil(bootstrap.demo)
        XCTAssertNil(bootstrap.model.store)
        XCTAssertEqual((0..<6).map {bootstrap.model.configuration.slotTitle($0)}, [
            "1 · Chief of Staff", "2 · Project Coordinator", "3 · Releases",
            "4 · Selectable agent 4", "5 · Selectable agent 5", "6 · Selectable agent 6",
        ])
        XCTAssertTrue(bootstrap.model.configuration.slots.dropFirst(2).allSatisfy {$0 == nil})
        XCTAssertTrue(bootstrap.model.configuration.coordinators.dropFirst().allSatisfy {$0 == nil})
        try bootstrap.model.configuration.validate()
        await bootstrap.model.refresh()
        XCTAssertEqual(try bootstrap.model.record(slot: 0).native, NativeID(harness: "codex", sessionID: "demo-a"))
        XCTAssertEqual(try bootstrap.model.record(slot: 1).native, NativeID(harness: "claude", sessionID: "demo-b"))
        XCTAssertGreaterThan(try bootstrap.model.record(slot: 1).label.count, 60)
        try await bootstrap.model.shutdownAndWait()
    }
    func testCorruptConfigurationIsPreservedAndEditingDisabled() throws {
        let dir = try temporaryDirectory(); defer {try? FileManager.default.removeItem(at: dir)}
        let url = dir.appendingPathComponent("panel.json"), bytes = Data("corrupt".utf8)
        try bytes.write(to: url)
        let bootstrap = try PanelBootstrap(arguments: ["--config", url.path], home: dir.path, allowPersistence: false)
        XCTAssertNotNil(bootstrap.notice)
        XCTAssertFalse(bootstrap.writable)
        XCTAssertNil(bootstrap.model.store)
        XCTAssertEqual(try Data(contentsOf: url), bytes)
    }
    func testExplicitSourceAndInvalidFlagsUseTheSameOptions() throws {
        let bootstrap = try PanelBootstrap(arguments: ["--herdr-socket", "/unused/selected.sock"], home: "/unused", allowPersistence: false)
        XCTAssertEqual(bootstrap.model.configuration.source.socket, "/unused/selected.sock")
        for args in [["--focus"], ["--save"], ["--config", "relative"], ["--herdr-socket"]] {
            XCTAssertThrowsError(try PanelBootstrap(arguments: args, home: "/unused", allowPersistence: false))
        }
    }
}
