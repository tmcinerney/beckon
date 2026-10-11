import XCTest
import CryptoKit
import WLKit
@testable import BeckonCreatorMicro
@MainActor final class OneKeyFileReaderTests:XCTestCase {
    func digest(_ data:Data)->String {Insecure.SHA1.hash(data:data).map {String(format:"%02x",$0)}.joined()}
    func listing(_ data:Data)->[[String:Any]] {[["name":"keymap.json","size":String(data.count),"checksum":digest(data)]]}
    func testUsesDemonstratedSmallChunkContractAndChecksCompleteFile() async throws {
        let data=Data(repeating:65,count:4000);var calls:[String]=[]
        let result=try await OneKeyFileReader.read(deadline:60,clock:{0},valid:{true}) {method,params in
            calls.append(method)
            if method=="fs.list" {XCTAssertEqual(params["checksum"] as? Bool,true);return self.listing(data)}
            XCTAssertEqual(method,"fs.readbin");XCTAssertEqual(params["file"] as? String,"keymap.json")
            let offset=try XCTUnwrap(params["offset"] as? Int),length=try XCTUnwrap(params["len"] as? Int)
            XCTAssertEqual(length,min(192,data.count-offset))
            return ["total_size":data.count,"data":data.subdata(in:offset..<(offset+length)).base64EncodedString()]
        }
        XCTAssertEqual(result.bytes,data);XCTAssertEqual(result.chunks,21)
        XCTAssertEqual(calls,["fs.list"]+Array(repeating:"fs.readbin",count:21)+["fs.list"])
    }
    func testRejectsNullEmptyOversizeChangingTotalInvalidBase64AndCorruption() async throws {
        let data=Data("{}".utf8)
        let bad:[Any]=[NSNull(),["total_size":2,"data":""],["total_size":262145,"data":"e30="],["total_size":3,"data":"e30="],["total_size":2,"data":"not-base64"],["total_size":2,"data":"e30=\n"],["total_size":true,"data":"e30="],["total_size":2,"data":Data("[]".utf8).base64EncodedString()]]
        for raw in bad {
            do {_ = try await OneKeyFileReader.read(deadline:60,clock:{0},valid:{true}) {method,_ in method=="fs.list" ? self.listing(data):raw};XCTFail("Accepted malformed or corrupted file")}
            catch {XCTAssertFalse(error.localizedDescription.contains("not-base64"))}
        }
    }
    func testListingMustHaveUniqueExactFileBoundedSizeAndStableChecksum() async throws {
        let data=Data("{}".utf8)
        for list:Any in [[],[listing(data)[0],listing(data)[0]],[["name":"other.json","size":"2","checksum":digest(data)]],[["name":"keymap.json","size":"262145","checksum":digest(data)]]] {
            var reads=0
            do {_ = try await OneKeyFileReader.read(deadline:60,clock:{0},valid:{true}) {method,_ in if method != "fs.list" {reads+=1};return list};XCTFail("Accepted bad listing")}
            catch {XCTAssertEqual(reads,0)}
        }
        var lists=0
        do {_ = try await OneKeyFileReader.read(deadline:60,clock:{0},valid:{true}) {method,_ in
            if method=="fs.list" {lists+=1;return self.listing(lists==1 ? data:Data("[]".utf8))}
            return ["total_size":2,"data":data.base64EncodedString()]
        };XCTFail("Accepted changed file listing")} catch {XCTAssertEqual(lists,2)}
    }
    func testSlowFileTransferDoesNotRefreshEarlierLayerObservation() async throws {
        let target="{\"session\":\"test\",\"socket\":\"/herdr.sock\",\"pane_id\":\"p1\",\"terminal_id\":\"t1\"}"
        let launch=try XCTUnwrap(OneKeyLaunch.parse(["--input-one-key-live","--backend-socket","/backend.sock","--device-serial","exact","--device-product-id","0x8298","--profile-id","2","--layer-id","0","--expected-target",target]))
        let model=OneKeyLiveModel(launch:launch);var now=10.0
        model.recordLayerStatus(["layer_index":2],observedAt:now)
        let data=Data("{}".utf8)
        _ = try await OneKeyFileReader.read(deadline:60,clock:{now},valid:{true}) {method,_ in
            if method=="fs.list" {return self.listing(data)}
            now+=20;return ["total_size":2,"data":data.base64EncodedString()]
        }
        XCTAssertEqual(model.layerIndex,2)
        XCTAssertEqual(now-(try XCTUnwrap(model.layerObservedAt)),20)
    }
    func testRetirementDeadlineAndCancellationPreventFollowupReads() async throws {
        let data=Data("{}".utf8);var valid=true,calls=0
        do {_ = try await OneKeyFileReader.read(deadline:60,clock:{0},valid:{valid}) {_,_ in calls+=1;valid=false;return self.listing(data)};XCTFail("Accepted retired connection")}
        catch {XCTAssertEqual(calls,1)}
        do {_ = try await OneKeyFileReader.read(deadline:0,clock:{0},valid:{true}) {_,_ in calls+=1;return nil};XCTFail("Accepted deadline")}
        catch {XCTAssertEqual(calls,1)}
        let task=Task {try await OneKeyFileReader.read(deadline:60,clock:{0},valid:{true}) {_,_ in calls+=1;throw CancellationError()}}
        task.cancel();do {_ = try await task.value;XCTFail("Accepted cancellation")} catch {XCTAssertEqual(calls,1)}
    }
    func testRPCFailureIncludesExactFailedOffsetLengthAndCompletedChunksWithoutRetry() async throws {
        let data=Data(repeating:65,count:4000);var reads=0
        do {_ = try await OneKeyFileReader.read(deadline:60,clock:{0},valid:{true}) {method,params in
            if method=="fs.list" {return self.listing(data)}
            reads+=1
            if reads==2 {throw WLDevice.Failure.rpc("fs.readbin","json pool overflow",-32000)}
            return ["total_size":4000,"data":data.prefix(192).base64EncodedString()]
        };XCTFail("Expected device error")}
        catch {
            for text in ["offset=192","requested=192","completedChunks=1","rpcCode=-32000"] {XCTAssertTrue(error.localizedDescription.contains(text))}
            XCTAssertEqual(reads,2)
        }
    }
    func testExperimentalProbeIsOneBoundedPrefixNotFullSnapshotOrFallback() async throws {
        let data=Data(repeating:65,count:4000);var methods:[String]=[]
        let result=try await OneKeyFileReader.probe(deadline:60,clock:{0},valid:{true}) {method,params in
            methods.append(method)
            if method=="fs.list" {return self.listing(data)}
            XCTAssertEqual(params["offset"] as? Int,0);XCTAssertEqual(params["len"] as? Int,192)
            return ["total_size":4000,"data":data.prefix(192).base64EncodedString()]
        }
        XCTAssertEqual(methods,["fs.list","fs.readbin","fs.list"]);XCTAssertEqual(result.received,192);XCTAssertEqual(result.total,4000)
        var calls=0
        do {_ = try await OneKeyFileReader.probe(deadline:60,clock:{0},valid:{true}) {method,_ in
            calls+=1;if method=="fs.list" {return self.listing(data)}
            throw WLDevice.Failure.rpc("fs.readbin","json pool overflow",-32000)
        };XCTFail("Expected failed probe")}
        catch {XCTAssertEqual(calls,2);XCTAssertTrue(error.localizedDescription.contains("completedChunks=0"))}
    }

