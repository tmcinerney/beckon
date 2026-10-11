import Foundation

public enum ControlError: Error, Equatable {
    case invalidConfiguration, unavailable, ambiguous, stale, unsupported, wrongFocus, changedRequest, alreadyConsumed
}

public struct NativeIdentity: Codable, Hashable {
    public var provider: String
    public var sessionId: String

    var valid: Bool {
        !provider.isEmpty && provider.count <= 64 && provider.utf8.allSatisfy { byte in
            (48...57).contains(byte) || (65...90).contains(byte) || (97...122).contains(byte) || [45,46,95].contains(byte)
        } && !sessionId.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty && sessionId.count <= 512
            && !sessionId.unicodeScalars.contains(where: { CharacterSet.controlCharacters.contains($0) })
    }
}

public struct Surface: Codable, Equatable {
    public var kind: String
    public var session: String?
    public var paneId: String?
    public var threadId: String?

    func valid(for identity: NativeIdentity) -> Bool {
        switch kind {
        case "herdr":
            return !(session ?? "").isEmpty && !(paneId ?? "").isEmpty
        case "codex-desktop":
            return identity.provider == "codex" && threadId == identity.sessionId
        default: return false
        }
    }
}

public enum Attention: String, Codable {
    case approval, question, blocked, unread, none
    var rank: Int {
        switch self { case .approval: return 0; case .question: return 1; case .blocked: return 2; case .unread: return 3; case .none: return 4 }
    }
}
public enum Decision: String, Codable { case yes, no }
public struct PendingRequest: Codable, Equatable {
    public var id: String
    public var generation: Int
    public var kind: Attention
    public var choices: [Decision]
    public var expiresAt: Double
}

public struct LiveAgent: Codable {
    public var native: NativeIdentity
    public var boardId: String
    public var role: String?
    public var taskUid: String?
    public var surface: Surface
    public var observedAt: Double
    public var runtimeVerified: Bool
    public var attention: Attention
    public var canAnswer: Bool
    public var request: PendingRequest?

    func fresh(now: Double, age: Double) -> Bool {
        native.valid && runtimeVerified && observedAt.isFinite && now.isFinite
            && now >= observedAt && now - observedAt <= age && surface.valid(for: native)
    }
}

public struct Snapshot: Codable {
    public var version: Int
    public var agents: [LiveAgent]
}
public struct RoleBinding: Codable {
    public var key: Int
    public var boardId: String?
    public var role: String
    public var native: NativeIdentity
}
public struct RoutingConfig: Codable {
    public var version: Int
    public var bindings: [RoleBinding]
    public var boards: [String]
}
public struct FocusIntent: Codable, Equatable {
    public var native: NativeIdentity
    public var surface: Surface
    public var reason: String
}

/// Pure policy: returns intents. It never focuses a window, sends keys or opens HID.
public struct Router {
    let snapshot: Snapshot
    let config: RoutingConfig
    let maxAge: Double

    public init(snapshot: Snapshot, config: RoutingConfig, maxAge: Double = 10) throws {
        guard snapshot.version == 1, config.version == 1, maxAge.isFinite, maxAge > 0,
              Set(config.bindings.map(\.key)).count == config.bindings.count,
              Set(config.boards).count == config.boards.count,
              config.boards.allSatisfy({ !$0.isEmpty }),
              config.bindings.allSatisfy({ $0.native.valid && !$0.role.isEmpty && (0...5).contains($0.key)
                  && ($0.boardId.map { config.boards.contains($0) } ?? ($0.role == "chief-of-staff")) }),
              config.bindings.filter({ $0.role == "chief-of-staff" }).count <= 1
        else { throw ControlError.invalidConfiguration }
        self.snapshot = snapshot; self.config = config; self.maxAge = maxAge
    }

    func unique(_ candidates: [LiveAgent], now: Double, reason: String) throws -> FocusIntent {
        guard !candidates.isEmpty else { throw ControlError.unavailable }
        guard candidates.count == 1 else { throw ControlError.ambiguous }
        guard let agent=candidates.first else { throw ControlError.unavailable }
        guard snapshot.agents.filter({ $0.native == agent.native }).count == 1 else { throw ControlError.ambiguous }
        guard agent.fresh(now: now, age: maxAge) else { throw ControlError.stale }
        return FocusIntent(native: agent.native, surface: agent.surface, reason: reason)
    }

    public func role(key: Int, now: Double) throws -> FocusIntent {
        guard let binding=config.bindings.first(where: { $0.key == key }) else { throw ControlError.unavailable }
        return try unique(snapshot.agents.filter {
            $0.native == binding.native && $0.role == binding.role
                && (binding.boardId == nil || $0.boardId == binding.boardId)
        }, now: now, reason: binding.role)
    }

