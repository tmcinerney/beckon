import XCTest
@testable import BeckonControl

@MainActor final class HerdrFocusContractTests: XCTestCase {
    let runtimeA = HerdrRuntimeReference(server: "server-A", terminal: "terminal", epoch: 1)
    let clientA = HerdrClientReference(incarnation: "client-A", endpoint: "local", generation: 1)
    let hostA = TerminalHostReference(surface: "surface", transport: "pty-A", generation: 1)
    func assignment(_ slot: HerdrFocusSlot = .agent4,
                    runtime: HerdrRuntimeReference? = nil) -> HerdrFocusAssignment {
        HerdrFocusAssignment(slot: slot, runtime: runtime ?? runtimeA,
            coordinator: slot.rawValue < 3 ? HerdrCoordinatorBinding(board: "board", registration: slot.title, fingerprint: "accepted") : nil)
    }
    func target(_ binding: HerdrFocusAssignment? = nil, pane: String = "pane") -> HerdrFocusTarget {
        HerdrFocusTarget(assignment: binding ?? assignment(), pane: pane, attachmentEpoch: 1,
                         client: clientA, host: hostA)
    }
    func request(_ binding: HerdrFocusAssignment? = nil, id: String = "tx", caller: String = "caller",
                 expiresAt: UInt64 = 100, generation: UInt64 = 1) -> HerdrFocusRequest {
        let binding = binding ?? assignment()
        return HerdrFocusRequest(transaction: HerdrFocusTransaction(caller: caller, server: "server-A", id: id),
            slot: binding.slot, configuration: HerdrFocusConfiguration(generation: generation, assignments: [binding]),
            expiresAt: expiresAt)
    }
    private func fixture(_ targets: [HerdrFocusTarget]? = nil, capacity: Int = 128) -> FocusFixture {
        FocusFixture(targets: targets ?? [target()], capacity: capacity)
    }
    func assertRefused(_ result: HerdrFocusResult, file: StaticString = #filePath, line: UInt = #line) {
        guard case .finished(.refused) = result.state else {
            return XCTFail("Expected refusal; got \(result)", file: file, line: line)
        }
    }
    func testReusedTerminalIDCannotRetargetAcceptedRuntime() {
        let f = fixture(), r = request()
        XCTAssertEqual(f.controller.prepare(r, now: 1).state, .prepared)
        let replacement = HerdrRuntimeReference(server: "server-A", terminal: "terminal", epoch: 2)
        f.runtime.targets = [target(assignment(runtime: replacement))]
        assertRefused(f.controller.commit(r.transaction, current: r.configuration, now: 2))
        XCTAssertEqual(f.runtime.selected.count, 0)
        XCTAssertEqual(f.client.outputs.count, 0)
        XCTAssertEqual(f.host.activations.count, 0)
    }
    func testDelayedHostWorkCannotActivateReplacementAfterCancellation() {
        let f = fixture(), r = request()
        XCTAssertEqual(f.controller.prepare(r, now: 1).state, .prepared)
        XCTAssertEqual(f.controller.commit(r.transaction, current: r.configuration, now: 2).state, .awaitingConfirmation)
        _ = f.controller.cancel(r.transaction)
        let replacement = TerminalHostReference(surface: "surface", transport: "pty-B", generation: 2)
        XCTAssertTrue(f.host.adopt(replacement))
        f.host.applyWindow(r.transaction, now: 3)
        f.host.applyResponder(r.transaction, now: 3)
        XCTAssertEqual(f.host.activations, [])
        XCTAssertEqual(f.host.responders, [])
    }
    func testSixAcceptedSlotsSurviveReorderAndUniqueAttachmentMove() {
        let bindings = HerdrFocusSlot.allCases.map { slot in
            assignment(slot, runtime: HerdrRuntimeReference(server: "server-A", terminal: "t-\(slot.rawValue)", epoch: 1))
        }
        let config = HerdrFocusConfiguration(generation: 1, assignments: bindings)
        let f = fixture(bindings.reversed().map { target($0, pane: "moved-\($0.slot.rawValue)") })
        for slot in HerdrFocusSlot.allCases {
            let r = HerdrFocusRequest(transaction: HerdrFocusTransaction(caller: "caller", server: "server-A", id: slot.title),
                                     slot: slot, configuration: config, expiresAt: 100)
            XCTAssertEqual(config.assignment(for: slot), bindings[slot.rawValue])
            let now = UInt64(slot.rawValue * 4 + 1)
            XCTAssertEqual(f.controller.prepare(r, now: now).state, .prepared)
            XCTAssertEqual(f.controller.commit(r.transaction, current: config, now: now + 1).state, .awaitingConfirmation)
            f.host.applyWindow(r.transaction, now: now + 2); f.host.applyResponder(r.transaction, now: now + 2)
            guard case .finished(.confirmed(let receipt)) = f.controller.complete(r.transaction, now: now + 2).state else {
                return XCTFail("Expected committed target and host receipt")
            }
            XCTAssertEqual(receipt.output.target.assignment, bindings[slot.rawValue])
            XCTAssertEqual(receipt.output.target.pane, "moved-\(slot.rawValue)")
        }
        XCTAssertEqual(f.runtime.selected.count, 6)
    }
    func testMissingDuplicateAndChangedCoordinatorAssignmentsRefuse() {
        let b = assignment(.chief)
        let r = request(b)
        let changed = HerdrFocusAssignment(slot: .chief, runtime: runtimeA,
            coordinator: HerdrCoordinatorBinding(board: "other", registration: b.coordinator!.registration, fingerprint: "changed"))
        for records in [[], [target(b), target(b)], [target(changed)]] {
            let f = fixture(records.isEmpty ? [target()] : records)
            if records.isEmpty { f.runtime.targets = [] }
            assertRefused(f.controller.prepare(r, now: 1))
            XCTAssertEqual(f.runtime.selected.count, 0)
        }
    }
    func testDuplicateConfigAndUnassignedSlotRefuseWithoutEffects() {
        let f = fixture(), b = assignment()
        for assignments in [[], [b, b], [assignment(.chief), assignment(.agent4)]] {
            let r = HerdrFocusRequest(transaction: HerdrFocusTransaction(caller: "caller", server: "server-A", id: "bad-\(assignments.count)"),
                slot: .agent4, configuration: HerdrFocusConfiguration(generation: 1, assignments: assignments), expiresAt: 100)
            assertRefused(f.controller.prepare(r, now: 1))
        }
        XCTAssertEqual(f.runtime.selected.count, 0)
    }
    func testEveryMissingCapabilityRefusesWithoutResolvingOrReserving() {
        for port in 0..<3 {
            let f = fixture(), r = request()
            if port == 0 { f.runtime.supported = false }
            if port == 1 { f.client.supported = false }
            if port == 2 { f.host.supported = false }
            assertRefused(f.controller.prepare(r, now: 1))
            XCTAssertEqual(f.runtime.resolutions, 0)
            XCTAssertTrue(f.runtime.held.isEmpty && f.client.held.isEmpty && f.host.held.isEmpty)
        }
    }
    func testConfigChangeMoveAndClientHostReplacementRefuseBeforeCommit() {
        for change in 0..<4 {
            let f = fixture(), r = request()
            XCTAssertEqual(f.controller.prepare(r, now: 1).state, .prepared)
            var config = r.configuration
            if change == 0 { config = HerdrFocusConfiguration(generation: 2, assignments: config.assignments) }
            if change == 1 { f.runtime.targets = [target(pane: "moved-after-prepare")] }
            if change == 2 { f.client.reference = HerdrClientReference(incarnation: "new-client", endpoint: "local", generation: 2) }
            if change == 3 { f.host.reference = TerminalHostReference(surface: "surface", transport: "pty-B", generation: 2) }
            assertRefused(f.controller.commit(r.transaction, current: config, now: 2))
            XCTAssertTrue(f.runtime.selected.isEmpty && f.client.outputs.isEmpty && f.host.activations.isEmpty)
        }
    }
    func testExpiryAndBackwardClockRefuseBeforeCommit() {
        for now: UInt64 in [0, 100] {
            let f = fixture(), r = request()
            XCTAssertEqual(f.controller.prepare(r, now: 1).state, .prepared)
            assertRefused(f.controller.commit(r.transaction, current: r.configuration, now: now))
            XCTAssertTrue(f.runtime.selected.isEmpty)
        }
    }
    func testLostRuntimeCommitOrClientOutputIsPartialNotRefused() {
        for runtimeLoss in [true, false] {
            let f = fixture(), r = request()
            f.runtime.loseCommit = runtimeLoss; f.client.loseOutput = !runtimeLoss
            XCTAssertEqual(f.controller.prepare(r, now: 1).state, .prepared)
            guard case .finished(.selectedUnconfirmed) = f.controller.commit(r.transaction, current: r.configuration, now: 2).state else {
                return XCTFail("Postcommit loss must not claim refusal or confirmation")
            }
            XCTAssertEqual(f.runtime.selected.count, 1)
            XCTAssertTrue(f.host.queued.isEmpty)
        }
    }
    func testWrongClientOutputCannotQueueHostWork() {
        let f = fixture(), r = request(); f.client.wrongOutput = true
        XCTAssertEqual(f.controller.prepare(r, now: 1).state, .prepared)
        guard case .finished(.selectedUnconfirmed) = f.controller.commit(r.transaction, current: r.configuration, now: 2).state else {
            return XCTFail("Wrong output must remain unconfirmed")
        }
        XCTAssertTrue(f.host.queued.isEmpty)
    }
    func testBothHostQueuesCheckExpiryAndAssociationAtApplyTime() {
        for expireBeforeWindow in [true, false] {
            let f = fixture(), r = request()
            _ = f.controller.prepare(r, now: 1)
            _ = f.controller.commit(r.transaction, current: r.configuration, now: 2)
            if !expireBeforeWindow { f.host.applyWindow(r.transaction, now: 3) }
            f.host.reference = TerminalHostReference(surface: "surface", transport: "pty-B", generation: 2)
            f.host.applyWindow(r.transaction, now: 100)
            f.host.applyResponder(r.transaction, now: 100)
            XCTAssertFalse(f.host.activations.contains(f.host.reference))
            XCTAssertTrue(f.host.responders.isEmpty)
            guard case .finished(.selectedUnconfirmed) = f.controller.complete(r.transaction, now: 100).state else {
                return XCTFail("Expiry after commit must be unconfirmed")
            }
        }
    }
    func testOutputFlushAloneAndWrongHostReceiptCannotConfirm() {
        for wrongReceipt in [true, false] {
            let f = fixture(), r = request(); f.host.wrongReceipt = wrongReceipt
            _ = f.controller.prepare(r, now: 1)
            _ = f.controller.commit(r.transaction, current: r.configuration, now: 2)
            if wrongReceipt { f.host.applyWindow(r.transaction, now: 3); f.host.applyResponder(r.transaction, now: 3) }
            if !wrongReceipt { XCTAssertEqual(f.controller.complete(r.transaction, now: 3).state, .awaitingConfirmation) }
            guard case .finished(.selectedUnconfirmed) = f.controller.complete(r.transaction, now: wrongReceipt ? 3 : 100).state else {
                return XCTFail("Output is not host receipt and altered receipt is not confirmation")
            }
        }
    }
    func testCancellationRetiresBeforeReplacementAndNeverRollsBackSelection() {
        let f = fixture(), r = request()
        _ = f.controller.prepare(r, now: 1)
        _ = f.controller.commit(r.transaction, current: r.configuration, now: 2)
        XCTAssertFalse(f.host.adopt(TerminalHostReference(surface: "surface", transport: "pty-B", generation: 2)))
        guard case .finished(.selectedUnconfirmed) = f.controller.cancel(r.transaction).state else { return XCTFail("Partial selection") }
        XCTAssertEqual(f.runtime.selected.count, 1)
        XCTAssertTrue(f.runtime.held.isEmpty && f.client.held.isEmpty && f.host.held.isEmpty)
        XCTAssertTrue(f.host.queued.isEmpty)
    }
    func testUnprovedRetirementBlocksNewRequestsAndKeepsClientRuntimeHeld() {
        let f = fixture(), r = request()
        _ = f.controller.prepare(r, now: 1)
        _ = f.controller.commit(r.transaction, current: r.configuration, now: 2)
        f.host.retirementAvailable = false
        _ = f.controller.cancel(r.transaction)
        XCTAssertFalse(f.controller.reconcileRetirement(r.transaction))
        XCTAssertFalse(f.runtime.held.isEmpty || f.client.held.isEmpty)
        assertRefused(f.controller.prepare(request(id: "next"), now: 3))
        XCTAssertFalse(f.host.adopt(TerminalHostReference(surface: "surface", transport: "pty-B", generation: 2)))
        f.host.retirementAvailable = true
        XCTAssertTrue(f.controller.reconcileRetirement(r.transaction))
        XCTAssertEqual(f.controller.prepare(request(id: "new"), now: 4).state, .prepared)
    }
    func testPostcommitRuntimeOrClientReplacementCannotApplyEitherHostQueue() {
        for replaceRuntime in [true, false] {
            let f = fixture(), r = request()
            _ = f.controller.prepare(r, now: 1)
            _ = f.controller.commit(r.transaction, current: r.configuration, now: 2)
            if replaceRuntime {
                f.runtime.targets = [target(assignment(runtime: HerdrRuntimeReference(server: "server-A", terminal: "terminal", epoch: 2)))]
            } else {
                f.client.reference = HerdrClientReference(incarnation: "client-B", endpoint: "other", generation: 2)
            }
            f.host.applyWindow(r.transaction, now: 3); f.host.applyResponder(r.transaction, now: 3)
            XCTAssertTrue(f.host.activations.isEmpty && f.host.responders.isEmpty)
            guard case .finished(.selectedUnconfirmed) = f.controller.complete(r.transaction, now: 100).state else {
                return XCTFail("Postcommit replacement cannot confirm original identity")
            }
        }
    }
    func testCapabilityRevocationBeforeCommitRefusesWithoutEffects() {
        for port in 0..<3 {
            let f = fixture(), r = request()
            XCTAssertEqual(f.controller.prepare(r, now: 1).state, .prepared)
            if port == 0 { f.runtime.supported = false }
            if port == 1 { f.client.supported = false }
            if port == 2 { f.host.supported = false }
            assertRefused(f.controller.commit(r.transaction, current: r.configuration, now: 2))
            XCTAssertTrue(f.runtime.selected.isEmpty && f.client.outputs.isEmpty && f.host.queued.isEmpty)
        }
    }
    func testCapabilityRevocationAfterCommitBlocksBothHostQueuesAndConfirmation() {
        for port in 0..<3 {
            let f = fixture(), r = request()
            _ = f.controller.prepare(r, now: 1)
            _ = f.controller.commit(r.transaction, current: r.configuration, now: 2)
            if port == 0 { f.runtime.supported = false }
            if port == 1 { f.client.supported = false }
            if port == 2 { f.host.supported = false }
            f.host.applyWindow(r.transaction, now: 3); f.host.applyResponder(r.transaction, now: 3)
            XCTAssertTrue(f.host.activations.isEmpty && f.host.responders.isEmpty)
            guard case .finished(.selectedUnconfirmed) = f.controller.complete(r.transaction, now: 3).state else {
                return XCTFail("Revoked capability cannot confirm or leave delayed work active")
            }
            XCTAssertTrue(f.host.queued.isEmpty)
        }
    }
    func testDefaultControllerHasNoLiveCapability() {
        let controller = HerdrFocusController()
        XCTAssertFalse(controller.supported)
        assertRefused(controller.prepare(request(), now: 1))
    }
    func testReplayIsHistoricalAndChangedFingerprintCannotReexecute() {
        let f = fixture(), r = request()
        _ = f.controller.prepare(r, now: 1)
        _ = f.controller.commit(r.transaction, current: r.configuration, now: 2)
        f.host.applyWindow(r.transaction, now: 3); f.host.applyResponder(r.transaction, now: 3)
        let done = f.controller.complete(r.transaction, now: 3)
        XCTAssertTrue(f.controller.prepare(r, now: 4).replay)
        XCTAssertEqual(f.controller.commit(r.transaction, current: r.configuration, now: 4).state, done.state)
        XCTAssertTrue(f.controller.complete(r.transaction, now: 4).replay)
        assertRefused(f.controller.prepare(request(expiresAt: 101), now: 4))
        XCTAssertEqual(f.runtime.selected.count, 1)
        XCTAssertEqual(f.host.activations.count, 1)
    }
    func testCallerNamespaceCapacityAndPendingRequestAreBounded() {
        let f = fixture(capacity: 2), r = request()
        _ = f.controller.prepare(r, now: 1)
        assertRefused(f.controller.prepare(request(caller: "second"), now: 1))
        assertRefused(f.controller.cancel(r.transaction))
        XCTAssertTrue(f.controller.prepare(r, now: 2).replay)
        assertRefused(f.controller.prepare(request(id: "full"), now: 2))
        XCTAssertEqual(f.runtime.selected.count, 0)
        let g = fixture()
        _ = g.controller.prepare(r, now: 1); _ = g.controller.cancel(r.transaction)
        XCTAssertEqual(g.controller.prepare(request(caller: "second"), now: 2).state, .prepared)
    }
}

