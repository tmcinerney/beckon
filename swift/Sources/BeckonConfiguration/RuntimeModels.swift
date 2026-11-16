import Foundation

// AIDEV-NOTE: Runtime v1 is a cache-only sample, not legacy status() reconciliation.
// Producer/consumer wire acceptance must use actual Rust serializer bytes, not synthetic inputs.
protocol OpenStringState: RawRepresentable, Codable where RawValue == String {
    static var unknown: Self { get }
}
extension OpenStringState {
    public init(from decoder: Decoder) throws {
        self = Self(rawValue: try decoder.singleValueContainer().decode(String.self)) ?? Self.unknown
    }
    public func encode(to encoder: Encoder) throws { var c = encoder.singleValueContainer(); try c.encode(rawValue) }
}
public enum Connectivity: String, OpenStringState, Sendable { case unknown, connected, disconnected }
public enum SourceAvailability: String, OpenStringState, Sendable { case unknown, available, unavailable }
public enum OwnerAvailability: String, OpenStringState, Sendable { case unknown, free, live, dormant }
public enum PostingPermission: String, OpenStringState, Sendable { case unknown, allowed, denied }
public enum DiagnosticSeverity: String, OpenStringState, Sendable { case unknown, info, warning, error }
public enum DiagnosticCode: String, OpenStringState, Sendable {
    case unknown, device_unavailable, input_failed, output_failed, posting_denied, event_creation_failed, cleanup_pending
    case source_unavailable, source_snapshot_invalid, ledger_unavailable, ledger_invalid, recovered
    public var message: String {
        switch self {
        case .unknown: return "Unsupported diagnostic"
        case .device_unavailable: return "Device unavailable"
        case .input_failed: return "Device input failed"
        case .output_failed: return "Device output failed"
        case .posting_denied: return "Key posting permission denied"
        case .event_creation_failed: return "Could not create key event"
        case .cleanup_pending: return "Key cleanup pending"
        case .source_unavailable: return "Agent source unavailable"
        case .source_snapshot_invalid: return "Agent source snapshot invalid"
        case .ledger_unavailable: return "Binding ownership unavailable"
        case .ledger_invalid: return "Binding ledger invalid"
        case .recovered: return "Recovered"
        }
    }
}
public enum Recovery: String, OpenStringState, Sendable {
    case unknown, none, reconnect_device, check_device_configuration, check_source, check_ledger
    case grant_posting_permission, retry_cleanup, upgrade_client
    public var guidance: String {
        switch self {
        case .unknown: return "This client cannot interpret the recovery advice."
        case .none: return "No action required."
        case .reconnect_device: return "Check the device connection during an attended session."
        case .check_device_configuration: return "Review the device configuration."
        case .check_source: return "Check the agent source."
        case .check_ledger: return "Review the binding ledger before changing ownership."
        case .grant_posting_permission: return "Review Beckon's posting permission during an attended session."
        case .retry_cleanup: return "Unresolved key cleanup requires an attended daemon check; do not replay the action."
        case .upgrade_client: return "Use a client supporting this daemon capability."
        }
    }
}
public struct DiagnosticEvent: Codable, Equatable, Identifiable, Sendable {
    public var id: UInt64
    public var age_ms: UInt64
    public var severity: DiagnosticSeverity
    public var code: DiagnosticCode
    public var device: String?
    public var control: String?
    public var source: String?
    public var active: Bool
    public var count: UInt64
    public var recovery: Recovery
    public var summary: String { "\(code.message) · \(active ? "active" : "historical") · \(device ?? source ?? "daemon")\(control.map { " / \($0)" } ?? "")" }
}
public struct BuildIdentity: Codable, Equatable, Sendable {
    public var version: String
    public var revision: String?
    public var dirty: Bool?
}
public struct PostingHealth: Codable, Equatable, Sendable {
    public var permission: PostingPermission
    public var last_checked_age_ms: UInt64?
    public var last_success_age_ms: UInt64?
    public var last_error: DiagnosticEvent?
    public var cleanup_pending: Bool
}
public struct LayerDescriptor: Codable, Equatable, Sendable {
    public var name: String?
    public var profile: UInt16?
    public var position: UInt16?
    public var label: String {
        name ?? "Layer name unavailable · profile \(profile.map(String.init) ?? "unknown") / position \(position.map(String.init) ?? "unknown")"
    }
}
public struct ObservedLayer: Codable, Equatable, Sendable {
    public var name: String?
    public var profile: UInt16?
    public var position: UInt16?
    public var observed_age_ms: UInt64
    public var label: String { LayerDescriptor(name: name, profile: profile, position: position).label }
}
public struct MemberDescriptor: Codable, Equatable, Sendable {
    public var address: String
    public var name: String?
    public var layer: LayerDescriptor?
    public var label: String { "\(name ?? address)\(layer.map { " · \($0.label)" } ?? "")" }
}
public struct ControlObservation: Codable, Equatable, Identifiable, Sendable {
    public var id: String
    public var phase: String
    public var pressed: Bool
    public var cleanup_pending: Bool
}
public struct DeviceHealth: Codable, Equatable, Identifiable, Sendable {
    public var id: String
    public var adapter: String
    public var observation_supported: Bool
    public var connectivity: Connectivity
    public var observed_age_ms: UInt64?
    public var member_descriptors: [MemberDescriptor]
    public var layer: ObservedLayer?
    public var last_input_success_age_ms: UInt64?
    public var last_output_success_age_ms: UInt64?
    public var last_error: DiagnosticEvent?
    public var controls: [ControlObservation]
}
public struct SourceHealth: Codable, Equatable, Identifiable, Sendable {
    public var id: String
    public var availability: SourceAvailability
    public var observed_age_ms: UInt64?
    public var last_success_age_ms: UInt64?
    public var last_error: DiagnosticEvent?
}
public struct BindingLegendEntry: Codable, Equatable, Identifiable, Sendable {
    public var key: String
    public var configured: Bool
    public var owner: AgentIdentity?
    public var availability: OwnerAvailability
    public var scope_title: String?
    public var title: String?
    public var labels_stale: Bool
    public var age_ms: UInt64?
    public var labels_age_ms: UInt64?
    public var id: String { key }
}
public struct ObservedPane: Codable, Equatable, Identifiable, Sendable {
    public var reference: AgentIdentity
    public var scope_title: String?
    public var title: String?
    public var agent_status: String
    public var focused: Bool
    public var availability: OwnerAvailability
    public var labels_stale: Bool
    public var age_ms: UInt64?
    public var labels_age_ms: UInt64?
    public var id: AgentIdentity { reference }
}
public struct RuntimeSnapshot: Codable, Equatable, Sendable {
    public var version: Int
    public var instance_id: String
    public var sequence: UInt64
    public var sampled_at_unix_ms: UInt64
    public var age_ms: UInt64
    public var stale_after_ms: UInt64
    public var build: BuildIdentity
    public var configuration: Configuration
    public var configuration_revision: String
    public var ledger_available: Bool
    public var posting: PostingHealth
    public var devices: [DeviceHealth]
    public var bindings: [BindingLegendEntry]
    public var panes: [ObservedPane]
    public var sources: [SourceHealth]
    public var events: [DiagnosticEvent]
    public func validate() throws {
        try configuration.validate()
        guard version == 1, !instance_id.isEmpty, !configuration_revision.isEmpty,
              configuration.revision == configuration_revision, stale_after_ms > 0,
              Set(devices.map(\.id)).count == devices.count, Set(bindings.map(\.key)).count == bindings.count,
              Set(devices.map(\.id)) == Set(configuration.devices.map(\.id)),
              devices.allSatisfy({ device in configuration.devices.contains { $0.id == device.id && $0.definition.adapter == device.adapter } }),
              Set(panes.map(\.reference)).count == panes.count, Set(sources.map(\.id)).count == sources.count,
              events.count <= 64 else { throw ClientError.invalid("Invalid runtime v1 sample or configuration revision mismatch.") }
    }
}
