import Foundation
import SocketSupport

public enum Endpoint {
    public static func discover(environment: [String: String] = ProcessInfo.processInfo.environment,
                                temporary: String = NSTemporaryDirectory()) -> String {
        if let runtime = environment["XDG_RUNTIME_DIR"] { return runtime + "/beckon.sock" }
        let state = environment["XDG_STATE_HOME"]
            ?? environment["HOME"].map { $0 + "/.local/state" } ?? temporary
        return state + "/beckon/beckon.sock"
    }
    public static func validate(_ path: String) throws {
        guard path.hasPrefix("/"), !path.utf8.contains(0), path.utf8.count < 104 else {
            throw ClientError.invalid("Socket path must be absolute and shorter than 104 UTF-8 bytes.")
        }
    }
}
public protocol Transport: Sendable {
    func send(_ request: Request, to endpoint: String) async throws -> Data
}
private final class CancelToken: @unchecked Sendable {
    let raw: OpaquePointer
    init() throws {
        guard let token = bp_token_create() else { throw ClientError.unavailable("Cannot allocate socket request.") }
        raw = token
    }
    func cancel() { bp_token_cancel(raw) }
    deinit { bp_token_destroy(raw) }
}
public struct SocketTransport: Transport {
    public var timeoutMilliseconds: Int32
    public init(timeoutMilliseconds: Int32 = 2000) { self.timeoutMilliseconds = timeoutMilliseconds }
    public func send(_ request: Request, to endpoint: String) async throws -> Data {
        try Endpoint.validate(endpoint)
        guard (100...2000).contains(timeoutMilliseconds) else { throw ClientError.invalid("Invalid request timeout.") }
        let bytes = try JSONEncoder().encode(request)
        guard bytes.count < 4096 else { throw ClientError.invalid("Request is too large.") }
        let line = String(decoding: bytes, as: UTF8.self) + "\n"
        try Task.checkCancellation()
        let token = try CancelToken()
        return try await withTaskCancellationHandler(operation: {
            try await Task.detached(priority: .utility) {
                var result = bp_socket_limit(endpoint, line, timeoutMilliseconds, 1_048_576, token.raw)
                defer { bp_result_free(&result) }
                switch result.code {
                case 0: return result.bytes.map { Data(bytes: $0, count: result.length) } ?? Data()
                case 1: throw ClientError.cancelled
                case 2: throw ClientError.timeout
                case 3: throw ClientError.outputLimit
                default: throw ClientError.unavailable("Socket unavailable or reply incomplete: \(endpoint)")
                }
            }.value
        }, onCancel: { token.cancel() })
    }
}
