import XCTest
@testable import BeckonCreatorMicro
@MainActor final class OneKeyConfigBackupTests:XCTestCase {
    func testExactDurableReadbackAndRefusalToOverwriteOrCrossDeviceRestore() throws {
        let parent=FileManager.default.temporaryDirectory.resolvingSymlinksInPath().appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at:parent,withIntermediateDirectories:false)
        defer {try? FileManager.default.removeItem(at:parent)}
        let backup=OneKeyConfigBackup(directory:parent.appendingPathComponent("recovery"),serial:"device-A")
        let patch=try OneKeyConfigPatch.make(original:OneKeyConfigTransferTests().baseline(),deviceID:"device-A")
        try backup.persist(patch);try backup.journal("applied_verified")
        XCTAssertEqual(try backup.load().original,patch.original)
        XCTAssertEqual(try backup.load().candidate,patch.candidate)
        XCTAssertThrowsError(try backup.persist(patch))
        XCTAssertThrowsError(try OneKeyConfigBackup(directory:backup.directory,serial:"device-B").load())
        try Data("tampered".utf8).write(to:backup.directory.appendingPathComponent("candidate-keymap.json"))
        XCTAssertThrowsError(try backup.load())
    }
    func testSymlinkBackupRefused() throws {
        let parent=FileManager.default.temporaryDirectory.resolvingSymlinksInPath().appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at:parent,withIntermediateDirectories:false)
        defer {try? FileManager.default.removeItem(at:parent)}
        let link=parent.appendingPathComponent("link")
        try FileManager.default.createSymbolicLink(at:link,withDestinationURL:parent)
        let patch=try OneKeyConfigPatch.make(original:OneKeyConfigTransferTests().baseline(),deviceID:"device-A")
        XCTAssertThrowsError(try OneKeyConfigBackup(directory:link.appendingPathComponent("recovery"),serial:"device-A").persist(patch))
    }
}
