import XCTest
import WLKit
@testable import BeckonCreatorMicro
@MainActor final class OneKeyAcceptanceTests:XCTestCase {
    final class Port:OneKeyDevicePort {
        var generation:UInt64=1
        var transportDeadline=60.0,remainingRequests=256
        var budgetClock:()->Double={0}
        var deferred=false,pendingAdmissions:[CheckedContinuation<Void,Never>]=[]
        func allocate(reserving:Int=2) throws {
            guard budgetClock()<transportDeadline,remainingRequests>reserving else {throw InputPilotError.invalid("Fake process budget expired")}
            remainingRequests-=1
        }
        var status:[String:Any]=["version":"0.6.3","profile_index":0,"layer_index":2]
        var records:[OAI.Thread]=[],reads:[String]=[]
        var afterRead:(()->Void)?
        var holdNextPaint=false,held:CheckedContinuation<Void,Never>?
        func connect() throws {}
        func read(_ method:String) async throws -> Any? {
            if deferred {await withCheckedContinuation {pendingAdmissions.append($0)}}
            try allocate();reads.append(method);afterRead?();return status
        }
        func cleanupStatus() async throws -> Any? {try allocate(reserving:0);reads.append("cleanup.status");return status}
        func cleanupPaint() async throws {try allocate(reserving:0);records.append(OneKeyLED.cleanup)}
        func paint(_ record:OAI.Thread) async throws {
            try allocate();records.append(record)
            if holdNextPaint {holdNextPaint=false;await withCheckedContinuation {held=$0}}
        }
        func disconnect() {generation+=1}
    }
    func snapshot(ag:Bool=true) throws -> InputSnapshot {
        var d=try XCTUnwrap(JSONSerialization.jsonObject(with:InputPilotExamples.inputSnapshot()) as? [String:Any])
        var profiles=d["profiles"] as! [[String:Any]],layers=profiles[0]["layers"] as! [[String:Any]]
        let i=try XCTUnwrap(layers.firstIndex(where:{$0["id"] as? Int==0}))
        var layout=layers[i]["layout"] as! [String:Any],keys=layout["keymap"] as! [[String]]
        keys[1][1]=ag ? "KV_OAI_AG03":"KC_NONE";layout["keymap"]=keys;layers[i]["layout"]=layout;profiles[0]["layers"]=layers;d["profiles"]=profiles
        return try InputSnapshot.decode(JSONSerialization.data(withJSONObject:d),deviceID:"33432")
    }
    func render()->OneKeyFrame.Render {.init(key:"f4",state:"blocked",color:"#FFAA00",brightness:1,motion:"steady")}
    func testOnlyVerifiedHerdrCellCanAuthorizeExplicitMode() throws {
        let p=Port();let scope=try OneKeySlotScope.resolve(snapshot:snapshot(),status:p.status)
        XCTAssertEqual(scope.layerIndex,2)
        XCTAssertThrowsError(try OneKeySlotScope.resolve(snapshot:snapshot(ag:false),status:p.status))
        for bad:[String:Any] in [["version":"0.6.3","profile_index":0,"layer_index":0],["version":"0.6.3","profile_index":false,"layer_index":2],["version":"0.6.3","profile_index":0,"layer_index":3],["version":"0.6.2","profile_index":0,"layer_index":2]] {
            XCTAssertThrowsError(try OneKeySlotScope.resolve(snapshot:snapshot(),status:bad))
        }
        XCTAssertThrowsError(try OneKeyAcceptance(port:p,scope:scope,allowed:false,deadline:60,clock:{0}))
        XCTAssertTrue(p.records.isEmpty);XCTAssertTrue(p.reads.isEmpty)
    }
    func testStatusPaintIsOnlyId3LowBrightnessAndCleanupScoped() async throws {
        let p=Port(),scope=try OneKeySlotScope.resolve(snapshot:snapshot(),status:["version":"0.6.3","profile_index":0,"layer_index":2])
        let a=try OneKeyAcceptance(port:p,scope:scope,allowed:true,deadline:60,clock:{0})
        try await a.update {self.render()}
        XCTAssertEqual(p.records.count,1);XCTAssertEqual(p.records[0].id,3);XCTAssertEqual(p.records[0].brightness,0.2)
        XCTAssertEqual(p.records[0].syncKeys,false);XCTAssertEqual(p.records[0].syncAmbient,false)
        await a.stop();XCTAssertTrue(a.cleanupAcknowledged);XCTAssertEqual(p.records.count,2);XCTAssertEqual(p.records[1],OneKeyLED.cleanup)
        do {try await a.update {self.render()};XCTFail("Painted after stop")} catch {}
        XCTAssertEqual(p.records.count,2)
    }
    func testChangedLayerRetirementAndDeadlinePreventWritesOrWrongLayerCleanup() async throws {
        for change in 0..<3 {
            let p=Port();var now=0.0;let scope=try OneKeySlotScope.resolve(snapshot:snapshot(),status:p.status)
            let a=try OneKeyAcceptance(port:p,scope:scope,allowed:true,deadline:60,clock:{now})
            if change==0 {p.status["layer_index"]=0}
            if change==1 {p.afterRead={p.generation+=1}}
            if change==2 {now=60}
            do {try await a.update {self.render()};XCTFail("Accepted invalid session")} catch {}
            await a.stop();XCTAssertTrue(p.records.isEmpty)
        }
        let p=Port(),scope=try OneKeySlotScope.resolve(snapshot:snapshot(),status:["version":"0.6.3","profile_index":0,"layer_index":2])
        let a=try OneKeyAcceptance(port:p,scope:scope,allowed:true,deadline:60,clock:{0});try await a.update {self.render()}
        p.status["layer_index"]=0;await a.stop();XCTAssertEqual(p.records.count,1);XCTAssertFalse(a.cleanupAcknowledged)
    }
    func testEventNeverDispatchesUnfencedFocusAndLatestRenderReadAfterStatus() async throws {
        let p=Port(),scope=try OneKeySlotScope.resolve(snapshot:snapshot(),status:["version":"0.6.3","profile_index":0,"layer_index":2])
        let a=try OneKeyAcceptance(port:p,scope:scope,allowed:true,deadline:60,clock:{0})
        let result=try await a.acceptEvent(.init(sequence:1,connectionGeneration:1,observedAt:0))
        XCTAssertEqual(result,OneKeyAcceptance.focusUnavailable);XCTAssertTrue(p.records.isEmpty)
        var render:OneKeyFrame.Render?=self.render();p.afterRead={render=nil}
        try await a.update {render};XCTAssertEqual(p.records,[OneKeyLED.cleanup])
        do {_ = try await a.acceptEvent(.init(sequence:2,connectionGeneration:2,observedAt:0));XCTFail("Accepted retired event")} catch {}
        await a.stop()
    }
    func testSharedOriginalDeadlineCannotBeExtendedByLateSnapshotOrStatusRead() async throws {
        let p=Port();var now=49.0
        let scope=try OneKeySlotScope.resolve(snapshot:snapshot(),status:p.status)
        let a=try OneKeyAcceptance(port:p,scope:scope,allowed:true,deadline:60-OneKeyAcceptance.cleanupReserve,clock:{now})
        p.afterRead={now=50}
        do {try await a.update {self.render()};XCTFail("Painted beyond original run deadline")} catch {}
        XCTAssertTrue(p.records.isEmpty)
        XCTAssertThrowsError(try OneKeyAcceptance(port:p,scope:scope,allowed:true,deadline:130,clock:{60}))
        await a.stop()
    }

