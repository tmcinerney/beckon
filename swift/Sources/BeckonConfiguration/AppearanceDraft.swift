/// A refresh never advances a draft's revision implicitly. The operator must choose
/// to discard it or reconcile it against the newly observed configuration.
public struct AppearanceDraft<Value: Equatable> {
    public var value: Value
    public private(set) var baseline: Value
    public private(set) var revision: String?
    public init(_ value: Value, revision: String?) {
        self.value = value; baseline = value; self.revision = revision
    }
    public var hasChanges: Bool { value != baseline }
    public func matches(_ currentRevision: String?) -> Bool { revision == currentRevision }
    public mutating func discard(using current: Value, revision: String?) {
        value = current; baseline = current; self.revision = revision
    }
    public mutating func reconcile(using current: Value, revision: String?) {
        baseline = current; self.revision = revision
    }
}
