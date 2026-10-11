import Foundation

/// A Herdr-owned terminal lifetime, deliberately independent of native harness conversations.
public struct HerdrRuntimeReference: Hashable, Sendable {
    public let server: String
    public let terminal: String
    public let epoch: UInt64
    public init(server: String, terminal: String, epoch: UInt64) {
        self.server = server; self.terminal = terminal; self.epoch = epoch
    }
}
public enum HerdrFocusSlot: Int, CaseIterable, Sendable {
    case chief, projectCoordinator, releases, agent4, agent5, agent6
    public var title: String {
        ["Chief of Staff", "Project Coordinator", "Releases", "Selectable agent 4",
         "Selectable agent 5", "Selectable agent 6"][rawValue]
    }
}
public struct HerdrCoordinatorBinding: Equatable, Sendable {
    public let board: String
    public let registration: String
    public let fingerprint: String
    public init(board: String, registration: String, fingerprint: String) {
        self.board = board; self.registration = registration; self.fingerprint = fingerprint
    }
}
/// Ephemeral projection of accepted assignments, not a second configuration store or registry.
public struct HerdrFocusAssignment: Equatable, Sendable {
    public let slot: HerdrFocusSlot
    public let runtime: HerdrRuntimeReference
    public let coordinator: HerdrCoordinatorBinding?
    public init(slot: HerdrFocusSlot, runtime: HerdrRuntimeReference,
                coordinator: HerdrCoordinatorBinding? = nil) {
        self.slot = slot; self.runtime = runtime; self.coordinator = coordinator
    }
}
public struct HerdrFocusConfiguration: Equatable, Sendable {
    public let generation: UInt64
    public let assignments: [HerdrFocusAssignment]
    public init(generation: UInt64, assignments: [HerdrFocusAssignment]) {
        self.generation = generation; self.assignments = assignments
    }
    public func assignment(for slot: HerdrFocusSlot) -> HerdrFocusAssignment? {
        let matches = assignments.filter { $0.slot == slot }
        return matches.count == 1 ? matches[0] : nil
    }
}
public struct HerdrClientReference: Hashable, Sendable {
    public let incarnation: String
    public let endpoint: String
    public let generation: UInt64
    public init(incarnation: String, endpoint: String, generation: UInt64) {
        self.incarnation = incarnation; self.endpoint = endpoint; self.generation = generation
    }
}
public struct TerminalHostReference: Hashable, Sendable {
    public let surface: String
    public let transport: String
    public let generation: UInt64
    public init(surface: String, transport: String, generation: UInt64) {
        self.surface = surface; self.transport = transport; self.generation = generation
    }
}
public struct HerdrFocusTarget: Equatable, Sendable {
    public let assignment: HerdrFocusAssignment
    public let pane: String
    public let attachmentEpoch: UInt64
    public let client: HerdrClientReference
    public let host: TerminalHostReference
    public init(assignment: HerdrFocusAssignment, pane: String, attachmentEpoch: UInt64,
                client: HerdrClientReference, host: TerminalHostReference) {
        self.assignment = assignment; self.pane = pane; self.attachmentEpoch = attachmentEpoch
        self.client = client; self.host = host
    }
}
/// Namespaced transaction identity; an RPC correlation ID is not this reference.
public struct HerdrFocusTransaction: Hashable, Sendable {
    public let caller: String
    public let server: String
    public let id: String
    public init(caller: String, server: String, id: String) {
        self.caller = caller; self.server = server; self.id = id
    }
}
public struct HerdrFocusRequest: Equatable, Sendable {
    public let transaction: HerdrFocusTransaction
    public let slot: HerdrFocusSlot
    public let configuration: HerdrFocusConfiguration
    public let expiresAt: UInt64
    public init(transaction: HerdrFocusTransaction, slot: HerdrFocusSlot,
                configuration: HerdrFocusConfiguration, expiresAt: UInt64) {
        self.transaction = transaction; self.slot = slot
        self.configuration = configuration; self.expiresAt = expiresAt
    }
}
public struct HerdrOutputReceipt: Equatable, Sendable {
    public let transaction: HerdrFocusTransaction
    public let target: HerdrFocusTarget
    public let projection: UInt64
    public init(transaction: HerdrFocusTransaction, target: HerdrFocusTarget, projection: UInt64) {
        self.transaction = transaction; self.target = target; self.projection = projection
    }
}
public struct TerminalHostReceipt: Equatable, Sendable {
    public let output: HerdrOutputReceipt
    public init(output: HerdrOutputReceipt) { self.output = output }
}
public enum HerdrFocusOutcome: Equatable, Sendable {
    case refused(String)
    case selectedUnconfirmed(String)
    /// Host selection plus identified output commitment; never proof of pixels/native conversation.
    case confirmed(TerminalHostReceipt)
}
public enum HerdrFocusState: Equatable, Sendable {
    case prepared
    case awaitingConfirmation
    case finished(HerdrFocusOutcome)
}
public struct HerdrFocusResult: Equatable, Sendable {
    public let state: HerdrFocusState
    /// Historical replay must not be displayed as a new successful focus operation.
    public let replay: Bool
    public init(state: HerdrFocusState, replay: Bool = false) { self.state = state; self.replay = replay }
}