    func testAwaitedShutdownDrainsHeldPaintThenClearsOnlyId3() async throws {
        let p=Port(),scope=try OneKeySlotScope.resolve(snapshot:snapshot(),status:["version":"0.6.3","profile_index":0,"layer_index":2])
        let a=try OneKeyAcceptance(port:p,scope:scope,allowed:true,deadline:60,clock:{0})
        p.holdNextPaint=true
        let paint=Task {try? await a.update {self.render()}}
        for _ in 0..<100 where p.held==nil {await Task.yield()}
        let held=try XCTUnwrap(p.held)
        var stopped=false
        let stop=Task {await a.stop();stopped=true}
        await Task.yield();XCTAssertFalse(stopped);XCTAssertEqual(p.records.count,1)
        held.resume();await paint.value;await stop.value
        XCTAssertTrue(stopped);XCTAssertTrue(a.cleanupAcknowledged)
        XCTAssertEqual(p.records.count,2);XCTAssertEqual(p.records[1],OneKeyLED.cleanup)
    }

    func testImportedReplacementCannotAuthorizeOrChangeVerifiedDeviceScope() throws {
        let p=Port(),verifiedNoAG=try OneKeyVerifiedScope(deviceSnapshot:snapshot(ag:false),generation:1)
        var display:InputSnapshot?=try snapshot(ag:false)
        display=try snapshot(ag:true)
        XCTAssertNotNil(display)
        XCTAssertThrowsError(try verifiedNoAG.resolve(status:p.status,generation:1))
        let verifiedAG=try OneKeyVerifiedScope(deviceSnapshot:snapshot(ag:true),generation:1)
        display=nil
        XCTAssertEqual(try verifiedAG.resolve(status:p.status,generation:1).layerIndex,2)
        XCTAssertThrowsError(try verifiedAG.resolve(status:p.status,generation:2))
    }

