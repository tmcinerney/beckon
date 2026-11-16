import Foundation

public struct RuntimeEvidence {
    public let snapshot: RuntimeSnapshot
    public let receivedAt: Date
    public let now: Date
    public init(snapshot: RuntimeSnapshot, receivedAt: Date, now: Date) {
        self.snapshot = snapshot; self.receivedAt = receivedAt; self.now = now
    }
    public func age(_ evidence: UInt64? = 0) -> Double? {
        evidence.map { Double($0) + Double(snapshot.age_ms) + max(0, now.timeIntervalSince(receivedAt)) * 1000 }
    }
    public func fresh(_ evidence: UInt64? = 0) -> Bool {
        now >= receivedAt && age(evidence).map { $0 < Double(min(snapshot.stale_after_ms, 15_000)) } == true
    }
}
public enum HealthLevel: String, Sendable { case disconnected, stale, unavailable, degraded, fault, observed }
public struct HealthSummary: Equatable, Sendable {
    public let level: HealthLevel
    public let title: String
    public let detail: String
    public var text: String { "\(title) · \(detail)" }
    public var symbol: String {
        switch level {
        case .disconnected: return "bolt.slash"
        case .stale: return "clock.badge.exclamationmark"
        case .unavailable: return "questionmark.circle"
        case .degraded: return "exclamationmark.triangle"
        case .fault: return "xmark.octagon"
        case .observed: return "keyboard"
        }
    }
    public static func make(connected: Bool, stale: Bool, snapshot: RuntimeSnapshot?, receivedAt: Date?, now: Date) -> Self {
        guard connected else { return Self(level: .disconnected, title: "Disconnected", detail: "No automatic reconnect; retained data is stale.") }
        guard let snapshot, let receivedAt else {
            return Self(level: .unavailable, title: "Diagnostics unavailable", detail: "IPC reachable is not device or permission health.")
        }
        let evidence = RuntimeEvidence(snapshot: snapshot, receivedAt: receivedAt, now: now)
        let connectedCount = snapshot.devices.filter { $0.observation_supported && $0.connectivity == .connected && evidence.fresh($0.observed_age_ms) }.count
        let disconnectedCount = snapshot.devices.filter { $0.observation_supported && $0.connectivity == .disconnected }.count
        let unknownCount = snapshot.devices.count - connectedCount - disconnectedCount
        let counts = "\(connectedCount) connected · \(disconnectedCount) disconnected · \(unknownCount) unknown"
        let diagnostics = snapshot.events + snapshot.devices.compactMap(\.last_error) + snapshot.sources.compactMap(\.last_error) + [snapshot.posting.last_error].compactMap { $0 }
        let faults = diagnostics.filter { $0.active && $0.severity == .error && $0.code != .device_unavailable && $0.code != .recovered && $0.code != .unknown }
        if let fault = faults.first {
            return Self(level: .fault, title: "Active error", detail: "\(fault.summary) · \(counts)\(stale || !evidence.fresh() ? " · snapshot stale" : "")")
        }
        if stale || !evidence.fresh() { return Self(level: .stale, title: "Stale", detail: counts) }
        if snapshot.devices.isEmpty || snapshot.devices.allSatisfy({ !$0.observation_supported }) {
            return Self(level: .unavailable, title: "Device observations unavailable", detail: counts)
        }
        let degraded = disconnectedCount > 0 || unknownCount > 0 || snapshot.posting.permission != .allowed
            || !evidence.fresh(snapshot.posting.last_checked_age_ms) || snapshot.posting.cleanup_pending
            || snapshot.sources.contains { $0.availability != .available || !evidence.fresh($0.observed_age_ms) }
            || !Set(snapshot.configuration.sources ?? []).isSubset(of: Set(snapshot.sources.map(\.id)))
            || diagnostics.contains { $0.code == .unknown || ($0.active && $0.code != .device_unavailable && $0.code != .recovered) }
        return Self(level: degraded ? .degraded : .observed, title: degraded ? "Degraded / unknown" : "Observed availability",
                    detail: "\(counts) · posting \(snapshot.posting.permission.rawValue) (host evidence only)")
    }
}

public struct BindingLegendRow: Equatable, Identifiable, Sendable {
    public let key: String
    public let configured: Bool
    public let owner: AgentIdentity?
    public let availability: OwnerAvailability
    public let labelsStale: Bool
    public let scopeTitle: String?
    public let title: String?
    public var id: String { key }
    public var summary: String {
        guard let owner else { return availability == .free ? "Free · authoritative ledger" : "Ownership unknown / unavailable" }
        let scope = "\(scopeTitle ?? "Workspace/session title unavailable") [\(owner.session)]"
        let pane = "\(title ?? "Block/pane title unavailable") [\(owner.pane_id)]"
        return "\(owner.source) → \(scope) → \(pane) · \(availability.rawValue)\(labelsStale ? " · labels stale" : "")"
    }
    public static func rows(snapshot: RuntimeSnapshot, receivedAt: Date, now: Date, retained: Bool) -> [Self] {
        let evidence = RuntimeEvidence(snapshot: snapshot, receivedAt: receivedAt, now: now)
        let records = Dictionary(uniqueKeysWithValues: snapshot.bindings.map { ($0.key, $0) })
        let sources = Dictionary(uniqueKeysWithValues: snapshot.sources.map { ($0.id, $0) })
        let keys = snapshot.configuration.bindings + snapshot.bindings.filter { !snapshot.configuration.bindings.contains($0.key) }.map(\.key)
        return keys.map { key in
            let record = records[key]
            var availability = record?.availability ?? .unknown
            if !snapshot.ledger_available { availability = .unknown }
            if availability == .free, retained || !evidence.fresh() { availability = .unknown }
            if availability == .free, record?.owner != nil { availability = .unknown }
            if availability == .live {
                let source = record?.owner.flatMap { sources[$0.source] }
                if source?.availability == .unavailable { availability = .dormant }
                else if retained || !evidence.fresh(record?.age_ms) || source?.availability != .available
                    || !evidence.fresh(source?.observed_age_ms) {
                    availability = .unknown
                }
            }
            let labelsStale = retained || record?.labels_stale != false || availability != .live || !evidence.fresh(record?.labels_age_ms)
            return Self(key: key, configured: snapshot.configuration.bindings.contains(key), owner: record?.owner,
                        availability: availability, labelsStale: labelsStale, scopeTitle: record?.scope_title, title: record?.title)
        }
    }
}
extension ActionDefinition {
    public var legend: String {
        switch self {
        case .chord(let keys, let mode, let ms):
            return "\(keys.joined(separator: " + ")) · \(mode)\(ms.map { " · configured \($0) ms" } ?? "")"
        case .tapHoldChords(let tap, let hold, let ms):
            return "Tap release: \(tap.joined(separator: " + ")) · Hold ≥ \(ms) ms: \(hold.joined(separator: " + ")) once · No tap on held release"
        case .unknown(let kind, let fields):
            return "Unsupported action \(kind) · inspect-only\n\(JSONValue.object(fields).formatted)"
        }
    }
}