    func testLongDeviceMessageCannotHideRPCCodeAndProbeRejectsMalformedRetiredChangedReplies() async throws {
        let data=Data(repeating:65,count:4000)
        for code:Int? in [-32000,nil] {
            do {_ = try await OneKeyFileReader.probe(deadline:60,clock:{0},valid:{true}) {method,_ in
                if method=="fs.list" {return self.listing(data)}
                throw WLDevice.Failure.rpc("fs.readbin",String(repeating:"x",count:700),code)
            };XCTFail("Expected error")}
            catch {XCTAssertTrue(error.localizedDescription.contains("rpcCode=\(code.map(String.init) ?? "unavailable")"));XCTAssertLessThan(error.localizedDescription.count,650)}
        }
        for bad:Any in [NSNull(),["total_size":4000,"data":""],["total_size":4001,"data":data.prefix(192).base64EncodedString()],["total_size":4000,"data":data.prefix(193).base64EncodedString()]] {
            var calls=0
            do {_ = try await OneKeyFileReader.probe(deadline:60,clock:{0},valid:{true}) {method,_ in calls+=1;return method=="fs.list" ? self.listing(data):bad};XCTFail("Accepted malformed probe")}
            catch {XCTAssertEqual(calls,2)}
        }
        var valid=true,calls=0
        do {_ = try await OneKeyFileReader.probe(deadline:60,clock:{0},valid:{valid}) {_,_ in calls+=1;valid=false;return self.listing(data)};XCTFail("Accepted retirement")}
        catch {XCTAssertEqual(calls,1)}
        do {_ = try await OneKeyFileReader.probe(deadline:0,clock:{0},valid:{true}) {_,_ in calls+=1;return nil};XCTFail("Accepted expired probe")}
        catch {XCTAssertEqual(calls,1)}
        var lists=0
        do {_ = try await OneKeyFileReader.probe(deadline:60,clock:{0},valid:{true}) {method,_ in
            if method=="fs.list" {lists+=1;return self.listing(lists==1 ? data:Data("[]".utf8))}
            return ["total_size":4000,"data":data.prefix(192).base64EncodedString()]
        };XCTFail("Accepted changed probe listing")}
        catch {XCTAssertEqual(lists,2)}
    }

