import Foundation
// AIDEV-NOTE: An exit notification records evidence; only explicit reconciliation releases this latch.
struct CleanupGate {
    private(set) var pendingPID:Int32?
    private(set) var reaped=false
    mutating func requireReconciliation(pid:Int32) {pendingPID=pid;reaped=false}
    mutating func observedReaped(pid:Int32) {if pendingPID==pid {reaped=true}}
    mutating func reconcile() throws {
        guard pendingPID != nil else {return}
        guard reaped else {throw PanelError.cleanupUnconfirmed}
        pendingPID=nil;reaped=false
    }
}
