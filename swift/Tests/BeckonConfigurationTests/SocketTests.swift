import XCTest
import Darwin
@testable import BeckonConfiguration

// Reuses the prototype's fragmented Unix-socket fixture pattern, never a live daemon.
private final class UnixFixture: @unchecked Sendable {
    let path: String
    let listener: Int32
    let worker: Task<Data, Never>
    init(reply: Data, delay: UInt64 = 0, fragment: Int = 7) throws {
        path = NSTemporaryDirectory() + "bc-\(UUID().uuidString.prefix(8)).sock"
        try Endpoint.validate(path)
        let fd = socket(AF_UNIX, SOCK_STREAM, 0); listener = fd
        guard fd >= 0 else { throw ClientError.unavailable("Fixture socket") }
        var address = sockaddr_un(); address.sun_family = sa_family_t(AF_UNIX)
        address.sun_len = UInt8(MemoryLayout<sockaddr_un>.size)
        let pathBytes = Array(path.utf8) + [0]
        withUnsafeMutableBytes(of: &address.sun_path) { $0.copyBytes(from: pathBytes) }
        let bound = withUnsafePointer(to: &address) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { Darwin.bind(fd, $0, socklen_t(MemoryLayout<sockaddr_un>.size)) }
        }
        guard bound == 0, listen(fd, 1) == 0 else { close(fd); throw ClientError.unavailable("Fixture bind") }
        worker = Task.detached {
            var poller = pollfd(fd: fd, events: Int16(POLLIN), revents: 0)
            guard poll(&poller, 1, 3000) > 0 else { return Data() }
            let client = accept(fd, nil, nil)
            guard client >= 0 else { return Data() }
            defer { close(client) }
            var yes: Int32 = 1
            setsockopt(client, SOL_SOCKET, SO_NOSIGPIPE, &yes, socklen_t(MemoryLayout<Int32>.size))
            var timeout = timeval(tv_sec: 2, tv_usec: 0)
            setsockopt(client, SOL_SOCKET, SO_RCVTIMEO, &timeout, socklen_t(MemoryLayout<timeval>.size))
            var request = Data(), buffer = [UInt8](repeating: 0, count: 4096)
            while !request.contains(10), request.count < 8192 {
                let n = read(client, &buffer, buffer.count)
                guard n > 0 else { return request }
                request.append(contentsOf: buffer.prefix(n))
            }
            if delay > 0 { try? await Task.sleep(nanoseconds: delay) }
            var sent = 0
            while sent < reply.count {
                let n = reply.withUnsafeBytes {
                    Darwin.send(client, $0.baseAddress!.advanced(by: sent), min(fragment, reply.count - sent), 0)
                }
                guard n > 0 else { break }; sent += n
            }
            return request
        }
    }
    deinit { shutdown(listener, SHUT_RDWR); close(listener); unlink(path) }
}

final class SocketTests: XCTestCase {
    func testFragmentedSuccessAndExactQualifiedBindRequest() async throws {
        let server = try UnixFixture(reply: ok(configJSON))
        let data = try await SocketTransport().send(.configModel, to: server.path)
        let model = try JSONDecoder().decode(Reply<Configuration>.self, from: data).value()
        XCTAssertEqual(model.bindings.count, 3)
        let request = await server.worker.value
        XCTAssertEqual(try JSONSerialization.jsonObject(with: request) as? [String: String], ["op":"config_model"])
        let binding = try UnixFixture(reply: ok("{}"))
        _ = try await SocketTransport().send(.bind(key: "review", agent: identity), to: binding.path)
        let bound = await binding.worker.value
        XCTAssertEqual(try JSONSerialization.jsonObject(with: bound) as? [String: String],
                       ["op":"bind", "key":"review", "source":"rex", "session":"work", "pane_id":"same"])
    }
    func testDaemonErrorsAreNotSuccess() async throws {
        let server = try UnixFixture(reply: Data("{\"type\":\"error\",\"message\":\"occupied\"}\n".utf8))
        let data = try await SocketTransport().send(.releaseKey("review"), to: server.path)
        XCTAssertThrowsError(try JSONDecoder().decode(Reply<JSONValue>.self, from: data).value()) {
            XCTAssertEqual($0 as? ClientError, .daemon("occupied"))
        }
        _ = await server.worker.value
    }
    func testTruncationMalformedAndOversizedResponses() async throws {
        for reply in [Data("{\"type\":".utf8), Data(repeating: 120, count: 1_048_578)] {
            let server = try UnixFixture(reply: reply, fragment: 8192)
            do { _ = try await SocketTransport().send(.status, to: server.path); XCTFail("Accepted incomplete response") }
            catch { XCTAssertNotNil(error as? ClientError) }
            _ = await server.worker.value
        }
        let server = try UnixFixture(reply: Data("not-json\n".utf8))
        let data = try await SocketTransport().send(.status, to: server.path)
        XCTAssertThrowsError(try JSONDecoder().decode(Reply<Status>.self, from: data))
        _ = await server.worker.value
    }
    func testTimeoutCancellationAndDisconnected() async throws {
        let slow = try UnixFixture(reply: ok("{}"), delay: 400_000_000)
        do { _ = try await SocketTransport(timeoutMilliseconds: 100).send(.status, to: slow.path); XCTFail("No timeout") }
        catch { XCTAssertEqual(error as? ClientError, .timeout) }
        _ = await slow.worker.value
        let cancelled = try UnixFixture(reply: ok("{}"), delay: 400_000_000)
        let job = Task { try await SocketTransport().send(.status, to: cancelled.path) }
        try await Task.sleep(nanoseconds: 40_000_000); job.cancel()
        do { _ = try await job.value; XCTFail("Ignored cancellation") }
        catch { XCTAssertEqual(error as? ClientError, .cancelled) }
        _ = await cancelled.worker.value
        do { _ = try await SocketTransport().send(.status, to: "/nonexistent-beckon-fixture.sock"); XCTFail("Connected") }
        catch { XCTAssertNotNil(error as? ClientError) }
    }
}