    func testNaturalExpiryReservesActualProcessAllocatorForCleanupAndRefusesReuse() async throws {
        let p=Port();var now=0.0;p.budgetClock={now}
        let scope=try OneKeySlotScope.resolve(snapshot:snapshot(),status:p.status)
        let a=try OneKeyAcceptance(port:p,scope:scope,allowed:true,deadline:60,clock:{now})
        try await a.update {self.render()}
        now=50
        do {try await a.update {self.render()};XCTFail("Painted into cleanup reserve")} catch {}
        await a.stop();XCTAssertTrue(a.cleanupAcknowledged);XCTAssertEqual(p.records,[try OneKeyLED.record(render()),OneKeyLED.cleanup])
        now=55
        XCTAssertThrowsError(try OneKeyAcceptance(port:p,scope:scope,allowed:true,deadline:115,clock:{now}))
        XCTAssertEqual(p.transportDeadline,60)
        let scarce=Port();scarce.remainingRequests=2
        XCTAssertThrowsError(try OneKeyAcceptance(port:scarce,scope:scope,allowed:true,deadline:60,clock:{0}))
    }

    func testDeferredConcurrentEventReadsCannotConsumeCleanupReserve() async throws {
        let p=Port();p.remainingRequests=6
        let scope=try OneKeySlotScope.resolve(snapshot:snapshot(),status:p.status)
        let a=try OneKeyAcceptance(port:p,scope:scope,allowed:true,deadline:60,clock:{0})
        try await a.update {self.render()};XCTAssertEqual(p.remainingRequests,4)
        p.deferred=true
        let tasks=(1...3).map {n in Task {try? await a.acceptEvent(.init(sequence:n,connectionGeneration:1,observedAt:0))}}
        for _ in 0..<200 where p.pendingAdmissions.count<3 {await Task.yield()}
        XCTAssertEqual(p.pendingAdmissions.count,3)
        let pending=p.pendingAdmissions;p.pendingAdmissions=[];p.deferred=false
        pending.forEach {$0.resume()}
        for task in tasks {_ = await task.value}
        XCTAssertEqual(p.remainingRequests,2)
        await a.stop();XCTAssertTrue(a.cleanupAcknowledged);XCTAssertEqual(p.remainingRequests,0)
        XCTAssertEqual(p.records.last,OneKeyLED.cleanup)
    }

}
