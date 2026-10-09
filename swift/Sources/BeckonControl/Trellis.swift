import Foundation

public struct TaskLocator: Codable {
    public let taskUid: String
    public let externalId: String
    public let title: String
    public let revision: Int
    public let phase: String
    public let nextAction: String?
    public let claim: Claim?
    public let recovery: Recovery?

    public struct Claim: Codable {
        public let owner: String?
        public let runUid: String
        public let generation: Int
        public let state: String
    }
    public struct Recovery: Codable {
        public let provider: String
        public let sessionId: String
        public let workingDirectory: String
        public let runtimeVerified: Bool
    }
}
public struct TrellisPage: Codable {
    public let compactVersion: Int
    public let tasks: [TaskLocator]
    public let hasMore: Bool
    public let nextOffset: Int?
}
private struct Envelope: Codable {
    let apiVersion: Int
    let ok: Bool
    let data: TrellisPage
}
public enum TrellisReader {
    public static func decode(_ data: Data) throws -> TrellisPage {
        let decoder=JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        let envelope=try decoder.decode(Envelope.self,from:data)
        guard envelope.ok, envelope.apiVersion == 1, envelope.data.compactVersion == 1 else {
            throw ControlError.unsupported
        }
        // Stored locators cannot become live/focus/approval evidence merely by decoding.
        guard envelope.data.tasks.allSatisfy({
            !$0.taskUid.isEmpty && $0.revision >= 0 && $0.recovery?.runtimeVerified != true
        }) else { throw ControlError.invalidConfiguration }
        return envelope.data
    }
}
