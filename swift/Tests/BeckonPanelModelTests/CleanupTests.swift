import XCTest
@testable import BeckonPanelModel
final class CleanupTests:XCTestCase {
    func testUnconfirmedCleanupRemainsLockedUntilExplicitReconciliation() throws {
        var gate=CleanupGate()
        gate.requireReconciliation(pid:123)
        XCTAssertEqual(gate.pendingPID,123)
        XCTAssertThrowsError(try gate.reconcile())
        gate.observedReaped(pid:999)
        XCTAssertFalse(gate.reaped)
        gate.observedReaped(pid:123)
        XCTAssertTrue(gate.reaped)
        XCTAssertEqual(gate.pendingPID,123,"Exit notification must not auto-release capacity")
        try gate.reconcile()
        XCTAssertNil(gate.pendingPID)
    }
}
