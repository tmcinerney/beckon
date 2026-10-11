import XCTest
@testable import BeckonPanelModel
final class CoordinatorTests: XCTestCase {
    let board = BoardSelection(executable:"/bin/trellis",config:"/pin/config",registry:"/pin/boards",boardConfig:"/pin/board",alias:"development",uid:"board-id",ledger:"/pin/ledger")
    let paths = Data(#"{"ok":true,"data":{"config":"/pin/config","ledger":"/pin/ledger"}}"#.utf8)
    let context = Data(#"{"ok":true,"data":{"board":{"id":"board-id"},"selection":{"name":"development","registry":"/pin/boards","board_config":"/pin/board","db":"/pin/ledger"}}}"#.utf8)
    let agents = Data(#"{"ok":true,"data":{"registered_coordinators":[{"id":"chief","role":"Chief","link":{"provider":"herdr","stable_id":"session-a","target":{"session":"default","agent":"a"},"identity":{"agent_session":{"agent":"codex","kind":"id","source":"herdr:codex","value":"session-a"},"terminal_id":"t-a","pane_id":"p-a"}}}]}}"#.utf8)
    func testPinnedBoardAndFullBindingRejectReplacementAndPaneMoves() throws {
        let directory = try CoordinatorSnapshot.decode(paths:paths,context:context,agents:agents,board:board)
        let assignment = try directory.assignment(id:"chief")
        let fixtures=ModelTests()
        let snapshot=try AgentSnapshot.decode(fixtures.wire(fixtures.a),source:fixtures.source,observedAt:10)
        XCTAssertEqual(try directory.match(assignment,snapshot:snapshot,now:11).paneID,"p-a")
        var moved=snapshot; moved.agents[0].paneID="replacement"
        XCTAssertEqual(try moved.resolve(assignment.native,now:11).paneID,"replacement")
        XCTAssertThrowsError(try directory.match(assignment,snapshot:moved,now:11))
        var changed=directory
        changed.coordinators[0].link?.target.session="different"
        XCTAssertThrowsError(try changed.match(assignment,snapshot:snapshot,now:11))
        var wrongBoard=board; wrongBoard.uid="replacement"
        XCTAssertThrowsError(try CoordinatorSnapshot.decode(paths:paths,context:context,agents:agents,board:wrongBoard))
        var wrongSource=snapshot; wrongSource.source.session="other"
        XCTAssertThrowsError(try directory.match(assignment,snapshot:wrongSource,now:11))
    }
    func testMetadataAllowlistAndNoProviderExecution() async throws {
        XCTAssertThrowsError(try board.arguments(command:"agent inspect"))
        let args=try board.arguments(command:"board-agents")
        XCTAssertEqual(args.last,"board-agents")
        XCTAssertTrue(args.contains("--boards-config"))
        let runner=FixtureRunner(replies:[paths,context,agents])
        let result=try await CoordinatorDirectory(runner:runner).read(board:board)
        XCTAssertEqual(result.coordinators.count,1)
        let commands=await runner.commands
        XCTAssertEqual(commands.map { $0.last! },["paths","board-context","board-agents"])
    }
    func testOuterBudgetAcrossSlowCommandsDoesNotReset() async {
        let runner=FixtureRunner(replies:[paths,context,agents],delay:0.3)
        do {
            _=try await CoordinatorDirectory(runner:runner).read(board:board,budget:1.2)
            XCTFail("Budget restarted")
        } catch {XCTAssertEqual(error as? PanelError,.timeout)}
        let commands=await runner.commands
        XCTAssertEqual(commands.count,1)
    }
    func testOuterBudgetCannotRestartPerCommand() async {
        let runner=FixtureRunner(replies:[paths,context,agents],delay:0.2)
        do {
            _=try await CoordinatorDirectory(runner:runner).read(board:board,budget:0.15)
            XCTFail("Outer deadline ignored")
        } catch {}
        let commands=await runner.commands
        XCTAssertTrue(commands.isEmpty)
    }
}
actor FixtureRunner: MetadataCommandRunner {
    var replies:[Data]; var commands:[[String]]=[]; let delay:Double
    init(replies:[Data],delay:Double=0){self.replies=replies;self.delay=delay}
    func run(executable:String,arguments:[String],budget:Double) async throws -> Data {
        commands.append(arguments)
        if delay>0 {try await Task.sleep(nanoseconds:UInt64(delay*1e9))}
        return replies.removeFirst()
    }
}