public enum HerdrRuntimeCommit: Sendable {
    case refused
    case selected
    case uncertain
}
/// Implementations must reserve at the owner before effects, and invalidate/drain on replacement.
/// A refused commit guarantees zero effects; lost transport acknowledgements are uncertain.
@MainActor public protocol HerdrRuntimeFocusPort: AnyObject {
    var supported: Bool { get }
    func resolve(_ assignment: HerdrFocusAssignment) -> HerdrFocusTarget?
    func reserve(_ target: HerdrFocusTarget, request: HerdrFocusRequest) -> Bool
    func validate(_ target: HerdrFocusTarget, request: HerdrFocusRequest) -> Bool
    func commit(_ target: HerdrFocusTarget, request: HerdrFocusRequest) -> HerdrRuntimeCommit
    func retire(_ transaction: HerdrFocusTransaction) -> Bool
}
@MainActor public protocol HerdrClientFocusPort: AnyObject {
    var supported: Bool { get }
    func reserve(_ target: HerdrFocusTarget, request: HerdrFocusRequest) -> Bool
    func validate(_ target: HerdrFocusTarget, request: HerdrFocusRequest) -> Bool
    func commit(_ target: HerdrFocusTarget, request: HerdrFocusRequest) -> HerdrOutputReceipt?
    func retire(_ transaction: HerdrFocusTransaction) -> Bool
}
/// A host must check capability, the sealed association and deadline in BOTH focus queues.
/// Retire may return true only when every queued effect is drained/invalidated before reuse.
@MainActor public protocol TerminalHostFocusPort: AnyObject {
    var supported: Bool { get }
    func reserve(_ target: HerdrFocusTarget, request: HerdrFocusRequest) -> Bool
    func validate(_ target: HerdrFocusTarget, request: HerdrFocusRequest) -> Bool
    func enqueue(_ output: HerdrOutputReceipt, request: HerdrFocusRequest) -> Bool
    func receipt(_ transaction: HerdrFocusTransaction) -> TerminalHostReceipt?
    func retire(_ transaction: HerdrFocusTransaction) -> Bool
}

/// Offline contract prototype. No live port or application/hardware transport is implemented.
@MainActor public final class HerdrFocusController {
    private final class Entry {
        let request: HerdrFocusRequest
        var target: HerdrFocusTarget?
        var state: HerdrFocusState = .prepared
        var output: HerdrOutputReceipt?
        var committed = false
        var runtimeReserved = false
        var clientReserved = false
        var hostReserved = false
        var retirementPending = false
        init(_ request: HerdrFocusRequest) { self.request = request }
    }
    private let runtime: HerdrRuntimeFocusPort
    private let client: HerdrClientFocusPort
    private let host: TerminalHostFocusPort
    private let capacity: Int
    private var entries: [HerdrFocusTransaction: Entry] = [:]
    private var lastTime: UInt64?
    public var supported: Bool { runtime.supported && client.supported && host.supported }

    public init(runtime: HerdrRuntimeFocusPort, client: HerdrClientFocusPort,
                host: TerminalHostFocusPort, capacity: Int = 128) {
        self.runtime = runtime; self.client = client; self.host = host; self.capacity = capacity
    }
    /// Default construction cannot accidentally activate a transport. Test ports are opt-in.
    public convenience init() {
        self.init(runtime: UnavailableHerdrRuntime(), client: UnavailableHerdrClient(), host: UnavailableTerminalHost())
    }

