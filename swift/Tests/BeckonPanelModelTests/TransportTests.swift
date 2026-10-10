import XCTest
import Darwin
@testable import BeckonPanelModel

final class TransportTests: XCTestCase {
    func testDrainsBothPipesAndReturnsBoundedOutput() async throws {
        let runner = ProcessRunner()
        let bytes = try await runner.run(executable: "/bin/sh", arguments: ["-c",
            "i=0; while [ \"$i\" -lt 3000 ]; do printf 'e' >&2; i=$((i+1)); done; printf '{\"ok\":true}'"])
        XCTAssertEqual(String(decoding: bytes, as: UTF8.self), #"{"ok":true}"#)
    }
    func testSuccessfulRootCleansRedirectedDescendantBeforeReuse() async throws {
        let dir=FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at:dir,withIntermediateDirectories:true)
        defer {try? FileManager.default.removeItem(at:dir)}
        let file=dir.appendingPathComponent("child")
        let runner=ProcessRunner()
        let result=try await runner.run(executable:"/bin/sh",arguments:["-c",
            "/bin/sleep 20 >/dev/null 2>&1 & child=$!; printf '%s' \"$child\" > '\(file.path)'; printf '{}'; exit 0"])
        let pid=try XCTUnwrap(Int32(String(contentsOf:file,encoding:.utf8)))
        defer {if kill(pid,0)==0 {kill(pid,SIGKILL)}}
        XCTAssertEqual(String(decoding:result,as:UTF8.self),"{}")
        XCTAssertEqual(kill(pid,0),-1,"Redirected inherited-group child remains after success")
        _=try await runner.run(executable:"/bin/echo",arguments:["reuse"])
    }
    func testRootExitWithLeakedInheritedPipeIsStoppedPromptly() async throws {
        let runner=ProcessRunner(),start=Date()
        do {
            _=try await runner.run(executable:"/bin/sh",arguments:["-c","/bin/sleep 20 & exit 0"])
            XCTFail("Leaked pipe accepted")
        } catch {}
        XCTAssertLessThan(Date().timeIntervalSince(start),1.0)
    }
    func testStderrCapAlsoStopsChild() async throws {
        do {
            _=try await ProcessRunner().run(executable:"/bin/sh",arguments:["-c","/usr/bin/yes x >&2"])
            XCTFail("Stderr cap ignored")
        } catch {XCTAssertEqual(error as? PanelError,.outputLimit)}
    }
    func testOutputCapsAndDeadline() async throws {
        let runner = ProcessRunner()
        do { _ = try await runner.run(executable: "/usr/bin/yes", arguments: ["x"]); XCTFail("Unbounded output accepted") }
        catch { XCTAssertEqual(error as? PanelError, .outputLimit) }
        let start = Date()
        do { _ = try await runner.run(executable: "/bin/sh", arguments: ["-c", "trap '' TERM; /bin/sleep 20"], budget: 0.7); XCTFail("Deadline ignored") }
        catch { XCTAssertEqual(error as? PanelError, .timeout) }
        XCTAssertLessThan(Date().timeIntervalSince(start), 1.5)
    }
    func testCancellationAndInheritedNestedChildAreCleanedUp() async throws {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: dir) }
        let pidFile = dir.appendingPathComponent("pid")
        let runner = ProcessRunner()
        let job = Task { try await runner.run(executable: "/bin/sh", arguments: ["-c",
            "/bin/sleep 20 & child=$!; printf '%s' \"$child\" > '\(pidFile.path)'; wait"]) }
        for _ in 0..<50 {
            if FileManager.default.fileExists(atPath: pidFile.path) { break }
            try await Task.sleep(nanoseconds: 10_000_000)
        }
        job.cancel()
        do { _ = try await job.value; XCTFail("Cancellation ignored") }
        catch { XCTAssertEqual(error as? PanelError, .cancelled) }
        let pid = try XCTUnwrap(Int32(String(contentsOf: pidFile, encoding: .utf8)))
        for _ in 0..<50 {
            if kill(pid, 0) == -1 { break }
            try await Task.sleep(nanoseconds: 10_000_000)
        }
        XCTAssertEqual(kill(pid, 0), -1, "Inherited child escaped cleanup")
        let reused = try await runner.run(executable: "/bin/echo", arguments: ["reused"])
        XCTAssertEqual(String(decoding: reused, as: UTF8.self), "reused\n")
    }
}
