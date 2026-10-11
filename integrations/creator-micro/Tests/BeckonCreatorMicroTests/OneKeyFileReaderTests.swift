import XCTest
import CryptoKit
@testable import BeckonCreatorMicro
@MainActor final class OneKeyFileReaderTests:XCTestCase {
    func digest(_ data:Data)->String {Insecure.SHA1.hash(data:data).map {String(format:"%02x",$0)}.joined()}
    func listing(_ data:Data)->[[String:Any]] {[["name":"keymap.json","size":String(data.count),"checksum":digest(data)]]}
    func testFollowsInstalledChunkedContractAndChecksCompleteFile() async throws {
        let data=Data(repeating:65,count:4000);var calls:[String]=[]
        let result=try await OneKeyFileReader.read(deadline:60,clock:{0},valid:{true}) {method,params in
            calls.append(method)
            if method=="fs.list" {XCTAssertEqual(params["checksum"] as? Bool,true);return self.listing(data)}
            XCTAssertEqual(method,"fs.readbin");XCTAssertEqual(params["file"] as? String,"keymap.json")
            let offset=try XCTUnwrap(params["offset"] as? Int),length=try XCTUnwrap(params["len"] as? Int)
            XCTAssertEqual(length,min(3072,data.count-offset))
            return ["total_size":data.count,"data":data.subdata(in:offset..<(offset+length)).base64EncodedString()]
        }
        XCTAssertEqual(result.bytes,data);XCTAssertEqual(result.chunks,2)
        XCTAssertEqual(calls,["fs.list","fs.readbin","fs.readbin","fs.list"])
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
}
