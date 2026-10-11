import XCTest
@testable import BeckonControl

final class ControlTests: XCTestCase {
    func agent(_ id:String="a", board:String="dev", attention:Attention = .none) -> LiveAgent {
        LiveAgent(native:NativeIdentity(provider:"codex",sessionId:id),boardId:board,role:"coordinator",
                  taskUid:"task-"+id,surface:Surface(kind:"herdr",session:"default",paneId:"w:p"+id,threadId:nil),
                  observedAt:100,runtimeVerified:true,attention:attention,canAnswer:false,request:nil)
    }
    func config(_ a:LiveAgent) -> RoutingConfig {
        RoutingConfig(version:1,bindings:[RoleBinding(key:0,boardId:"dev",role:"coordinator",native:a.native)],boards:["dev","personal"])
    }
    func router(_ agents:[LiveAgent], _ cfg:RoutingConfig?=nil) throws -> Router {
        try Router(snapshot:Snapshot(version:1,agents:agents),config:cfg ?? config(agent()))
    }
    func testRoleIgnoresOrderAndTracksNativePaneMove() throws {
        let a=agent(); let b=agent("b")
        XCTAssertEqual(try router([a,b]).role(key:0,now:101),try router([b,a]).role(key:0,now:101))
        var moved=a; moved.surface.paneId="new-pane"
        XCTAssertEqual(try router([b,moved]).role(key:0,now:101).surface.paneId,"new-pane")
        XCTAssertThrowsError(try router([b]).role(key:0,now:101))
    }
    func testStaleAmbiguousFutureAndUnsupportedSurfacesFail() throws {
        let a=agent()
        XCTAssertThrowsError(try router([a]).role(key:0,now:111))
        XCTAssertThrowsError(try router([a]).role(key:0,now:99))
        XCTAssertThrowsError(try router([a,a]).role(key:0,now:101))
        var x=a; x.surface.kind="window-title"
        XCTAssertThrowsError(try router([x]).role(key:0,now:101))
    }
    func testBoardAndTaskNavigationDoNotUseTabOrder() throws {
        let a=agent(), b=agent("b",board:"personal")
        let r=try router([b,a])
        XCTAssertEqual(try r.board(relative:1,from:"personal"),"dev")
        XCTAssertEqual(try r.board(relative:-1,from:"dev"),"personal")
        XCTAssertThrowsError(try r.board(relative:1,from:"missing"))
        XCTAssertEqual(try r.task("task-a",board:"dev",now:101).native,a.native)
        XCTAssertThrowsError(try r.task("task-a",board:"personal",now:101))
    }
    func testAttentionIsFreshScopedAndDeterministic() throws {
        var a=agent(attention:.blocked), b=agent("b",attention:.question), c=agent("c",board:"personal",attention:.approval)
        a.observedAt=80; b.canAnswer=false; c.request=nil
        let r=try router([a,b,c])
        XCTAssertEqual(try r.attention(now:101).native,c.native)
        XCTAssertEqual(try r.attention(board:"dev",now:101).native,b.native)
        XCTAssertEqual(try r.attention(after:c.native,now:101).native,b.native)
        XCTAssertThrowsError(try DisplayedRequest(agent:b,now:101))
    }
    func requested() -> LiveAgent {
        var a=agent(attention:.approval); a.canAnswer=true
        a.request=PendingRequest(id:"request",generation:1,kind:.approval,choices:[.yes,.no],expiresAt:110)
        return a
    }
    func testYesNoHasExactFocusAndSingleUse() throws {
        let a=requested(), ticket=try DisplayedRequest(agent:a,now:101)
        var gate=AnswerGate(); let snap=Snapshot(version:1,agents:[a])
        XCTAssertThrowsError(try gate.consume(ticket,decision:.yes,snapshot:snap,focused:agent("b").native,now:102))
        XCTAssertEqual(try gate.consume(ticket,decision:.no,snapshot:snap,focused:a.native,now:102).decision,.no)
        XCTAssertThrowsError(try gate.consume(ticket,decision:.yes,snapshot:snap,focused:a.native,now:103)) {
            XCTAssertEqual($0 as? ControlError,.alreadyConsumed)
        }
    }
    func testRequestChangeExpiryAndUnsupportedAnswerFail() throws {
        let a=requested(), ticket=try DisplayedRequest(agent:a,now:101)
        var gate=AnswerGate(), changed=a
        changed.request?.generation=2
        XCTAssertThrowsError(try gate.consume(ticket,decision:.yes,snapshot:Snapshot(version:1,agents:[changed]),focused:a.native,now:102))
        changed=a; changed.canAnswer=false
        XCTAssertThrowsError(try gate.consume(ticket,decision:.yes,snapshot:Snapshot(version:1,agents:[changed]),focused:a.native,now:102))
        XCTAssertThrowsError(try gate.consume(ticket,decision:.yes,snapshot:Snapshot(version:1,agents:[a]),focused:a.native,now:111))
        changed=a; changed.surface.paneId="other"
        XCTAssertThrowsError(try gate.consume(ticket,decision:.yes,snapshot:Snapshot(version:1,agents:[changed]),focused:a.native,now:102))
    }
    func testHardwareRequiresBackupFirmwareHandoffAndNoCompetition() {
        var p=HardwarePrerequisites(durableBackupVerified:true,firmwareVerified:true,supportedHandoffVerified:false,otherControllerActive:false)
        XCTAssertFalse(p.mayDrive); p.supportedHandoffVerified=true; XCTAssertTrue(p.mayDrive)
        p.otherControllerActive=true; XCTAssertFalse(p.mayDrive)
    }
    func testDuplicateConfigurationAndChangedRoleFail() throws {
        let a=agent(); var cfg=config(a); cfg.bindings.append(cfg.bindings[0])
        XCTAssertThrowsError(try router([a],cfg))
        var changed=a; changed.role="other"
        XCTAssertThrowsError(try router([changed]).role(key:0,now:101))
    }
    func testTrellisLocatorIsRecordedNotLiveAndPageIsPreserved() throws {
        let json=Data(#"{"api_version":1,"ok":true,"data":{"compact_version":1,"tasks":[{"task_uid":"task","external_id":"local:task","title":"Work","revision":3,"phase":"claimed","next_action":"Review","claim":{"owner":"worker","run_uid":"run","generation":1,"state":"claimed"},"recovery":{"provider":"codex","session_id":"native","working_directory":"/work","runtime_verified":false}}],"has_more":true,"next_offset":100}}"#.utf8)
        let page=try TrellisReader.decode(json)
        XCTAssertEqual(page.tasks[0].recovery?.sessionId,"native")
        XCTAssertEqual(page.tasks[0].recovery?.runtimeVerified,false)
        XCTAssertTrue(page.hasMore); XCTAssertEqual(page.nextOffset,100)
        let bad=Data(String(decoding:json,as:UTF8.self).replacingOccurrences(of:"runtime_verified\":false",with:"runtime_verified\":true").utf8)
        XCTAssertThrowsError(try TrellisReader.decode(bad))
    }

    func testConflictingNativeRecordsAndInvalidIdentityFail() throws {
        let a=agent(); var duplicate=a; duplicate.boardId="personal"; duplicate.role="other"
        XCTAssertThrowsError(try router([a,duplicate]).role(key:0,now:101))
        var bad=a; bad.native.sessionId=" "
        XCTAssertThrowsError(try router([bad],config(bad)))
    }
    func testChiefBindingIsGlobalAndSingleton() throws {
        var chief=agent(); chief.role="chief-of-staff"; chief.boardId="personal"
        let binding=RoleBinding(key:0,boardId:nil,role:"chief-of-staff",native:chief.native)
        var cfg=RoutingConfig(version:1,bindings:[binding],boards:["dev","personal"])
        XCTAssertEqual(try router([chief],cfg).role(key:0,now:101).native,chief.native)
        var second=binding; second.key=1; cfg.bindings.append(second)
        XCTAssertThrowsError(try router([chief],cfg))
    }
}
