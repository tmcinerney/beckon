import XCTest
import BeckonPanelModel
@testable import BeckonCreatorMicro

@MainActor final class ModelPreviewTests: XCTestCase {
    func testSharedModelDrivesBothViewsAcrossDemoTransitions() async throws {
        let bootstrap = try PanelBootstrap(arguments: ["--demo"], home: "/unused", allowPersistence: false)
        let demo = try XCTUnwrap(bootstrap.demo), model = bootstrap.model
        defer {model.shutdown()}
        for step in 0..<8 {
            await model.refresh()
            let now = Date().timeIntervalSince1970
            let frame = StatusAdapter.prepare(model: model, now: now)
            XCTAssertEqual(frame.threads.map(\.id), [1, 0, 2, 3, 4, 5])
            for slot in 0..<6 {
                let record = try? model.record(slot: slot, now: now)
                XCTAssertEqual(frame.slots[slot].presentation, record?.presentation)
                if record == nil {XCTAssertEqual(frame.threads[slot].brightness, 0)}
            }
            if step == 0 {XCTAssertEqual(frame.threads[0].color, 0x3BA0FF)}
            if step == 1 {XCTAssertEqual(frame.threads[0].color, 0x22C55E)}
            if step == 2 {XCTAssertEqual(frame.threads[0].color, 0xFFB020)}
            if step == 4 || step == 7 {XCTAssertNotNil(frame.slots[0].unavailableReason)}
            XCTAssertFalse(frame.focusAvailable); XCTAssertFalse(frame.liveWriterAvailable)
            await demo.next()
        }
    }
    func testAssignmentAndSourceChangesImmediatelyUpdateTheFrame() async throws {
        let bootstrap = try PanelBootstrap(arguments: ["--demo"], home: "/unused", allowPersistence: false)
        let model = bootstrap.model
        defer {model.shutdown()}
        await model.refresh()
        var changed = model.configuration
        changed.coordinators[0] = nil; changed.slots[0] = changed.slots[1]; changed.slots[1] = nil
        try model.edit(changed, expectedGeneration: model.generation)
        let first = StatusAdapter.prepare(model: model, now: Date().timeIntervalSince1970)
        XCTAssertEqual(first.threads[0].color, 0xFFFFFF)
        XCTAssertEqual(first.threads[1].brightness, 0)
        changed.source = SourceKey(socket: "/other/socket")
        try model.edit(changed, expectedGeneration: model.generation)
        XCTAssertNil(model.snapshot)
        let next = StatusAdapter.prepare(model: model, now: Date().timeIntervalSince1970)
        XCTAssertTrue(next.threads.allSatisfy {$0.brightness == 0})
    }
    func testClockExpiryAndShutdownCannotRetainHealthyLEDPreview() async throws {
        let bootstrap = try PanelBootstrap(arguments: ["--demo"], home: "/unused", allowPersistence: false)
        let model = bootstrap.model
        await model.refresh()
        let now = try XCTUnwrap(model.snapshot).observedAt
        XCTAssertEqual(StatusAdapter.prepare(model: model, now: now).threads[0].color, 0x3BA0FF)
        XCTAssertThrowsError(try model.record(slot: 0, now: now + 11))
        XCTAssertTrue(StatusAdapter.prepare(model: model, now: now + 11).threads.allSatisfy {$0.brightness == 0})
        try await model.shutdownAndWait()
        XCTAssertTrue(StatusAdapter.prepare(model: model, now: now).threads.allSatisfy {$0.brightness == 0})
    }
}
