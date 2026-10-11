import XCTest
@testable import BeckonPanelModel
@MainActor final class InteractionTests: XCTestCase {
    func model(delay:Double=0.15) throws -> PanelModel {
        let fixtures=ModelTests()
        var config=PanelConfiguration(source:fixtures.source)
        config.slots[0]=NativeID(harness:"codex",sessionID:"session-a")
        return PanelModel(configuration:config,source:DelayedSource(data:fixtures.wire(fixtures.a),delay:delay))
    }
    func testPreviewIsSerialAndHasNoFocusCapability() async throws {
        let model=try model()
        let first=Task { try await model.preview(slot:0) }
        try await Task.sleep(nanoseconds:20_000_000)
        do {try await model.preview(slot:0); XCTFail("Concurrent preview accepted")}
        catch {XCTAssertEqual(error as? PanelError,.unavailable("Busy"))}
        try await first.value
        XCTAssertTrue(model.preview.contains("Live focus unavailable"))
        XCTAssertFalse(model.busy)
    }
    func testReassignmentAndSourceChangeFenceDelayedPublication() async throws {
        let model=try model()
        let job=Task {try await model.preview(slot:0)}
        try await Task.sleep(nanoseconds:20_000_000)
        var changed=model.configuration;changed.slots[0]=nil
        try model.edit(changed,expectedGeneration:model.generation)
        do {try await job.value;XCTFail("Superseded preview published")}catch{}
        XCTAssertEqual(model.preview,"")
        let original=model.generation
        changed.source=SourceKey(socket:"/another/socket")
        try model.edit(changed,expectedGeneration:original)
        XCTAssertNil(model.snapshot)
        XCTAssertThrowsError(try model.edit(changed,expectedGeneration:original))
    }
    func testPreviewTransportFailureImmediatelyClearsSnapshot() async throws {
        let fixtures=ModelTests(),source=ToggleSource()
        await source.set(fixtures.wire(fixtures.a))
        var config=PanelConfiguration(source:fixtures.source)
        config.slots[0]=NativeID(harness:"codex",sessionID:"session-a")
        let model=PanelModel(configuration:config,source:source)
        await model.refresh()
        await source.set(nil)
        do {try await model.preview(slot:0);XCTFail("Disconnect accepted")}catch{}
        XCTAssertNil(model.snapshot)
        XCTAssertTrue(model.health.contains("Unavailable"))
    }
    func testOlderReplyCannotReplaceNewerSnapshotInSameGeneration() async throws {
        let fixtures=ModelTests(),now=Date().timeIntervalSince1970
        let source=OrderedSource(data:fixtures.wire(fixtures.a),times:[now-1,now-2])
        let model=PanelModel(configuration:PanelConfiguration(source:fixtures.source),source:source)
        await model.refresh()
        XCTAssertNotNil(model.snapshot)
        await model.refresh()
        XCTAssertNil(model.snapshot,"An older observation replaced a newer sample")
        XCTAssertTrue(model.health.contains("Unavailable"))
    }
    func testShutdownAndWaitRefusesUnconfirmedMetadataCleanup() async throws {
        let fixtures=ModelTests()
        let model=PanelModel(configuration:PanelConfiguration(source:fixtures.source),source:ToggleSource(),
                             directory:CoordinatorDirectory(runner:UnconfirmedRunner()))
        do {try await model.shutdownAndWait();XCTFail("Quit dropped unresolved supervision")}
        catch {XCTAssertEqual(error as? PanelError,.cleanupUnconfirmed)}
    }
    func testShutdownRefusesPublicationAndConfigurationWrites() async throws {
        let model=try model()
        let job=Task {try await model.preview(slot:0)}
        try await Task.sleep(nanoseconds:20_000_000)
        model.shutdown()
        do {try await job.value;XCTFail("Shutdown preview published")}catch{}
        XCTAssertEqual(model.preview,"")
        XCTAssertThrowsError(try model.edit(model.configuration,expectedGeneration:model.generation))
    }
    func testDisconnectClearsPreviouslyHealthyAgents() async throws {
        let fixtures=ModelTests(), source=ToggleSource()
        await source.set(fixtures.wire(fixtures.a))
        let model=PanelModel(configuration:PanelConfiguration(source:fixtures.source),source:source)
        await model.refresh()
        XCTAssertNotNil(model.snapshot)
        await source.set(nil)
        await model.refresh()
        XCTAssertNil(model.snapshot)
        XCTAssertFalse(model.health.contains("Connected"))
    }
}
struct DelayedSource:AgentStateSource {
    var data:Data;var delay:Double
    func read(source:SourceKey,budget:Double) async throws -> AgentSnapshot {
        // Intentionally returns even after cancellation to test publication fencing.
        try? await Task.sleep(nanoseconds:UInt64(delay*1e9))
        return try AgentSnapshot.decode(data,source:source,observedAt:Date().timeIntervalSince1970)
    }
}
actor ToggleSource:AgentStateSource {
    var data:Data?
    func set(_ data:Data?){self.data=data}
    func read(source:SourceKey,budget:Double) async throws -> AgentSnapshot {
        guard let data else {throw PanelError.unavailable("Disconnected")}
        return try AgentSnapshot.decode(data,source:source,observedAt:Date().timeIntervalSince1970)
    }
}

struct UnconfirmedRunner:MetadataCommandRunner {
    var hasUnconfirmedCleanup:Bool {get async {true}}
    func run(executable:String,arguments:[String],budget:Double) async throws -> Data {throw PanelError.cleanupUnconfirmed}
}

actor OrderedSource:AgentStateSource {
    let data:Data;var times:[Double]
    init(data:Data,times:[Double]){self.data=data;self.times=times}
    func read(source:SourceKey,budget:Double) async throws -> AgentSnapshot {
        try AgentSnapshot.decode(data,source:source,observedAt:times.removeFirst())
    }
}