    public func task(_ uid: String, board: String, now: Double) throws -> FocusIntent {
        guard config.boards.contains(board), !uid.isEmpty else { throw ControlError.unavailable }
        return try unique(snapshot.agents.filter { $0.taskUid == uid && $0.boardId == board },
                          now: now, reason: "task")
    }

    public func board(relative step: Int, from board: String) throws -> String {
        guard step == 1 || step == -1, let index=config.boards.firstIndex(of: board),
              !config.boards.isEmpty else { throw ControlError.unavailable }
        return config.boards[(index + step + config.boards.count) % config.boards.count]
    }

    public func attention(board: String? = nil, after: NativeIdentity? = nil, now: Double) throws -> FocusIntent {
        if let board, !config.boards.contains(board) { throw ControlError.unavailable }
        let candidates=snapshot.agents.filter {
            $0.attention != .none && $0.fresh(now: now, age: maxAge) && (board == nil || $0.boardId == board)
        }.sorted {
            if $0.attention.rank != $1.attention.rank { return $0.attention.rank < $1.attention.rank }
            if $0.native.provider != $1.native.provider { return $0.native.provider < $1.native.provider }
            return $0.native.sessionId < $1.native.sessionId
        }
        guard !candidates.isEmpty else { throw ControlError.unavailable }
        let next=after.flatMap { identity in candidates.firstIndex(where: { $0.native == identity }) }.map { ($0+1)%candidates.count } ?? 0
        let chosen=candidates[next]
        // Duplicate native identities are contradictory observations, never a safe target.
        return try unique(snapshot.agents.filter { $0.native == chosen.native }, now: now, reason: chosen.attention.rawValue)
    }
}

public struct DisplayedRequest: Equatable {
    public let native: NativeIdentity
    public let surface: Surface
    public let request: PendingRequest
    public let displayedAt: Double

    public init(agent: LiveAgent, now: Double, maxAge: Double = 10) throws {
        guard agent.fresh(now: now, age: maxAge) else { throw ControlError.stale }
        guard agent.canAnswer, let request=agent.request,
              [.approval,.question].contains(request.kind), !request.id.isEmpty,
              request.generation >= 0, !request.choices.isEmpty,
              request.expiresAt.isFinite, request.expiresAt >= now else { throw ControlError.unsupported }
        native=agent.native; surface=agent.surface; self.request=request; displayedAt=now
    }
}
public struct AnswerIntent: Codable, Equatable {
    public let native: NativeIdentity
    public let requestId: String
    public let generation: Int
    public let decision: Decision
}

/// AIDEV-NOTE: blocked status and Enter are not structured approvals.
/// The native adapter must supply fresh request/focus evidence and deduplicate delivery.
public struct AnswerGate {
    private struct Key: Hashable { let native: NativeIdentity; let id: String; let generation: Int }
    private var used=Set<Key>()
    public init() {}

    public mutating func consume(_ displayed: DisplayedRequest, decision: Decision,
                                 snapshot: Snapshot, focused: NativeIdentity?, now: Double,
                                 maxAge: Double = 10) throws -> AnswerIntent {
        guard snapshot.version == 1, maxAge.isFinite, maxAge > 0, now.isFinite,
              now >= displayed.displayedAt, now-displayed.displayedAt <= maxAge else { throw ControlError.stale }
        guard focused == displayed.native else { throw ControlError.wrongFocus }
        let candidates=snapshot.agents.filter { $0.native == displayed.native }
        guard candidates.count == 1, let agent=candidates.first else { throw ControlError.ambiguous }
        guard agent.fresh(now: now, age: maxAge) else { throw ControlError.stale }
        guard agent.canAnswer else { throw ControlError.unsupported }
        guard agent.surface == displayed.surface, agent.request == displayed.request,
              displayed.request.expiresAt >= now, displayed.request.choices.contains(decision)
        else { throw ControlError.changedRequest }
        let key=Key(native:displayed.native,id:displayed.request.id,generation:displayed.request.generation)
        guard used.insert(key).inserted else { throw ControlError.alreadyConsumed }
        return AnswerIntent(native:displayed.native,requestId:displayed.request.id,
                            generation:displayed.request.generation,decision:decision)
    }
}

/// Prospective controller policy; not proof of exclusive hardware ownership.
public struct HardwarePrerequisites {
    public var durableBackupVerified: Bool
    public var firmwareVerified: Bool
    public var supportedHandoffVerified: Bool
    public var otherControllerActive: Bool

    public var mayDrive: Bool {
        durableBackupVerified && firmwareVerified && supportedHandoffVerified && !otherControllerActive
    }
}
