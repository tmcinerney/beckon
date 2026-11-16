import Foundation

public protocol ClientRefreshScheduler: Sendable {
    func wait(seconds: TimeInterval) async throws
}
public struct ContinuousRefreshScheduler: ClientRefreshScheduler {
    public init() {}
    public func wait(seconds: TimeInterval) async throws {
        try await Task.sleep(nanoseconds: UInt64(seconds * 1_000_000_000))
    }
}
