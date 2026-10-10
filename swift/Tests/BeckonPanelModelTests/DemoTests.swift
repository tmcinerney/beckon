import XCTest
@testable import BeckonPanelModel
final class DemoTests:XCTestCase {
    func testReplayStatesDisconnectReorderAndCoordinatorMismatch() async throws {
        let demo=DemoSource(),source=SourceKey(socket:"/demo/socket")
        let native=NativeID(harness:"codex",sessionID:"demo-a")
        let directory=try DemoSource.directorySnapshot(),assignment=try DemoSource.directorySnapshot().assignment(id:"demo-coordinator")
        var statuses:[String]=[]
        for step in 0..<8 {
            if step==4 {
                do {_=try await demo.read(source:source,budget:2);XCTFail("Disconnect omitted")}catch{}
            } else {
                let snapshot=try await demo.read(source:source,budget:2)
                let record=try snapshot.resolve(native,now:Date().timeIntervalSince1970)
                statuses.append(record.agentStatus)
                if step==6 {XCTAssertEqual(snapshot.agents.first?.native?.harness,"claude")}
                if step==7 {XCTAssertThrowsError(try directory.match(assignment,snapshot:snapshot,now:Date().timeIntervalSince1970))}
            }
            await demo.next()
        }
        XCTAssertEqual(statuses,["working","done","blocked","future-state","idle","idle","working"])
    }
    func testOptionsNeverEnableLiveFocusAndRequireAbsolutePaths() throws {
        XCTAssertThrowsError(try PanelOptions(arguments:["--focus"],home:"/user"))
        XCTAssertThrowsError(try PanelOptions(arguments:["--config","relative"],home:"/user"))
        let options=try PanelOptions(arguments:["--demo","--no-save"],home:"/user")
        XCTAssertTrue(options.demo);XCTAssertTrue(options.noSave)
    }
}
