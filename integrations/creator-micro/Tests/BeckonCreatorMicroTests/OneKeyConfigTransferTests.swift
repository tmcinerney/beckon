import XCTest
import CryptoKit
@testable import BeckonCreatorMicro
@MainActor final class OneKeyConfigTransferTests:XCTestCase {
    final class Device {
        var live:Data,staged=Data(),persisted=false
        var fail:String?,methods:[String]=[],offsets:[Int]=[],lengths:[Int]=[],completed:[Bool]=[]
        init(_ data:Data) {live=data}
        func call(_ method:String,_ params:[String:Any]) throws -> Any? {
            methods.append(method)
            switch method {
            case "fs.list":
                return [["name":"keymap.json","size":String(live.count),"checksum":Insecure.SHA1.hash(data:live).map{String(format:"%02x",$0)}.joined()]]
            case "fs.readbin":
                let offset=try XCTUnwrap(params["offset"] as? Int),length=try XCTUnwrap(params["len"] as? Int)
                return ["total_size":live.count,"data":live.subdata(in:offset..<min(offset+length,live.count)).base64EncodedString()]
            case "fs.txbegin":
                XCTAssertTrue(persisted);XCTAssertTrue(params.isEmpty);staged=Data();return ["tx":7]
            case "fs.writebin":
                XCTAssertEqual(params["file"] as? String,"keymap.json");XCTAssertEqual((params["tx"] as? NSNumber)?.int64Value,7);XCTAssertEqual(params["append"] as? Bool,true)
                let offset=try XCTUnwrap(params["offset"] as? Int),encoded=try XCTUnwrap(params["data"] as? String)
                let data=try XCTUnwrap(Data(base64Encoded:encoded));XCTAssertLessThanOrEqual(data.count,192);XCTAssertEqual(offset,staged.count)
                offsets.append(offset);lengths.append(data.count);completed.append(try XCTUnwrap(params["completed"] as? Bool))
                if fail=="stage" {throw InputPilotError.invalid("Injected staging failure")}
                staged.append(data);return ["ok":1]
            case "fs.txcommit":
                XCTAssertEqual((params["tx"] as? NSNumber)?.int64Value,7);live=staged
                if fail=="commit_applied" {throw InputPilotError.invalid("Lost commit reply")}
                if fail=="commit_divergent" {
                    var object=try XCTUnwrap(JSONSerialization.jsonObject(with:live) as? [String:Any]);object["unrelated-new-field"]=true
                    live=try JSONSerialization.data(withJSONObject:object);throw InputPilotError.invalid("Unknown commit outcome")
                }
                return ["ok":1]
            default:throw InputPilotError.invalid("Unexpected method")
            }
        }
    }
    func baseline() throws -> Data {
        var object=try XCTUnwrap(JSONSerialization.jsonObject(with:InputPilotExamples.inputSnapshot()) as? [String:Any])
        var profiles=object["profiles"] as! [[String:Any]],layers=profiles[0]["layers"] as! [[String:Any]]
        let li=try XCTUnwrap(layers.firstIndex(where:{$0["id"] as? Int==0}))
        var layout=layers[li]["layout"] as! [String:Any],keys=layout["keymap"] as! [[String]];keys[1][1]="KC_NONE"
        layout["keymap"]=keys;layers[li]["layout"]=layout;profiles[0]["layers"]=layers;object["profiles"]=profiles
        return try JSONSerialization.data(withJSONObject:object,options:[.prettyPrinted])
    }
    func testBackupPrecedesTransactionAndExactApplyRestoreRoundTrip() async throws {
        let original=try baseline(),device=Device(original);var journal:[String]=[]
        let result=try await OneKeyConfigTransfer.apply(deviceID:"33432",deadline:60,clock:{0},valid:{true},persist:{patch in XCTAssertEqual(patch.original,original);device.persisted=true},journal:{journal.append($0)},call:device.call)
        XCTAssertTrue(try result.patch.matchesCandidate(device.live));XCTAssertEqual(device.live,result.readback)
        XCTAssertEqual(device.offsets,Array(stride(from:0,to:result.patch.candidate.count,by:192)))
        XCTAssertEqual(device.completed.dropLast().allSatisfy{!$0},true);XCTAssertEqual(device.completed.last,true)
        XCTAssertTrue(journal.contains("applied_verified"))
        let restored=try await OneKeyConfigTransfer.restore(patch:result.patch,deviceID:"33432",deadline:60,clock:{0},valid:{true},journal:{journal.append($0)},call:device.call)
        XCTAssertTrue(restored.restored);XCTAssertEqual(device.live,original);XCTAssertTrue(journal.contains("restored_verified"))
        XCTAssertFalse(device.methods.contains("fs.txabort"));XCTAssertEqual(device.methods.filter{$0=="fs.txcommit"}.count,2)
    }
    func testFailureReadbackClassifiesLiveOriginalCandidateOrDivergentWithoutRetryOrAbort() async throws {
        for (failure,outcome) in [("stage","original_verified"),("commit_applied","candidate_verified"),("commit_divergent","divergent")] {
            let device=Device(try baseline());device.fail=failure
            do {_ = try await OneKeyConfigTransfer.apply(deviceID:"33432",deadline:60,clock:{0},valid:{true},persist:{_ in device.persisted=true},journal:{_ in},call:device.call);XCTFail("Expected failure")}
            catch let error as OneKeyConfigTransfer.Failure {
                XCTAssertEqual(error.liveOutcome,outcome);XCTAssertNotNil(error.readbackSHA256)
                XCTAssertLessThanOrEqual(device.methods.filter{$0=="fs.txcommit"}.count,1)
                XCTAssertFalse(device.methods.contains("fs.txabort"))
                if failure=="stage" {XCTAssertFalse(device.methods.contains("fs.txcommit"))}
            }
        }
    }
    func testFailedBackupOrDivergentRestoreMakesNoMutatingCalls() async throws {
        let device=Device(try baseline())
        do {_ = try await OneKeyConfigTransfer.apply(deviceID:"33432",deadline:60,clock:{0},valid:{true},persist:{_ in throw InputPilotError.invalid("Backup not durable")},journal:{_ in},call:device.call);XCTFail("Expected backup error")}
        catch {XCTAssertFalse(device.methods.contains("fs.txbegin"));XCTAssertFalse(device.methods.contains("fs.writebin"))}
        let patch=try OneKeyConfigPatch.make(original:device.live,deviceID:"33432")
        var object=try XCTUnwrap(JSONSerialization.jsonObject(with:patch.candidate) as? [String:Any]);object["unrelated-edit"]=true;device.live=try JSONSerialization.data(withJSONObject:object)
        do {_ = try await OneKeyConfigTransfer.restore(patch:patch,deviceID:"33432",deadline:60,clock:{0},valid:{true},journal:{_ in},call:device.call);XCTFail("Accepted divergent restore")}
        catch {XCTAssertFalse(device.methods.contains("fs.txbegin"))}
    }
}
