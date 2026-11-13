import Foundation

public enum ClientError: Error, LocalizedError, Equatable {
    case invalid(String), unavailable(String), daemon(String), cancelled, timeout, outputLimit
    public var errorDescription: String? {
        switch self {
        case .invalid(let message), .unavailable(let message), .daemon(let message): return message
        case .cancelled: return "Request cancelled."
        case .timeout: return "Beckon did not reply before the deadline."
        case .outputLimit: return "Beckon reply exceeded the size limit."
        }
    }
}

public enum Motion: String, Codable, CaseIterable, Sendable { case steady, breathe, pulse, flicker }
public enum ThemeState: String, Codable, CaseIterable, Sendable { case idle, working, blocked, done, unknown }
public enum FeedbackState: String, Codable, CaseIterable, Sendable { case idle, held, latched }
public struct Treatment: Codable, Equatable, Sendable {
    public var color: String
    public var brightness: Double
    public var motion: Motion
    public init(color: String, brightness: Double, motion: Motion) {
        self.color = color; self.brightness = brightness; self.motion = motion
    }
    public func validate() throws {
        guard color.range(of: "^#[0-9A-Fa-f]{6}$", options: .regularExpression) != nil,
              brightness.isFinite, (0...1).contains(brightness) else {
            throw ClientError.invalid("Use #RRGGBB and brightness between 0 and 1.")
        }
    }
}
public struct Display: Codable, Equatable, Sendable {
    public var theme: String
    public var themes: [String: [String: Treatment]]
}
public struct Control: Codable, Equatable, Sendable {
    public var members: [String]
    public var binding: String?
    public var action: String?
    public var effects: String?
}
public struct ActionDefinition: Codable, Equatable, Sendable {
    public var kind: String
    public var keys: [String]
    public var mode: String
    public var tap_ms: UInt64?
}
public struct Feedback: Codable, Equatable, Sendable {
    public var idle: Treatment?
    public var held: Treatment?
    public var latched: Treatment?
    public init(idle: Treatment? = nil, held: Treatment? = nil, latched: Treatment? = nil) {
        self.idle = idle; self.held = held; self.latched = latched
    }
    private enum CodingKeys: String, CodingKey { case idle, held, latched }
    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        // AIDEV-NOTE: The daemon replaces the effect; explicit null clears an override.
        try c.encode(idle, forKey: .idle)
        try c.encode(held, forKey: .held)
        try c.encode(latched, forKey: .latched)
    }
    public func validate() throws {
        guard idle != nil || held != nil || latched != nil else {
            throw ClientError.invalid("Keep at least one feedback treatment.")
        }
        for treatment in [idle, held, latched].compactMap({ $0 }) { try treatment.validate() }
    }
    public subscript(state: FeedbackState) -> Treatment? {
        get { self[state.rawValue] }
        set {
            switch state { case .idle: idle = newValue; case .held: held = newValue; case .latched: latched = newValue }
        }
    }
    public subscript(state: String) -> Treatment? {
        switch state { case "idle": return idle; case "held": return held; case "latched": return latched; default: return nil }
    }
}
// AIDEV-NOTE: Adapter options are an extension point, not a second structural config parser.
// Preserve unknown JSON options for inspection without hard-coding device names or mappings.
public enum JSONValue: Codable, Equatable, Sendable {
    case string(String), number(Double), bool(Bool), object([String: JSONValue]), array([JSONValue]), null
    public init(from decoder: Decoder) throws {
        let c = try decoder.singleValueContainer()
        if c.decodeNil() { self = .null }
        else if let v = try? c.decode(Bool.self) { self = .bool(v) }
        else if let v = try? c.decode(String.self) { self = .string(v) }
        else if let v = try? c.decode(Double.self) { self = .number(v) }
        else if let v = try? c.decode([String: JSONValue].self) { self = .object(v) }
        else { self = .array(try c.decode([JSONValue].self)) }
    }
    public func encode(to encoder: Encoder) throws {
        var c = encoder.singleValueContainer()
        switch self {
        case .string(let v): try c.encode(v)
        case .number(let v): try c.encode(v)
        case .bool(let v): try c.encode(v)
        case .object(let v): try c.encode(v)
        case .array(let v): try c.encode(v)
        case .null: try c.encodeNil()
        }
    }
    public var formatted: String {
        let encoder = JSONEncoder(); encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
        return (try? encoder.encode(self)).map { String(decoding: $0, as: UTF8.self) } ?? "Unavailable"
    }
}
public struct DeviceDefinition: Codable, Equatable, Sendable {
    public var adapter: String
    public var options: [String: JSONValue]
    public var keys: [String: Control]
    public var actions: [String: ActionDefinition]
    public var effects: [String: Feedback]
    public func channelTreatment(_ channel: String) -> Treatment? {
        guard let value = options[channel], let bytes = try? JSONEncoder().encode(value),
              let treatment = try? JSONDecoder().decode(Treatment.self, from: bytes),
              (try? treatment.validate()) != nil else { return nil }
        return treatment
    }
}
public struct Device: Codable, Equatable, Identifiable, Sendable {
    public var id: String
    public var path: String
    public var definition: DeviceDefinition
}
public struct Capabilities: Codable, Equatable, Sendable {
    public var presentation_changes: [String]
    public var binding_release_guard: Bool
    public var device_channels: [String: [String]]
}
public struct Configuration: Codable, Equatable, Sendable {
    public var config_version: Int
    public var revision: String?
    public var capabilities: Capabilities?
    public var sources: [String]?
    public var bindings: [String]
    public var devices: [Device]
    public var display: Display
    public func supportsPresentation(_ kind: String) -> Bool {
        revision?.isEmpty == false && capabilities?.presentation_changes.contains(kind) == true
    }
    public func supports(_ change: PresentationChange) -> Bool {
        switch change {
        case .themeState(let theme, let state, _):
            return supportsPresentation("theme_state") && display.themes[theme]?[state.rawValue] != nil
        case .deviceEffect(let device, let effect, _):
            return supportsPresentation("device_effect") && devices.first(where: { $0.id == device })?.definition.effects[effect] != nil
        case .deviceChannel(let device, let channel, _):
            return supportsPresentation("device_channel") && capabilities?.device_channels[device]?.contains(channel) == true
                && devices.first(where: { $0.id == device })?.definition.channelTreatment(channel) != nil
        }
    }
    public func validate() throws {
        guard config_version == 3, Set(bindings).count == bindings.count,
              Set(devices.map(\.id)).count == devices.count,
              display.themes[display.theme] != nil else { throw ClientError.invalid("Invalid or unsupported configuration model.") }
        for theme in display.themes.values { for treatment in theme.values { try treatment.validate() } }
    }
}
public enum PresentationChange: Equatable, Sendable, Encodable {
    case themeState(theme: String, state: ThemeState, treatment: Treatment)
    case deviceEffect(device: String, effect: String, effects: Feedback)
    case deviceChannel(device: String, channel: String, treatment: Treatment)
    private enum Key: String, CodingKey { case kind, theme, state, treatment, device, effect, effects, channel }
    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: Key.self)
        switch self {
        case .themeState(let theme, let state, let treatment):
            try c.encode("theme_state", forKey: .kind); try c.encode(theme, forKey: .theme)
            try c.encode(state, forKey: .state); try c.encode(treatment, forKey: .treatment)
        case .deviceEffect(let device, let effect, let effects):
            try c.encode("device_effect", forKey: .kind); try c.encode(device, forKey: .device)
            try c.encode(effect, forKey: .effect); try c.encode(effects, forKey: .effects)
        case .deviceChannel(let device, let channel, let treatment):
            try c.encode("device_channel", forKey: .kind); try c.encode(device, forKey: .device)
            try c.encode(channel, forKey: .channel); try c.encode(treatment, forKey: .treatment)
        }
    }
    public func validate() throws {
        switch self {
        case .themeState(_, _, let treatment), .deviceChannel(_, _, let treatment): try treatment.validate()
        case .deviceEffect(_, _, let effects): try effects.validate()
        }
    }
}
public struct AgentIdentity: Hashable, Codable, Sendable {
    public var source: String
    public var session: String
    public var pane_id: String
    public init(source: String, session: String, pane_id: String) {
        self.source = source; self.session = session; self.pane_id = pane_id
    }
    public var description: String { "\(source) · \(session) · \(pane_id)" }
}
public struct Agent: Codable, Equatable, Identifiable, Sendable {
    public var source: String
    public var session: String
    public var pane_id: String
    public var title: String
    public var binding: String
    public var agent_status: String
    public var focused: Bool
    public var id: AgentIdentity { AgentIdentity(source: source, session: session, pane_id: pane_id) }
}
public struct BoundPane: Codable, Equatable, Sendable {
    public var source: String
    public var session: String
    public var pane_id: String
    public var agent_status: String
}
public struct BindingRecord: Codable, Equatable, Sendable {
    public var key: String
    public var source: String
    public var session: String
    public var pane: BoundPane
    public var reference: AgentIdentity { AgentIdentity(source: source, session: session, pane_id: pane.pane_id) }
}
public struct Status: Codable, Equatable, Sendable {
    public var bindings: [BindingRecord]
    public var panes: [Agent]
    public func validate() throws {
        guard Set(panes.map(\.id)).count == panes.count,
              Set(bindings.map(\.key)).count == bindings.count,
              panes.allSatisfy({ !$0.source.isEmpty && !$0.session.isEmpty && !$0.pane_id.isEmpty }) else {
            throw ClientError.invalid("Ambiguous agent identities or duplicate binding records in status.")
        }
    }
}
struct ThemeReceipt: Decodable { let theme: String }
struct BindReceipt: Decodable { let key: String; let session: String; let pane_id: String; let changed: Bool }
struct ReleaseReceipt: Decodable { let key: String; let changed: Bool }

