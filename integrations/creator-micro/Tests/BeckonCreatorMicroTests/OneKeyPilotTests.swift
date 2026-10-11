import XCTest
import WLKit
@testable import BeckonCreatorMicro
@MainActor final class OneKeyPilotTests:XCTestCase {
    func target() throws -> OneKeyTarget {try JSONDecoder().decode(OneKeyTarget.self,from:Data("{\"session\":\"test\",\"socket\":\"/herdr.sock\",\"pane_id\":\"p1\",\"terminal_id\":\"t1\"}".utf8))}
    func frame(age:Int=0,epoch:String="one",generation:Int=1,refresh:Int=1) throws -> OneKeyFrame {
        try OneKeyFrame.decode(JSONSerialization.data(withJSONObject:["type":"ok","data":["version":"beckon.pilot.v1","epoch":epoch,"available":true,"binding":["key":"f4","session":"test","pane_id":"p1"],"source":["session":"test","socket":"/herdr.sock","generation":generation,"refresh":refresh,"age_ms":age],"terminal_id":"t1","raw_status":"unknown","render":["key":"f4","state":"unknown","color":"#808080","brightness":0.5,"motion":"steady"]]]))
    }
    func testMonotonicExpiryAndExplicitReselectionOnEpochChange() throws {
        var observation=OneKeyObservation()
        try observation.accept(frame(age:9000),target:target(),now:20,requestStartedAt:20)
        XCTAssertNotNil(observation.current(now:20.9));XCTAssertNil(observation.current(now:21))
        XCTAssertThrowsError(try observation.accept(frame(epoch:"two"),target:target(),now:22,requestStartedAt:22))
        XCTAssertTrue(observation.requiresReselection)
        XCTAssertThrowsError(try observation.accept(frame(),target:target(),now:23,requestStartedAt:23))
        observation.select();try observation.accept(frame(epoch:"two"),target:target(),now:24,requestStartedAt:24)
        XCTAssertNotNil(observation.current(now:24))
    }
    func testAG03IsUnattributedExactAndDebouncedAcrossLayerRaceAndReconnect() throws {
        var observer=AG03Observer();observer.connection(1)
        XCTAssertNil(observer.receive(method:"v.oai.hid",params:["k":"AG3","act":1],generation:1,now:0))
        XCTAssertNil(observer.receive(method:"v.oai.hid",params:["k":"AG03","act":true],generation:1,now:0))
        let receipt=observer.receive(method:"v.oai.hid",params:["k":"AG03","act":1,"ag":9],generation:1,now:1)
        // Polling Herdr then switching ChatGPT cannot confer event-time layer attribution.
        XCTAssertEqual(receipt?.attribution,"UN-ATTRIBUTED")
        XCTAssertNil(observer.receive(method:"v.oai.hid",params:["k":"AG03","act":1],generation:1,now:1.1))
        observer.connection(2)
        XCTAssertNil(observer.receive(method:"v.oai.hid",params:["k":"AG03","act":0],generation:1,now:2))
        XCTAssertNil(observer.receive(method:"v.oai.hid",params:["k":"AG03","act":1],generation:2,now:2))
        XCTAssertNil(observer.receive(method:"v.oai.hid",params:["k":"AG03","act":0],generation:2,now:3))
        XCTAssertNotNil(observer.receive(method:"v.oai.hid",params:["k":"AG03","act":1],generation:2,now:4))
    }
    func testLiveFlagsCannotMixAutostartDemoOrMissingIdentity() throws {
        XCTAssertNil(try OneKeyLaunch.parse(["--input-pilot"]))
        XCTAssertThrowsError(try OneKeyLaunch.parse(["--input-one-key-live","--demo"]))
        XCTAssertThrowsError(try OneKeyLaunch.parse(["--input-one-key-live"]))
    }
    func testLiveLaunchRequiresExactProductAndSelectsCreatorMicro2() throws {
        let expected=String(data:try JSONEncoder().encode(target()),encoding:.utf8)!
        let args=["--input-one-key-live","--backend-socket","/backend.sock","--device-serial","1020BA73CBCC","--profile-id","2","--layer-id","0","--expected-target",expected]
        XCTAssertThrowsError(try OneKeyLaunch.parse(args))
        let launch=try XCTUnwrap(OneKeyLaunch.parse(args+["--device-product-id","0x8298"]))
        XCTAssertEqual(launch.selector.productID,0x8298)
        let actual=PilotDeviceSelector.Candidate(vendorID:0x303a,productID:0x8298,usagePage:0xff00,usage:1,serial:"1020BA73CBCC",transport:"USB")
        let other=PilotDeviceSelector.Candidate(vendorID:0x303a,productID:0x8297,usagePage:0xff00,usage:1,serial:"1020BA73CBCC",transport:"USB")
        XCTAssertEqual(try launch.selector.index(in:[other,actual]),1)
        XCTAssertThrowsError(try launch.selector.index(in:[other]))
        XCTAssertThrowsError(try OneKeyLaunch.parse(args+["--device-product-id","0x8299"]))
        XCTAssertThrowsError(try OneKeyLaunch.parse(args+["--device-product-id","33432junk"]))
    }
    func testOneRecordOnlyAndUnsupportedMotionFallsBackWithoutZoneSync() throws {
        let f=try frame();let record=try OneKeyLED.record(try XCTUnwrap(f.render))
        let params=OAI.threadsParams([record])
        XCTAssertEqual(params.count,1);XCTAssertEqual(params[0]["id"] as? Int,3)
        XCTAssertEqual(params[0]["b"] as? Double,0.2);XCTAssertEqual(params[0]["sk"] as? Int,0);XCTAssertEqual(params[0]["sa"] as? Int,0)
    }
    final class Port:OneKeyDevicePort {
        var generation:UInt64=1
        var records:[OAI.Thread]=[],disconnected=false
        var hold=false,waiting:CheckedContinuation<Void,Never>?
        func connect() throws {}
        func read(_ method:String) async throws -> Any? {nil}
        func paint(_ record:OAI.Thread) async throws {
            records.append(record)
            if hold {hold=false;await withCheckedContinuation {waiting=$0}}
        }
        func disconnect() {disconnected=true;generation+=1;waiting?.resume();waiting=nil}
    }
    func lease()->OneKeyLease {.init(generation:1,profileID:2,layerID:0,layerIndex:3,firmware:"0.6.3",keyCode:"KV_OAI_AG03")}
    func proof(merge:Bool=true,layer:Bool=true,cleanup:Bool=true,owner:Bool=true,drain:Bool=true)->OneKeyFirmwareProof {
        .init(omittedIDsPreserved:merge,layerScoped:layer,cleanupRestoresOwner:cleanup,coexistenceVerified:owner,wireDrained:drain)
    }
    func testUnknownReplacementGlobalBlackOverlayAndUnprovenRestartNeverArm() async throws {
        let p=Port(),l=lease(),writer=OneKeyWriter(port:p,safe:{l})
        // Fake firmware hypotheses: clearing a sentinel, leaking across layers or retaining
        // a black overlay fails its respective gate. An emulator ACK proves none of these.
        for bad in [nil,proof(merge:false),proof(layer:false),proof(cleanup:false),proof(owner:false),proof(drain:false)] {
            XCTAssertThrowsError(try writer.arm(l,proof:bad,allowed:true))
            await writer.update(try OneKeyLED.record(XCTUnwrap(frame().render)))
            XCTAssertTrue(p.records.isEmpty)
        }
        XCTAssertThrowsError(try writer.arm(l,proof:proof(),allowed:false))
    }
    func testStopRetiresLatestPaintAndAcknowledgementDoesNotProveCleanup() async throws {
        let p=Port(),l=lease(),writer=OneKeyWriter(port:p,safe:{l})
        try writer.arm(l,proof:proof(),allowed:true)
        p.hold=true
        let record=try OneKeyLED.record(XCTUnwrap(frame().render))
        let paint=Task {await writer.update(record)}
        while p.waiting==nil {await Task.yield()}
        var latest=record;latest.color=0x123456;await writer.update(latest)
        let stop=Task {await writer.stop()};await Task.yield()
        p.waiting?.resume();p.waiting=nil
        await paint.value;await stop.value
        XCTAssertEqual(p.records.count,2);XCTAssertEqual(p.records[1],OneKeyLED.cleanup)
        XCTAssertTrue(writer.cleanupAcknowledged);XCTAssertFalse(writer.cleanupConfirmed)
        XCTAssertTrue(p.disconnected)
    }
    func testRevokedLayerOwnershipDisconnectsWithoutForeignCleanup() async throws {
        let p=Port();var active:OneKeyLease?=lease();let writer=OneKeyWriter(port:p,safe:{active})
        try writer.arm(lease(),proof:proof(),allowed:true)
        active=nil;await writer.stop()
        XCTAssertTrue(p.records.isEmpty);XCTAssertTrue(p.disconnected);XCTAssertFalse(writer.cleanupConfirmed)
    }
    func testRepeatedSameRefreshCannotRenewOldObservationAndOldRefreshFails() throws {
        var observation=OneKeyObservation();try observation.accept(frame(age:9000),target:target(),now:20,requestStartedAt:20)
        try observation.accept(frame(age:9000),target:target(),now:20.9,requestStartedAt:20.9)
        XCTAssertNil(observation.current(now:21))
        try observation.accept(frame(refresh:3),target:target(),now:22,requestStartedAt:22)
        XCTAssertThrowsError(try observation.accept(frame(refresh:2),target:target(),now:23,requestStartedAt:23))
        XCTAssertNil(observation.current(now:23))
    }

    func testResponseTransitCountsAgainstSourceFreshness() throws {
        var observation=OneKeyObservation()
        XCTAssertThrowsError(try observation.accept(frame(age:9000),target:target(),now:20,requestStartedAt:18.5))
        XCTAssertNil(observation.current(now:20))
        try observation.accept(frame(age:9000),target:target(),now:20.5,requestStartedAt:20)
        XCTAssertNotNil(observation.current(now:20.9))
        XCTAssertNil(observation.current(now:21))
    }

}
