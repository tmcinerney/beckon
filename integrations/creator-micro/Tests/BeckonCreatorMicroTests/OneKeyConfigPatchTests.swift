import XCTest
@testable import BeckonCreatorMicro
final class OneKeyConfigPatchTests:XCTestCase {
    func baseline() throws -> Data {
        var d=try XCTUnwrap(JSONSerialization.jsonObject(with:InputPilotExamples.inputSnapshot()) as? [String:Any])
        var profiles=d["profiles"] as! [[String:Any]],layers=profiles[0]["layers"] as! [[String:Any]]
        let li=try XCTUnwrap(layers.firstIndex(where:{$0["id"] as? Int==0}))
        var layout=layers[li]["layout"] as! [String:Any],keys=layout["keymap"] as! [[String]];keys[1][1]="KC_NONE"
        layout["keymap"]=keys;layout["unknown-layout-field"]=["enabled":true,"offset":1.5];layers[li]["layout"]=layout
        profiles[0]["layers"]=layers;profiles[0]["unknown-profile-field"]="retained";d["profiles"]=profiles
        d["linkedApps"]=[["bundle":"example.app","profile":2]];d["unknown-definition"]=["version":4,"enabled":true]
        return try JSONSerialization.data(withJSONObject:d,options:[.prettyPrinted])
    }
    func testSingleCellPatchPreservesUnknownMetadataAndExactOriginalRollbackBytes() throws {
        let original=try baseline(),patch=try OneKeyConfigPatch.make(original:original,deviceID:"33432")
        XCTAssertEqual(patch.original,original)
        let snapshot=try InputSnapshot.decode(patch.candidate,deviceID:"33432")
        XCTAssertEqual(snapshot.profiles[0].layers.first(where:{$0.id==0})?.keymap[1][1],"KV_OAI_AG03")
        XCTAssertTrue(try patch.matchesOriginal(original));XCTAssertTrue(try patch.matchesCandidate(patch.candidate))
        XCTAssertEqual(try patch.restorePayload(current:patch.candidate),original)
        XCTAssertEqual(patch.originalSHA256,OneKeyConfigPatch.hash(original))
    }
    func testRestoreRefusesUnrelatedEditAndOccupiedSlotNeverBecomesOurBackup() throws {
        let patch=try OneKeyConfigPatch.make(original:baseline(),deviceID:"33432")
        var modified=try XCTUnwrap(JSONSerialization.jsonObject(with:patch.candidate) as? [String:Any])
        modified["unknown-definition"]=["version":5,"enabled":true]
        let changed=try JSONSerialization.data(withJSONObject:modified)
        XCTAssertThrowsError(try patch.restorePayload(current:changed))
        XCTAssertThrowsError(try OneKeyConfigPatch.make(original:patch.candidate,deviceID:"33432"))
        XCTAssertThrowsError(try OneKeyConfigPatch.make(original:Data(repeating:32,count:18433),deviceID:"33432"))
    }
}
