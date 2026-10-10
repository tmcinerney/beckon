import Foundation
import Darwin
import PanelSupport

private final class CancelToken: @unchecked Sendable {
    let raw = bp_token_create()!
    func cancel() { bp_token_cancel(raw) }
    deinit { bp_token_destroy(raw) }
}
private struct CapturedResult: Sendable {
    var bytes: Data
    var code: Int32
    var pendingPID: Int32
    init(_ input: BPResult) {
        var result = input
        bytes = result.bytes.map { Data(bytes: $0, count: result.length) } ?? Data()
        code = result.code; pendingPID = result.pending_pid
        bp_result_free(&result)
    }
    func checked() throws -> Data {
        switch code {
        case 0: return bytes
        case 1: throw PanelError.cancelled
        case 2: throw PanelError.timeout
        case 3: throw PanelError.outputLimit
        case 4: throw PanelError.cleanupUnconfirmed
        default: throw PanelError.unavailable("Read command or socket failed; no idle state inferred")
        }
    }
}
private func cStrings<T>(_ strings: [String], _ operation: (UnsafeMutablePointer<UnsafeMutablePointer<CChar>?>) -> T) -> T {
    var pointers = strings.map { strdup($0) } + [nil]
    defer { for p in pointers { free(p) } }
    return pointers.withUnsafeMutableBufferPointer { operation($0.baseAddress!) }
}
public actor ProcessRunner {
    private var active = false
    private var cleanupWatcher: DispatchSourceProcess?
    private var cleanup = CleanupGate()
    public init() {}
    public var hasUnconfirmedCleanup:Bool {cleanup.pendingPID != nil}
    public func run(executable: String, arguments: [String], budget: Double = 3) async throws -> Data {
        guard !active else { throw PanelError.unavailable("Metadata read busy") }
        guard cleanup.pendingPID == nil else { throw PanelError.cleanupUnconfirmed }
        guard executable.hasPrefix("/"), !executable.utf8.contains(0),
              arguments.count <= 64, arguments.allSatisfy({ !$0.utf8.contains(0) && $0.utf8.count <= 4096 }),
              budget.isFinite, budget >= 0.1, budget <= 3 else { throw PanelError.invalid("Invalid read command") }
        try Task.checkCancellation()
        active = true
        defer { active = false }
        let token = CancelToken()
        let environment = ["HOME=\(FileManager.default.homeDirectoryForCurrentUser.path)", "PATH=/usr/bin:/bin:/usr/sbin:/sbin", "LANG=en_US.UTF-8"]
        let result = await withTaskCancellationHandler(operation: {
            await Task.detached(priority: .utility) {
                cStrings([executable] + arguments) { args in
                    cStrings(environment) { env in
                        CapturedResult(bp_process(executable, args, env, Int32(budget * 1000), token.raw))
                    }
                }
            }.value
        }, onCancel: { token.cancel() })
        if result.pendingPID > 0 {
            cleanup.requireReconciliation(pid: result.pendingPID)
            let pid = result.pendingPID
            let watcher = DispatchSource.makeProcessSource(identifier: pid, eventMask: .exit, queue: .global(qos: .utility))
            watcher.setEventHandler { [weak self] in Task { await self?.reap(pid) } }
            cleanupWatcher = watcher; watcher.resume()
        }
        return try result.checked()
    }
    public func reconcileCleanup() throws {
        guard !active else {throw PanelError.unavailable("Metadata read busy")}
        if let pid=cleanup.pendingPID {reap(pid)}
        try cleanup.reconcile()
    }
    private func reap(_ pid: Int32) {
        var status: Int32 = 0
        let result = waitpid(pid, &status, WNOHANG)
        let rootGone = result == pid || (result < 0 && errno == ECHILD)
        let groupResult = kill(-pid,0)
        let groupGone = groupResult < 0 && errno == ESRCH
        if rootGone && groupGone {
            cleanupWatcher?.cancel(); cleanupWatcher = nil; cleanup.observedReaped(pid:pid)
        }
    }
}
public protocol AgentStateSource: Sendable {
    func read(source: SourceKey, budget: Double) async throws -> AgentSnapshot
}
public struct SocketAgentSource: AgentStateSource {
    public init() {}
    public func read(source: SourceKey, budget: Double = 2) async throws -> AgentSnapshot {
        try source.validate()
        guard budget.isFinite, budget >= 0.1, budget <= 2 else { throw PanelError.invalid("Invalid socket budget") }
        try Task.checkCancellation()
        let token = CancelToken(), id = UUID().uuidString
        let bytes = try JSONSerialization.data(withJSONObject: ["id": id, "method": "agent.list", "params": [:]])
        let request = String(decoding: bytes, as: UTF8.self) + "\n"
        let result = await withTaskCancellationHandler(operation: {
            await Task.detached(priority: .utility) {
                CapturedResult(bp_socket(source.socket, request, Int32(budget * 1000), token.raw))
            }.value
        }, onCancel: { token.cancel() })
        let data = try result.checked()
        guard let object = try JSONSerialization.jsonObject(with: data) as? [String: Any],
              object["id"] as? String == id else { throw PanelError.invalid("Socket response identity mismatch") }
        return try AgentSnapshot.decode(data, source: source, observedAt: Date().timeIntervalSince1970)
    }
}