    public func prepare(_ request: HerdrFocusRequest, now: UInt64) -> HerdrFocusResult {
        if let old = entries[request.transaction] {
            guard old.request == request else { return refusal("Changed transaction fingerprint") }
            return result(old, replay: true)
        }
        guard capacity > 0, capacity <= 1024, entries.count < capacity else { return refusal("Transaction capacity exhausted") }
        let entry = Entry(request); entries[request.transaction] = entry
        guard supported else { return finish(entry, .refused("Unsupported owner capability")) }
        guard acceptTime(now), valid(request, now: now) else { return finish(entry, .refused("Invalid request or clock")) }
        guard !entries.values.contains(where: { $0 !== entry && (active($0) || $0.retirementPending) }) else {
            return finish(entry, .refused("Another transaction or retirement is pending"))
        }
        guard let assignment = request.configuration.assignment(for: request.slot),
              let target = runtime.resolve(assignment), valid(target, expected: assignment) else {
            return finish(entry, .refused("Missing, stale or ambiguous assignment"))
        }
        entry.target = target
        entry.runtimeReserved = runtime.reserve(target, request: request)
        guard entry.runtimeReserved else { return finish(entry, .refused("Runtime reservation refused")) }
        entry.clientReserved = client.reserve(target, request: request)
        guard entry.clientReserved else { return finish(entry, .refused("Client reservation refused")) }
        entry.hostReserved = host.reserve(target, request: request)
        guard entry.hostReserved else { return finish(entry, .refused("Host association unavailable")) }
        return result(entry)
    }

    public func commit(_ transaction: HerdrFocusTransaction,
                       current: HerdrFocusConfiguration, now: UInt64) -> HerdrFocusResult {
        guard let entry = entries[transaction] else { return refusal("Unknown transaction") }
        guard entry.state == .prepared else { return result(entry, replay: true) }
        guard supported, acceptTime(now), now < entry.request.expiresAt,
              current == entry.request.configuration, let target = entry.target,
              runtime.validate(target, request: entry.request), client.validate(target, request: entry.request),
              host.validate(target, request: entry.request) else {
            return finish(entry, .refused("Prepared assignment or owner changed"))
        }
        switch runtime.commit(target, request: entry.request) {
        case .refused: return finish(entry, .refused("Runtime refused before selection"))
        case .uncertain:
            entry.committed = true
            return finish(entry, .selectedUnconfirmed("Runtime commit acknowledgement lost"))
        case .selected: entry.committed = true
        }
        guard let output = client.commit(target, request: entry.request), output.transaction == transaction,
              output.target == target, output.projection > 0 else {
            return finish(entry, .selectedUnconfirmed("Client output not confirmed"))
        }
        entry.output = output
        guard host.enqueue(output, request: entry.request) else {
            return finish(entry, .selectedUnconfirmed("Host enqueue not confirmed"))
        }
        entry.state = .awaitingConfirmation
        return result(entry)
    }

    public func complete(_ transaction: HerdrFocusTransaction, now: UInt64) -> HerdrFocusResult {
        guard let entry = entries[transaction] else { return refusal("Unknown transaction") }
        guard entry.state == .awaitingConfirmation else { return result(entry, replay: true) }
        guard supported, acceptTime(now), now < entry.request.expiresAt else {
            return finish(entry, .selectedUnconfirmed("Host confirmation expired or capability revoked"))
        }
        if let receipt = host.receipt(transaction) {
            guard let target = entry.target, receipt.output == entry.output,
                  runtime.validate(target, request: entry.request), client.validate(target, request: entry.request),
                  host.validate(target, request: entry.request) else {
                return finish(entry, .selectedUnconfirmed("Mismatched host receipt"))
            }
            return finish(entry, .confirmed(receipt))
        }
        return result(entry)
    }

    public func cancel(_ transaction: HerdrFocusTransaction) -> HerdrFocusResult {
        guard let entry = entries[transaction] else { return refusal("Unknown transaction") }
        guard active(entry) else { return result(entry, replay: true) }
        return finish(entry, entry.committed ? .selectedUnconfirmed("Cancelled after selection") : .refused("Cancelled before selection"))
    }
    public func status(_ transaction: HerdrFocusTransaction) -> HerdrFocusResult? {
        entries[transaction].map { result($0, replay: true) }
    }
    /// Explicit reconciliation only; never an automatic retry or re-execution of focus.
    public func reconcileRetirement(_ transaction: HerdrFocusTransaction) -> Bool {
        guard let entry = entries[transaction], !active(entry) else { return false }
        return retire(entry)
    }

