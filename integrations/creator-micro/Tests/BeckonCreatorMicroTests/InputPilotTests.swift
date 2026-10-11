import XCTest
@testable import BeckonCreatorMicro

@MainActor final class InputPilotTests: XCTestCase {
    private func input() throws -> Data { try InputPilotExamples.inputSnapshot() }
    private func changed(_ mutate: (inout [String: Any]) -> Void) throws -> Data {
        var value = try XCTUnwrap(JSONSerialization.jsonObject(with: input()) as? [String: Any])
        mutate(&value)
        return try JSONSerialization.data(withJSONObject: value)
    }
    private func ready() throws -> InputPilotModel {
        let model = InputPilotModel()
        model.loadSnapshot(try input(), deviceID: "33432")
        model.selectProfile(2); model.selectLayer(0)
        return model
    }
    func testActualInputShapeSeparatesIDsFromArrayPositions() throws {
        let snapshot = try InputSnapshot.decode(input(), deviceID: "33432")
        XCTAssertEqual(snapshot.deviceID, "33432")
        XCTAssertEqual(snapshot.activeProfileIndex, 0)
        XCTAssertEqual(snapshot.profiles[0].id, 2)
        XCTAssertEqual(snapshot.profiles[0].layers.map(\.id), [3, 1, 0, 5, 4])
        XCTAssertEqual(snapshot.profiles[0].layers[2].name, "Herdr")
        XCTAssertEqual(snapshot.profiles[0].layers[2].keymap[2][3], "KC_ESC")
        XCTAssertEqual(snapshot.fingerprint.count, 64)
    }
    func testReaderIsBoundedReadOnlyAndRejectsSymlink() throws {
        let folder = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: folder) }
        let file = folder.appendingPathComponent("keymap.json"), original = try input()
        try original.write(to: file)
        _ = try InputSnapshotReader.read(file, deviceID: "33432")
        XCTAssertEqual(try Data(contentsOf: file), original)
        let link = folder.appendingPathComponent("link.json")
        try FileManager.default.createSymbolicLink(at: link, withDestinationURL: file)
        XCTAssertThrowsError(try InputSnapshotReader.read(link, deviceID: "33432"))
        try Data(repeating: 32, count: InputSnapshot.maximumBytes + 1).write(to: file)
        XCTAssertThrowsError(try InputSnapshotReader.read(file, deviceID: "33432"))
        XCTAssertThrowsError(try InputSnapshot.decode(Data(repeating: 32, count: InputSnapshot.maximumBytes + 1), deviceID: "33432"))
    }
    func testUnsupportedVersionDuplicateIDsAndMalformedGeometryRefuse() throws {
        for change in 0..<5 {
            let data = try changed { value in
                var profiles = value["profiles"] as! [[String: Any]]
                var layers = profiles[0]["layers"] as! [[String: Any]]
                if change == 0 { value["version"] = 2 }
                if change == 1 { profiles.append(profiles[0]) }
                if change == 2 { layers.append(layers[0]); profiles[0]["layers"] = layers }
                if change == 3 { var layout = layers[2]["layout"] as! [String: Any]; layout["keymap"] = [["KC_NONE"]]; layers[2]["layout"] = layout; profiles[0]["layers"] = layers }
                if change == 4 { value["activeProfileId"] = 2 }
                value["profiles"] = profiles
            }
            XCTAssertThrowsError(try InputSnapshot.decode(data, deviceID: "33432"))
        }
        XCTAssertThrowsError(try InputSnapshot.decode(input(), deviceID: ""))
    }
    func testSelectionIsExplicitAndReorderRenameNeverRetarget() throws {
        let model = InputPilotModel(); model.loadSnapshot(try input(), deviceID: "33432")
        XCTAssertNil(model.profileID); XCTAssertNil(model.layerID)
        XCTAssertFalse(model.acknowledgeFakeAG("AG03"))
        model.selectProfile(2); model.selectLayer(0)
        XCTAssertEqual(model.selectedLayer?.index, 2)
        let data = try changed { value in
            var profiles = value["profiles"] as! [[String: Any]]
            var layers = profiles[0]["layers"] as! [[String: Any]]
            var herdr = layers.remove(at: 2); herdr["name"] = "Renamed Herdr"
            layers.insert(herdr, at: 0); profiles[0]["layers"] = layers; value["profiles"] = profiles
        }
        model.loadSnapshot(data, deviceID: "33432")
        XCTAssertEqual(model.layerID, 0); XCTAssertEqual(model.selectedLayer?.index, 0)
        XCTAssertEqual(model.selectedLayer?.name, "Renamed Herdr")
        XCTAssertEqual(model.keys.filter(\.enabled).map(\.slot), [4])
    }
    func testDeletedSelectionAndFailedImportDisableFakeEvents() throws {
        let model = try ready(); XCTAssertTrue(model.acknowledgeFakeAG("AG03"))
        let deleted = try changed { value in
            var profiles = value["profiles"] as! [[String: Any]]
            let layers = profiles[0]["layers"] as! [[String: Any]]
            profiles[0]["layers"] = layers.filter { ($0["id"] as? Int) != 0 }; value["profiles"] = profiles
        }
        model.loadSnapshot(deleted, deviceID: "33432")
        XCTAssertNil(model.layerID); XCTAssertNil(model.lastAcknowledgment)
        XCTAssertFalse(model.acknowledgeFakeAG("AG03"))
        model.loadSnapshot(Data("broken".utf8), deviceID: "33432")
        XCTAssertNil(model.snapshot); XCTAssertTrue(model.keys.allSatisfy { !$0.enabled })
        XCTAssertNotNil(model.error)
    }
    func testFailedPickerRevokesPreviousSelectionAndAcknowledgment() throws {
        let model = try ready(); XCTAssertTrue(model.acknowledgeFakeAG("AG03"))
        model.reportImportFailure(NSError(domain: NSCocoaErrorDomain, code: NSFileReadNoPermissionError))
        XCTAssertNil(model.snapshot); XCTAssertNil(model.profileID); XCTAssertNil(model.layerID)
        XCTAssertNil(model.lastAcknowledgment); XCTAssertEqual(model.acknowledgmentCount, 0)
        XCTAssertNotNil(model.error); XCTAssertTrue(model.keys.allSatisfy { !$0.enabled })
        XCTAssertFalse(model.acknowledgeFakeAG("AG03"))
    }
    func testOnlyCocoaPickerCancellationPreservesPreviousSelectionAndReceipt() throws {
        let model = try ready(); XCTAssertTrue(model.acknowledgeFakeAG("AG03"))
        let snapshot = model.snapshot?.fingerprint, receipt = model.lastAcknowledgment
        model.reportImportFailure(NSError(domain: NSCocoaErrorDomain, code: NSUserCancelledError))
        XCTAssertEqual(model.snapshot?.fingerprint, snapshot)
        XCTAssertEqual(model.profileID, 2); XCTAssertEqual(model.layerID, 0)
        XCTAssertEqual(model.lastAcknowledgment, receipt); XCTAssertEqual(model.acknowledgmentCount, 1)
        XCTAssertNil(model.error)
        model.reportImportFailure(NSError(domain: "different.domain", code: NSUserCancelledError))
        XCTAssertNil(model.snapshot); XCTAssertNil(model.lastAcknowledgment)
        XCTAssertFalse(model.acknowledgeFakeAG("AG03"))
    }
    func testRenderResourceFailureRevokesPreviousTreatmentAndReceipt() throws {
        let model = try ready(); XCTAssertTrue(model.acknowledgeFakeAG("AG03"))
        model.reportRenderFailure(NSError(domain: NSCocoaErrorDomain, code: NSFileReadNoSuchFileError))
        XCTAssertNil(model.render); XCTAssertNil(model.lastAcknowledgment)
        XCTAssertEqual(model.acknowledgmentCount, 0); XCTAssertNotNil(model.error)
        XCTAssertFalse(model.acknowledgeFakeAG("AG03"))
    }
    func testDeviceAndProfileChangesClearSelectionAndAcknowledgments() throws {
        let model = try ready(); _ = model.acknowledgeFakeAG("AG03")
        model.loadSnapshot(try input(), deviceID: "other-device")
        XCTAssertNil(model.profileID); XCTAssertNil(model.layerID); XCTAssertEqual(model.acknowledgmentCount, 0)
        model.selectProfile(2); model.selectLayer(0); _ = model.acknowledgeFakeAG("AG03")
        model.selectProfile(nil)
        XCTAssertNil(model.layerID); XCTAssertNil(model.lastAcknowledgment)
    }
    func testOnlyWorkerSlotFourUsesFixtureAndExactFakeAG03() throws {
        let model = try ready(), keys = model.keys
        XCTAssertEqual(keys.map(\.physicalID), [1, 0, 2, 3, 4, 5])
        XCTAssertEqual(keys.map(\.row), [0, 0, 1, 1, 1, 1])
        XCTAssertEqual(keys.map(\.column), [1, 0, 0, 1, 2, 3])
        XCTAssertEqual(keys.filter(\.enabled).map(\.slot), [4])
        let worker = try XCTUnwrap(keys.first { $0.slot == 4 })
        XCTAssertEqual(worker.inputCode, "KC_NONE")
        XCTAssertEqual(worker.treatment?.state, "working")
        XCTAssertEqual(worker.treatment?.color, "#F9E2AF")
        for code in ["AG01", "AG00", "AG02", "AG04", "AG05", "AG3", "AG03suffix"] { XCTAssertFalse(model.acknowledgeFakeAG(code)) }
        XCTAssertTrue(model.acknowledgeFakeAG("AG03")); XCTAssertEqual(model.acknowledgmentCount, 1)
        XCTAssertTrue(model.lastAcknowledgment?.contains("Simulated") == true)
        model.loadSnapshot(try input(), deviceID: "33432")
        XCTAssertEqual(model.layerID, 0); XCTAssertNil(model.lastAcknowledgment)
    }
    func testRenderProtocolAndCompleteUniqueLogicalKeysAreRequired() throws {
        let original = try InputPilotExamples.renderPlan()
        XCTAssertEqual(try InputRenderPlan.decode(original).keys.count, 10)
        for change in 0..<7 {
            var value = try XCTUnwrap(JSONSerialization.jsonObject(with: original) as? [String: Any])
            var plan = value["plan"] as! [String: Any], keys = plan["keys"] as! [[String: Any]]
            if change == 0 { value["version"] = 2 }
            if change == 1 { value["protocol"] = "other" }
            if change == 2 { keys.removeLast() }
            if change == 3 { keys[0]["key"] = "f4" }
            if change == 4 { keys[3]["brightness"] = 2 }
            if change == 5 { keys[3]["state"] = "waiting_for_user" }
            if change == 6 { keys[3]["color"] = "red" }
            plan["keys"] = keys; value["plan"] = plan
            XCTAssertThrowsError(try InputRenderPlan.decode(JSONSerialization.data(withJSONObject: value)))
        }
    }
    func testBadRenderRevokesPreviousFakeAcknowledgment() throws {
        let model = try ready(); XCTAssertTrue(model.acknowledgeFakeAG("AG03"))
        model.loadRender(Data("broken".utf8))
        XCTAssertNil(model.render); XCTAssertNil(model.lastAcknowledgment)
        XCTAssertFalse(model.acknowledgeFakeAG("AG03"))
    }
    func testOfflineModeRejectsLiveAndPersistenceArguments() throws {
        XCTAssertTrue(try InputPilotLaunch.isRequested(arguments: ["--input-pilot"]))
        XCTAssertFalse(try InputPilotLaunch.isRequested(arguments: ["--demo", "--no-save"]))
        for args in [["--input-pilot", "--save"], ["--input-pilot", "--herdr-socket", "/some/socket"], ["--input-pilot", "--config", "/some/config"]] {
            XCTAssertThrowsError(try InputPilotLaunch.isRequested(arguments: args))
        }
    }
}
