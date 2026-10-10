import Foundation

public struct BoardSelection: Codable, Equatable, Sendable {
    public var executable: String
    public var config: String
    public var registry: String
    public var boardConfig: String
    public var alias: String
    public var uid: String
    public var ledger: String
    public init(executable: String, config: String, registry: String, boardConfig: String, alias: String, uid: String, ledger: String) {
        self.executable=executable; self.config=config; self.registry=registry; self.boardConfig=boardConfig
        self.alias=alias; self.uid=uid; self.ledger=ledger
    }
    public func validate() throws {
        guard [executable,config,registry,boardConfig,ledger].allSatisfy({
            $0.hasPrefix("/") && !$0.utf8.contains(0) && $0.utf8.count < 4096
        }), !alias.isEmpty, alias.utf8.count <= 128, !uid.isEmpty, uid.utf8.count <= 512,
        !alias.unicodeScalars.contains(where:CharacterSet.controlCharacters.contains),
        !uid.unicodeScalars.contains(where:CharacterSet.controlCharacters.contains) else {
            throw PanelError.invalid("Select explicit absolute Trellis paths and an existing board")
        }
    }
    public func arguments(command: String) throws -> [String] {
        try validate()
        guard ["paths","board-context","board-agents"].contains(command) else {
            throw PanelError.invalid("Only local board metadata reads are supported")
        }
        return ["--config",config,"--boards-config",registry,"--board",alias,"--board-config",boardConfig,command]
    }
}
public struct ProviderBinding: Codable, Equatable, Sendable {
    public struct Target: Codable, Equatable, Sendable { public var session: String; public var agent: String }
    public struct Identity: Codable, Equatable, Sendable {
        public var agentSession: AgentSession
        public var terminalID: String
        public var paneID: String
        enum CodingKeys: String, CodingKey { case agentSession="agent_session",terminalID="terminal_id",paneID="pane_id" }
    }
    public var provider: String
    public var stableID: String
    public var target: Target
    public var identity: Identity
    enum CodingKeys: String, CodingKey { case provider,stableID="stable_id",target,identity }
    public var native: NativeID {
        NativeID(harness:identity.agentSession.agent,sessionID:identity.agentSession.value)
    }
    public func validate() throws {
        guard provider=="herdr",target.session=="default",!target.agent.isEmpty,
              identity.agentSession.kind=="id",native.valid,stableID==native.sessionID,
              !identity.terminalID.isEmpty,!identity.paneID.isEmpty else {
            throw PanelError.unavailable("Unsupported or incomplete coordinator binding")
        }
    }
}
public struct RegisteredCoordinator: Decodable, Sendable {
    public var id: String
    public var role: String
    public var link: ProviderBinding?
    public var linkStatus: String?
    enum CodingKeys:String,CodingKey {case id,role,link}
    private struct Status:Decodable {var status:String?}
    public init(from decoder:Decoder) throws {
        let c=try decoder.container(keyedBy:CodingKeys.self)
        id=try c.decode(String.self,forKey:.id);role=try c.decode(String.self,forKey:.role)
        // An unsupported provider remains registered, with no usable Herdr binding.
        link=try? c.decode(ProviderBinding.self,forKey:.link)
        linkStatus=(try? c.decode(Status.self,forKey:.link))?.status
    }
}
public struct CoordinatorSnapshot: Sendable {
    public var board: BoardSelection
    public var coordinators: [RegisteredCoordinator]
    private struct Paths:Decodable {struct Body:Decodable {var config:String;var ledger:String};var ok:Bool;var data:Body}
    private struct Context:Decodable {
        struct Body:Decodable {
            struct Board:Decodable {var id:String}
            struct Selection:Decodable {var name:String;var registry:String;var board_config:String;var db:String}
            var board:Board;var selection:Selection
        }
        var ok:Bool;var data:Body
    }
    private struct Agents:Decodable {struct Body:Decodable {var registered_coordinators:[RegisteredCoordinator]};var ok:Bool;var data:Body}
    public static func decode(paths: Data, context: Data, agents: Data, board: BoardSelection) throws -> Self {
        try board.validate()
        guard [paths,context,agents].allSatisfy({$0.count<=1_048_576}) else {throw PanelError.outputLimit}
        let decoder=JSONDecoder()
        let p=try decoder.decode(Paths.self,from:paths), c=try decoder.decode(Context.self,from:context)
        let a=try decoder.decode(Agents.self,from:agents)
        guard p.ok,c.ok,a.ok,p.data.config==board.config,p.data.ledger==board.ledger,
              c.data.board.id==board.uid,c.data.selection.name==board.alias,
              c.data.selection.registry==board.registry,c.data.selection.board_config==board.boardConfig,
              c.data.selection.db==board.ledger else {
            throw PanelError.unavailable("Board alias, UID or selected paths changed; reselect explicitly")
        }
        return Self(board:board,coordinators:a.data.registered_coordinators)
    }
    public func assignment(id: String) throws -> CoordinatorAssignment {
        let candidates=coordinators.filter {$0.id==id}
        guard candidates.count==1,let link=candidates[0].link,
              !["stopped","unavailable","offline"].contains(candidates[0].linkStatus ?? "") else {
            throw PanelError.unavailable("Coordinator unlinked, stopped or ambiguous")
        }
        try link.validate()
        return CoordinatorAssignment(boardAlias:board.alias,boardUID:board.uid,ledger:board.ledger,
            coordinatorID:id,acceptedBinding:try JSONEncoder().encode(link),native:link.native)
    }
    public func match(_ assignment: CoordinatorAssignment, snapshot: AgentSnapshot, now: Double) throws -> AgentRecord {
        try snapshot.source.validate()
        guard assignment.boardUID==board.uid,assignment.boardAlias==board.alias,assignment.ledger==board.ledger else {
            throw PanelError.unavailable("Coordinator board changed")
        }
        let current=try self.assignment(id:assignment.coordinatorID)
        let accepted=try JSONDecoder().decode(ProviderBinding.self,from:assignment.acceptedBinding)
        let actual=try JSONDecoder().decode(ProviderBinding.self,from:current.acceptedBinding)
        guard accepted==actual,accepted.native==assignment.native else {
            throw PanelError.unavailable("Coordinator binding changed; explicitly reselect")
        }
        let record=try snapshot.resolve(assignment.native,now:now)
        guard record.agentSession==accepted.identity.agentSession,
              record.terminalID==accepted.identity.terminalID,record.paneID==accepted.identity.paneID else {
            throw PanelError.unavailable("Registered coordinator surface no longer matches")
        }
        return record
    }
}
public protocol MetadataCommandRunner: Sendable {
    var hasUnconfirmedCleanup:Bool {get async}
    func reconcileCleanup() async throws
    func run(executable: String, arguments: [String], budget: Double) async throws -> Data
}
extension MetadataCommandRunner {
    public var hasUnconfirmedCleanup:Bool {get async {false}}
    public func reconcileCleanup() async throws {throw PanelError.unavailable("Cleanup reconciliation unsupported")}
}
extension ProcessRunner: MetadataCommandRunner {}
public struct CoordinatorDirectory: Sendable {
    public let runner: any MetadataCommandRunner
    public init(runner: any MetadataCommandRunner = ProcessRunner()) { self.runner=runner }
    public func read(board: BoardSelection, budget: Double = 8) async throws -> CoordinatorSnapshot {
        try board.validate()
        guard budget.isFinite,budget>=1,budget<=8 else {throw PanelError.timeout}
        let deadline=ProcessInfo.processInfo.systemUptime+budget
        var replies:[Data]=[]
        for command in ["paths","board-context","board-agents"] {
            try Task.checkCancellation()
            let remaining=deadline-ProcessInfo.processInfo.systemUptime
            guard remaining>1 else {throw PanelError.timeout}
            let reply=try await runner.run(executable:board.executable,arguments:board.arguments(command:command),budget:min(3,remaining))
            guard ProcessInfo.processInfo.systemUptime<deadline else {throw PanelError.timeout}
            replies.append(reply)
        }
        return try CoordinatorSnapshot.decode(paths:replies[0],context:replies[1],agents:replies[2],board:board)
    }
}