public enum Request: Equatable, Sendable, Encodable {
    case configModel, status, setTheme(String), bind(key: String, agent: AgentIdentity)
    case setPresentation(expectedRevision: String, change: PresentationChange)
    case releaseBinding(key: String, reference: AgentIdentity)
    public func encode(to encoder: Encoder) throws {
        enum Key: String, CodingKey { case op, name, key, pane_id, session, source, expected_revision, change, reference }
        var c = encoder.container(keyedBy: Key.self)
        switch self {
        case .configModel: try c.encode("config_model", forKey: .op)
        case .status: try c.encode("status", forKey: .op)
        case .setTheme(let name):
            try c.encode("set_theme", forKey: .op); try c.encode(name, forKey: .name)
        case .bind(let key, let agent):
            try c.encode("bind", forKey: .op); try c.encode(key, forKey: .key)
            try c.encode(agent.source, forKey: .source); try c.encode(agent.session, forKey: .session)
            try c.encode(agent.pane_id, forKey: .pane_id)
        case .setPresentation(let revision, let change):
            try c.encode("set_presentation", forKey: .op)
            try c.encode(revision, forKey: .expected_revision); try c.encode(change, forKey: .change)
        case .releaseBinding(let key, let reference):
            try c.encode("release_binding", forKey: .op)
            try c.encode(key, forKey: .key); try c.encode(reference, forKey: .reference)
        }
    }
}
public enum Reply<Value: Decodable>: Decodable {
    case ok(Value), error(String)
    private enum Key: String, CodingKey { case type, data, message }
    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: Key.self)
        switch try c.decode(String.self, forKey: .type) {
        case "ok": self = .ok(try c.decode(Value.self, forKey: .data))
        case "error": self = .error(try c.decode(String.self, forKey: .message))
        default: throw ClientError.invalid("Unknown Beckon response envelope.")
        }
    }
    public func value() throws -> Value {
        switch self { case .ok(let value): return value; case .error(let message): throw ClientError.daemon(message) }
    }
}
