import XCTest
@testable import BeckonPanelModel

actor ObservationProbe: AgentStateSource {
    let data: Data
    var reads = 0, completed = 0, cancelled = 0, active = 0, maximumActive = 0
    var delay: UInt64 = 10_000_000
    var fails = false
    init(data: Data) {self.data = data}
    func setDelay(_ value: UInt64) {delay = value}
    func disconnect() {fails = true}
    func read(source: SourceKey, budget: Double) async throws -> AgentSnapshot {
        reads += 1; active += 1; maximumActive = max(maximumActive, active)
        defer {active -= 1}
        let readDelay = delay
        do {try await Task.sleep(nanoseconds: readDelay)}
        catch {cancelled += 1; throw error}
        if fails {throw PanelError.unavailable("Probe disconnected")}
        completed += 1
        return try AgentSnapshot.decode(data, source: source, observedAt: Date().timeIntervalSince1970)
    }
}

@MainActor final class ObservationOwnershipTests: XCTestCase {
    func fixture() -> (PanelModel, ObservationProbe) {
        let fixtures = ModelTests()
        let probe = ObservationProbe(data: fixtures.wire(fixtures.a))
        var config = PanelConfiguration(source: fixtures.source)
        config.slots[0] = NativeID(harness: "codex", sessionID: "session-a")
        return (PanelModel(configuration: config, source: probe), probe)
    }
    func waitUntil(timeout: Double = 3.5, _ condition: () async -> Bool) async throws -> Bool {
        let deadline = ProcessInfo.processInfo.systemUptime + timeout
        while ProcessInfo.processInfo.systemUptime < deadline {
            if await condition() {return true}
            try await Task.sleep(nanoseconds: 20_000_000)
        }
        return await condition()
    }
    func testRemovingOneViewKeepsPollingAndFinalReleaseStopsIt() async throws {
        let (model, probe) = fixture(), first = UUID(), second = UUID()
        defer {model.shutdown()}
        model.startObserving(owner: first)
        model.startObserving(owner: first) // Repeated onAppear for the same view.
        model.startObserving(owner: second)
        let initial = try await waitUntil {model.snapshot != nil}
        XCTAssertTrue(initial)
        let firstTime = try XCTUnwrap(model.snapshot).observedAt
        model.stopObserving(owner: UUID()) // Unknown view cannot cancel anything.
        model.stopObserving(owner: second)
        model.stopObserving(owner: second) // Late onDisappear for an inactive tab.
        let advanced = try await waitUntil {await probe.completed >= 2}
        XCTAssertTrue(advanced, "Closing another view stopped the surviving view's observer")
        XCTAssertGreaterThan(try XCTUnwrap(model.snapshot).observedAt, firstTime)
        let maximum = await probe.maximumActive
        XCTAssertEqual(maximum, 1)
        model.stopObserving(owner: first)
        let stoppedAt = await probe.reads
        try await Task.sleep(nanoseconds: 2_800_000_000)
        let afterStop = await probe.reads
        XCTAssertEqual(afterStop, stoppedAt, "Duplicate appearance retained an extra observer owner")
    }
    func testFinalReleaseCancelsReadAndRapidReacquireHasNoOverlap() async throws {
        let (model, probe) = fixture(), first = UUID(), replacement = UUID()
        defer {model.shutdown()}
        await probe.setDelay(5_000_000_000)
        model.startObserving(owner: first)
        let started = try await waitUntil(timeout: 1) {await probe.active == 1}
        XCTAssertTrue(started)
        model.stopObserving(owner: first)
        await probe.setDelay(10_000_000)
        model.startObserving(owner: replacement)
        let resumed = try await waitUntil {model.snapshot != nil}
        XCTAssertTrue(resumed)
        let cancelled = await probe.cancelled, maximum = await probe.maximumActive
        XCTAssertEqual(cancelled, 1)
        XCTAssertEqual(maximum, 1)
        model.stopObserving(owner: replacement)
        let finished = try await waitUntil(timeout: 1) {await probe.active == 0}
        XCTAssertTrue(finished)
    }
    func testShutdownWithOwnersCannotRestartObservation() async throws {
        let (model, probe) = fixture(), first = UUID(), second = UUID()
        model.startObserving(owner: first); model.startObserving(owner: second)
        let initial = try await waitUntil(timeout: 1) {model.snapshot != nil}
        XCTAssertTrue(initial)
        try await model.shutdownAndWait()
        let stoppedAt = await probe.reads
        model.startObserving(owner: first); model.startObserving(owner: UUID())
        model.stopObserving(owner: second)
        try await Task.sleep(nanoseconds: 100_000_000)
        let after = await probe.reads, active = await probe.active
        XCTAssertEqual(after, stoppedAt); XCTAssertEqual(active, 0)
        XCTAssertNil(model.snapshot)
        XCTAssertFalse(model.observationHealth(now: Date().timeIntervalSince1970).contains("Connected"))
    }
    func testHealthExpiresAtTheSameBoundaryAsTheRecordAndPreservesErrors() async throws {
        let (model, probe) = fixture()
        defer {model.shutdown()}
        await model.refresh()
        let observed = try XCTUnwrap(model.snapshot).observedAt
        XCTAssertTrue(model.observationHealth(now: observed + 10).contains("Connected"))
        for clock in [observed + 11, observed - 1, Double.nan, Double.infinity] {
            XCTAssertFalse(model.observationHealth(now: clock).contains("Connected"))
            XCTAssertThrowsError(try model.record(slot: 0, now: clock))
        }
        var changed = model.configuration; changed.source = SourceKey(socket: "/changed/socket")
        try model.edit(changed, expectedGeneration: model.generation)
        XCTAssertTrue(model.observationHealth(now: observed).contains("Source changed"))
        await probe.disconnect(); await model.refresh()
        XCTAssertTrue(model.observationHealth(now: Date().timeIntervalSince1970).contains("Probe disconnected"))
    }
}
