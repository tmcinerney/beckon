import Foundation

public enum PanelError: Error, Equatable, LocalizedError {
    case invalid(String), unavailable(String), cancelled, timeout, outputLimit, cleanupUnconfirmed
    public var errorDescription: String? {
        switch self {
        case .invalid(let reason), .unavailable(let reason): return reason
        case .cancelled: return "Cancelled"
        case .timeout: return "Observation timed out"
        case .outputLimit: return "Observation exceeded its output limit"
        case .cleanupUnconfirmed: return "Process cleanup unconfirmed; metadata reads disabled"
        }
    }
}
private func validIdentifier(_ value: String, limit: Int = 512) -> Bool {
    !value.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        && value.utf8.count <= limit
        && !value.unicodeScalars.contains(where: CharacterSet.controlCharacters.contains)
}
public struct NativeID: Codable, Hashable, Sendable {
    public var harness: String
    public var sessionID: String
    public init(harness: String, sessionID: String) { self.harness = harness; self.sessionID = sessionID }
    public var valid: Bool { validIdentifier(harness, limit: 64) && validIdentifier(sessionID) }
}
public struct SourceKey: Codable, Hashable, Sendable {
    public var socket: String
    public var session: String
    public init(socket: String, session: String = "default") { self.socket = socket; self.session = session }
    public func validate() throws {
        guard session == "default", socket.hasPrefix("/"), !socket.utf8.contains(0),
              socket.utf8.count < 104 else {
            throw PanelError.invalid("Select an absolute existing default-session Unix socket")
        }
    }
}
public struct AgentSession: Codable, Equatable, Sendable {
    public var agent: String
    public var kind: String
    public var source: String?
    public var value: String
}
public struct AgentRecord: Codable, Equatable, Sendable {
    public var agentSession: AgentSession?
    public var terminalID: String?
    public var paneID: String?
    public var tabID: String?
    public var workspaceID: String?
    public var agentStatus: String
    public var name: String?
    public var revision: UInt64?
    public var completionSeq: UInt64?
    enum CodingKeys: String, CodingKey {
        case agentSession = "agent_session", terminalID = "terminal_id", paneID = "pane_id"
        case tabID = "tab_id", workspaceID = "workspace_id", agentStatus = "agent_status"
        case name, revision, completionSeq = "completion_seq"
    }
    public var native: NativeID? {
        guard let s = agentSession, s.kind == "id" else { return nil }
        let id = NativeID(harness: s.agent, sessionID: s.value)
        return id.valid ? id : nil
    }
    public var label: String { name.flatMap { validIdentifier($0) ? $0 : nil } ?? paneID ?? "Unidentified agent" }
    public var presentation: ActivityPresentation { ActivityPresentation(status: agentStatus) }
}
public struct ActivityPresentation: Equatable, Sendable {
    public let label: String
    public let color: String
    public let pulse: Bool
    public init(status: String) {
        pulse = status == "working"
        switch status {
        case "working": label = "Working"; color = "#3BA0FF"
        case "idle": label = "Idle"; color = "#FFFFFF"
        case "done": label = "Herdr-reported done/unread"; color = "#22C55E"
        case "blocked": label = "Blocked / attention"; color = "#FFB020"
        default: label = "Unknown"; color = "#808080"
        }
    }
}
public struct AgentSnapshot: Sendable {
    public var source: SourceKey
    public var observedAt: Double
    public var agents: [AgentRecord]
    private struct Envelope: Decodable {
        struct Result: Decodable { let agents: [AgentRecord] }
        let result: Result
    }
    public static func decode(_ data: Data, source: SourceKey, observedAt: Double) throws -> Self {
        try source.validate()
        guard data.count <= 1_048_576, observedAt.isFinite else { throw PanelError.invalid("Invalid observation") }
        let reply = try JSONDecoder().decode(Envelope.self, from: data)
        return Self(source: source, observedAt: observedAt, agents: reply.result.agents)
    }
    public func resolve(_ native: NativeID, now: Double) throws -> AgentRecord {
        guard now.isFinite, observedAt.isFinite, now >= observedAt, now - observedAt <= 10 else {
            throw PanelError.unavailable("Stale observation")
        }
        let found = agents.filter { $0.native == native }
        guard native.valid, found.count == 1,
              found[0].paneID.map({ validIdentifier($0) }) == true,
              found[0].terminalID.map({ validIdentifier($0) }) == true else {
            throw PanelError.unavailable("Missing, incomplete or ambiguous native identity")
        }
        return found[0]
    }
}
public struct CoordinatorAssignment: Codable, Equatable, Sendable {
    public var boardAlias: String
    public var boardUID: String
    public var ledger: String
    public var coordinatorID: String
    public var acceptedBinding: Data
    public var native: NativeID
    public init(boardAlias: String, boardUID: String, ledger: String,
                coordinatorID: String, acceptedBinding: Data, native: NativeID) {
        self.boardAlias = boardAlias; self.boardUID = boardUID; self.ledger = ledger
        self.coordinatorID = coordinatorID; self.acceptedBinding = acceptedBinding; self.native = native
    }
}
public struct PanelConfiguration: Codable, Equatable, Sendable {
    public var version = 1
    public var source: SourceKey
    public var slots: [NativeID?]
    public var coordinators: [CoordinatorAssignment?]
    public var boards: [BoardSelection] = []
    public var slotLabels: [String]?
    public init(source: SourceKey) {
        self.source = source; slots = Array(repeating: nil, count: 6)
        coordinators = Array(repeating: nil, count: 6)
    }
    public func slotTitle(_ slot: Int) -> String {
        guard let labels = slotLabels, labels.indices.contains(slot) else { return "Slot \(slot + 1)" }
        return "\(slot + 1) · \(labels[slot])"
    }
    public func validate() throws {
        try source.validate()
        guard version == 1, slots.count == 6, coordinators.count == 6 else {
            throw PanelError.invalid("Configuration must contain six slots at version 1")
        }
        if let labels = slotLabels {
            guard labels.count == 6, labels.allSatisfy({ validIdentifier($0, limit: 128) }) else {
                throw PanelError.invalid("Slot labels must contain six nonempty single-line names of at most 128 bytes")
            }
        }
        guard boards.count <= 6 else { throw PanelError.invalid("At most six explicit boards") }
        for board in boards { try board.validate() }
        var ids = Set<NativeID>()
        for i in slots.indices {
            guard slots[i] == nil || coordinators[i] == nil else { throw PanelError.invalid("Conflicting slot assignment") }
            if let c = coordinators[i] {
                guard validIdentifier(c.boardAlias, limit: 128), validIdentifier(c.boardUID),
                      c.ledger.hasPrefix("/"), validIdentifier(c.coordinatorID),
                      c.acceptedBinding.count <= 65_536 else { throw PanelError.invalid("Invalid coordinator reference") }
            }
            if let native = slots[i] ?? coordinators[i]?.native {
                guard native.valid, ids.insert(native).inserted else { throw PanelError.invalid("Invalid or duplicate assignment") }
            }
        }
    }
}
public struct ConfigStore: Sendable {
    public let url: URL
    public init(url: URL) { self.url = url }
    public func load(defaultSource: SourceKey) throws -> PanelConfiguration {
        guard FileManager.default.fileExists(atPath: url.path) else { return PanelConfiguration(source: defaultSource) }
        guard let size = try url.resourceValues(forKeys: [.fileSizeKey]).fileSize, size <= 65_536 else {
            throw PanelError.invalid("Configuration exceeds 64 KiB")
        }
        let config = try JSONDecoder().decode(PanelConfiguration.self, from: Data(contentsOf: url))
        try config.validate()
        return config
    }
    public func save(_ configuration: PanelConfiguration) throws {
        try configuration.validate()
        // AIDEV-NOTE: A malformed existing file is evidence to recover, not permission to replace it.
        if FileManager.default.fileExists(atPath: url.path) { _ = try load(defaultSource: configuration.source) }
        let bytes = try JSONEncoder().encode(configuration)
        guard bytes.count <= 65_536 else { throw PanelError.invalid("Configuration exceeds 64 KiB") }
        try FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
        try bytes.write(to: url, options: .atomic)
    }
}