    func testActual13298ByteShapeSeventyChunksFinalFiftyAndSchema() async throws {
        let original=try InputPilotExamples.inputSnapshot()
        XCTAssertLessThan(original.count,13298)
        let data=original+Data(repeating:32,count:13298-original.count)
        var offsets:[Int]=[],lengths:[Int]=[],lists=0
        let result=try await OneKeyFileReader.read(deadline:60,clock:{0},valid:{true}) {method,params in
            if method=="fs.list" {lists+=1;return self.listing(data)}
            let offset=try XCTUnwrap(params["offset"] as? Int),length=try XCTUnwrap(params["len"] as? Int)
            offsets.append(offset);lengths.append(length)
            return ["total_size":13298,"data":data.subdata(in:offset..<(offset+length)).base64EncodedString()]
        }
        XCTAssertEqual(result.bytes,data);XCTAssertEqual(result.chunks,70);XCTAssertEqual(lists,2)
        XCTAssertEqual(offsets,Array(stride(from:0,to:13298,by:192)))
        XCTAssertEqual(lengths,Array(repeating:192,count:69)+[50])
        try OneKeyUSBReply.validateJSON(String(decoding:result.bytes,as:UTF8.self))
        let snapshot=try InputSnapshot.decode(result.bytes,deviceID:"33432")
        XCTAssertEqual(snapshot.profiles[0].id,2)
    }
    func testFinalChunkCorruptionRejectedAndOversizeBudgetFailsBeforeRead() async throws {
        let data=Data(repeating:65,count:13298);var reads=0
        do {_ = try await OneKeyFileReader.read(deadline:60,clock:{0},valid:{true}) {method,params in
            if method=="fs.list" {return self.listing(data)}
            reads+=1
            let offset=try XCTUnwrap(params["offset"] as? Int),length=try XCTUnwrap(params["len"] as? Int)
            var bytes=data.subdata(in:offset..<(offset+length));if offset==13248 {bytes[0]=66}
            return ["total_size":13298,"data":bytes.base64EncodedString()]
        };XCTFail("Accepted final chunk corruption")}
        catch {XCTAssertTrue(error.localizedDescription.contains("SHA-1 mismatch"));XCTAssertEqual(reads,70)}
        var calls:[String]=[];let tooBig=Data(repeating:65,count:18433)
        do {_ = try await OneKeyFileReader.read(deadline:60,clock:{0},valid:{true}) {method,_ in calls.append(method);return self.listing(tooBig)};XCTFail("Accepted impossible request budget")}
        catch {XCTAssertEqual(calls,["fs.list"]);XCTAssertTrue(error.localizedDescription.contains("18432-byte"))}
    }

}