    private func result(_ entry: Entry, replay: Bool = false) -> HerdrFocusResult {
        HerdrFocusResult(state: entry.state, replay: replay)
    }
    private func refusal(_ reason: String) -> HerdrFocusResult { HerdrFocusResult(state: .finished(.refused(reason))) }
    private func active(_ entry: Entry) -> Bool {
        if case .finished = entry.state { return false }; return true
    }
    private func finish(_ entry: Entry, _ outcome: HerdrFocusOutcome) -> HerdrFocusResult {
        entry.state = .finished(outcome)
        _ = retire(entry)
        return result(entry)
    }
    private func retire(_ entry: Entry) -> Bool {
        // AIDEV-NOTE: Do not release upstream ownership while delayed host work can still apply.
        // Retained reservations block replacement adoption until the actual owner proves retirement.
        if entry.hostReserved {
            guard host.retire(entry.request.transaction) else { entry.retirementPending = true; return false }
            entry.hostReserved = false
        }
        if entry.clientReserved {
            guard client.retire(entry.request.transaction) else { entry.retirementPending = true; return false }
            entry.clientReserved = false
        }
        if entry.runtimeReserved {
            guard runtime.retire(entry.request.transaction) else { entry.retirementPending = true; return false }
            entry.runtimeReserved = false
        }
        entry.retirementPending = false
        return true
    }
    private func acceptTime(_ now: UInt64) -> Bool {
        guard lastTime.map({ now >= $0 }) ?? true else { return false }; lastTime = now; return true
    }
    private func identifier(_ value: String) -> Bool {
        !value.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty && value.utf8.count <= 512
            && !value.unicodeScalars.contains(where: { CharacterSet.controlCharacters.contains($0) })
    }
    private func valid(_ request: HerdrFocusRequest, now: UInt64) -> Bool {
        let assignments = request.configuration.assignments
        guard identifier(request.transaction.caller), identifier(request.transaction.server), identifier(request.transaction.id),
              request.expiresAt > now, request.expiresAt - now <= 30_000, assignments.count <= 6,
              Set(assignments.map(\.slot)).count == assignments.count,
              Set(assignments.map(\.runtime)).count == assignments.count,
              let chosen = request.configuration.assignment(for: request.slot), chosen.runtime.server == request.transaction.server else { return false }
        return assignments.allSatisfy { binding in
            let runtime = binding.runtime
            guard identifier(runtime.server), identifier(runtime.terminal), runtime.epoch > 0 else { return false }
            if binding.slot.rawValue < 3 {
                guard let coordinator = binding.coordinator else { return false }
                return identifier(coordinator.board) && identifier(coordinator.registration) && identifier(coordinator.fingerprint)
            }
            return binding.coordinator == nil
        }
    }
    private func valid(_ target: HerdrFocusTarget, expected: HerdrFocusAssignment) -> Bool {
        target.assignment == expected && identifier(target.pane) && target.attachmentEpoch > 0
            && identifier(target.client.incarnation) && identifier(target.client.endpoint) && target.client.generation > 0
            && identifier(target.host.surface) && identifier(target.host.transport) && target.host.generation > 0
    }
}

@MainActor private final class UnavailableHerdrRuntime: HerdrRuntimeFocusPort {
    let supported = false
    func resolve(_ assignment: HerdrFocusAssignment) -> HerdrFocusTarget? { nil }
    func reserve(_ target: HerdrFocusTarget, request: HerdrFocusRequest) -> Bool { false }
    func validate(_ target: HerdrFocusTarget, request: HerdrFocusRequest) -> Bool { false }
    func commit(_ target: HerdrFocusTarget, request: HerdrFocusRequest) -> HerdrRuntimeCommit { .refused }
    func retire(_ transaction: HerdrFocusTransaction) -> Bool { true }
}
@MainActor private final class UnavailableHerdrClient: HerdrClientFocusPort {
    let supported = false
    func reserve(_ target: HerdrFocusTarget, request: HerdrFocusRequest) -> Bool { false }
    func validate(_ target: HerdrFocusTarget, request: HerdrFocusRequest) -> Bool { false }
    func commit(_ target: HerdrFocusTarget, request: HerdrFocusRequest) -> HerdrOutputReceipt? { nil }
    func retire(_ transaction: HerdrFocusTransaction) -> Bool { true }
}
@MainActor private final class UnavailableTerminalHost: TerminalHostFocusPort {
    let supported = false
    func reserve(_ target: HerdrFocusTarget, request: HerdrFocusRequest) -> Bool { false }
    func validate(_ target: HerdrFocusTarget, request: HerdrFocusRequest) -> Bool { false }
    func enqueue(_ output: HerdrOutputReceipt, request: HerdrFocusRequest) -> Bool { false }
    func receipt(_ transaction: HerdrFocusTransaction) -> TerminalHostReceipt? { nil }
    func retire(_ transaction: HerdrFocusTransaction) -> Bool { true }
}