@MainActor private final class FocusFixture {
    let runtime: FakeHerdrRuntime
    let client: FakeHerdrClient
    let host: FakeTerminalHost
    let controller: HerdrFocusController
    init(targets: [HerdrFocusTarget], capacity: Int) {
        runtime = FakeHerdrRuntime(targets: targets)
        client = FakeHerdrClient(reference: targets.first!.client)
        host = FakeTerminalHost(reference: targets.first!.host)
        // Offline joined owner state models a capability the real installed host does not expose.
        host.bindingStillValid = { [runtime, client] target, request in
            runtime.validate(target, request: request) && client.validate(target, request: request)
        }
        controller = HerdrFocusController(runtime: runtime, client: client, host: host, capacity: capacity)
    }
}
@MainActor private final class FakeHerdrRuntime: HerdrRuntimeFocusPort {
    var supported = true
    var targets: [HerdrFocusTarget]
    var selected: [HerdrFocusTarget] = []
    var resolutions = 0
    var loseCommit = false
    var held: [HerdrFocusTransaction: HerdrFocusTarget] = [:]
    init(targets: [HerdrFocusTarget]) { self.targets = targets }
    func resolve(_ assignment: HerdrFocusAssignment) -> HerdrFocusTarget? {
        resolutions += 1
        let matches = targets.filter { $0.assignment == assignment }
        return matches.count == 1 ? matches[0] : nil
    }
    func reserve(_ target: HerdrFocusTarget, request: HerdrFocusRequest) -> Bool {
        guard resolve(target.assignment) == target else { return false }
        held[request.transaction] = target; return true
    }
    func validate(_ target: HerdrFocusTarget, request: HerdrFocusRequest) -> Bool {
        supported && held[request.transaction] == target && resolve(target.assignment) == target
    }
    func commit(_ target: HerdrFocusTarget, request: HerdrFocusRequest) -> HerdrRuntimeCommit {
        guard validate(target, request: request) else { return .refused }
        selected.append(target); return loseCommit ? .uncertain : .selected
    }
    func retire(_ transaction: HerdrFocusTransaction) -> Bool { held[transaction] = nil; return true }

}
@MainActor private final class FakeHerdrClient: HerdrClientFocusPort {
    var supported = true
    var reference: HerdrClientReference
    var outputs: [HerdrOutputReceipt] = []
    var held: [HerdrFocusTransaction: HerdrFocusTarget] = [:]
    var loseOutput = false
    var wrongOutput = false
    init(reference: HerdrClientReference) { self.reference = reference }
    func reserve(_ target: HerdrFocusTarget, request: HerdrFocusRequest) -> Bool {
        guard reference == target.client else { return false }
        held[request.transaction] = target; return true
    }
    func validate(_ target: HerdrFocusTarget, request: HerdrFocusRequest) -> Bool {
        supported && held[request.transaction] == target && reference == target.client
    }
    func commit(_ target: HerdrFocusTarget, request: HerdrFocusRequest) -> HerdrOutputReceipt? {
        guard validate(target, request: request) else { return nil }
        let output = HerdrOutputReceipt(transaction: request.transaction, target: target, projection: wrongOutput ? 0 : 1)
        outputs.append(output); return loseOutput ? nil : output
    }
    func retire(_ transaction: HerdrFocusTransaction) -> Bool { held[transaction] = nil; return true }
}
@MainActor private final class FakeTerminalHost: TerminalHostFocusPort {
    var supported = true
    var reference: TerminalHostReference
    var queued: [HerdrFocusTransaction: (HerdrOutputReceipt, HerdrFocusRequest)] = [:]
    var held: [HerdrFocusTransaction: HerdrFocusTarget] = [:]
    var receipts: [HerdrFocusTransaction: TerminalHostReceipt] = [:]
    var activations: [TerminalHostReference] = []
    var responders: [TerminalHostReference] = []
    var bindingStillValid: (HerdrFocusTarget, HerdrFocusRequest) -> Bool = { _, _ in false }
    var retirementAvailable = true
    var wrongReceipt = false
    var windows: Set<HerdrFocusTransaction> = []
    var responderTickets: Set<HerdrFocusTransaction> = []
    init(reference: TerminalHostReference) { self.reference = reference }
    func reserve(_ target: HerdrFocusTarget, request: HerdrFocusRequest) -> Bool {
        guard reference == target.host, bindingStillValid(target, request) else { return false }
        held[request.transaction] = target; return true
    }
    func validate(_ target: HerdrFocusTarget, request: HerdrFocusRequest) -> Bool {
        supported && held[request.transaction] == target && reference == target.host && bindingStillValid(target, request)
    }
    func enqueue(_ output: HerdrOutputReceipt, request: HerdrFocusRequest) -> Bool {
        guard output.transaction == request.transaction, validate(output.target, request: request) else { return false }
        queued[request.transaction] = (output, request); return true
    }
    func receipt(_ transaction: HerdrFocusTransaction) -> TerminalHostReceipt? { receipts[transaction] }
    func retire(_ transaction: HerdrFocusTransaction) -> Bool {
        guard retirementAvailable else { return false }
        held[transaction] = nil; queued[transaction] = nil; receipts[transaction] = nil
        windows.remove(transaction); responderTickets.remove(transaction); return true
    }
    func adopt(_ next: TerminalHostReference) -> Bool {
        guard held.isEmpty && queued.isEmpty else { return false }; reference = next; return true
    }
    func applyWindow(_ transaction: HerdrFocusTransaction, now: UInt64) {
        guard let (output, request) = queued[transaction], now < request.expiresAt,
              validate(output.target, request: request), windows.insert(transaction).inserted else { return }
        activations.append(reference)
    }
    func applyResponder(_ transaction: HerdrFocusTransaction, now: UInt64) {
        guard let (output, request) = queued[transaction], now < request.expiresAt,
              validate(output.target, request: request), windows.contains(transaction),
              responderTickets.insert(transaction).inserted else { return }
        responders.append(reference)
        receipts[transaction] = TerminalHostReceipt(output: HerdrOutputReceipt(transaction: output.transaction, target: output.target, projection: wrongReceipt ? 999 : output.projection))
    }
}
