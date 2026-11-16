import Foundation
@testable import BeckonConfiguration

func decodeSyntheticSnapshot(_ json: [String: Any]? = nil) throws -> RuntimeSnapshot {
    try JSONDecoder().decode(RuntimeSnapshot.self, from: JSONSerialization.data(withJSONObject: json ?? syntheticSnapshot()))
}
func syntheticDiagnostic(active: Bool = true, code: String = "output_failed", device: Any = "device-B") -> [String: Any] {
    ["id": 1, "age_ms": 100, "severity": "error", "code": code, "device": device,
     "control": NSNull(), "source": NSNull(), "active": active, "count": 1, "recovery": "check_device_configuration"]
}

// SYNTHETIC contract-v1 consumer fixture. Not produced by Rust; cross-language acceptance is pending.
func syntheticConfiguration(revision: String = "atomic-r2") throws -> [String: Any] {
    var config = try JSONSerialization.jsonObject(with: Data(configJSON.utf8)) as! [String: Any]
    config["revision"] = revision
    var caps = try JSONSerialization.jsonObject(with: Data(capabilitiesJSON.utf8)) as! [String: Any]
    caps["runtime_snapshot_versions"] = [1]
    caps["presentation_changes"] = ["theme_state", "device_effect", "device_effect_v2", "device_channel"]
    caps["action_kinds"] = ["chord", "tap-hold-chords"]
    caps["feedback_states"] = ["idle", "held", "latched", "pending", "hold_fired", "error"]
    config["capabilities"] = caps
    return config
}
func syntheticSnapshot(sequence: UInt64 = 7, instance: String = "synthetic-boot") throws -> [String: Any] {
    let nilValue = NSNull()
    return ["version": 1, "instance_id": instance, "sequence": sequence, "sampled_at_unix_ms": 100_000,
            "age_ms": 0, "stale_after_ms": 15_000, "build": ["version": "synthetic", "revision": nilValue, "dirty": nilValue],
            "configuration": try syntheticConfiguration(), "configuration_revision": "atomic-r2", "ledger_available": true,
            "posting": ["permission": "unknown", "last_checked_age_ms": nilValue, "last_success_age_ms": nilValue,
                        "last_error": nilValue, "cleanup_pending": false], "devices": [syntheticDevice()], "bindings": [], "panes": [], "sources": [], "events": []]
}
func syntheticDevice(id: String = "desk", connectivity: String = "unknown", age: Any = NSNull()) -> [String: Any] {
    ["id": id, "adapter": "creator-micro", "observation_supported": true, "connectivity": connectivity,
     "observed_age_ms": age, "member_descriptors": [], "layer": NSNull(), "last_input_success_age_ms": NSNull(),
     "last_output_success_age_ms": NSNull(), "last_error": NSNull(), "controls": []]
}
func syntheticReply(_ value: [String: Any]) throws -> Data {
    var data = try JSONSerialization.data(withJSONObject: ["type": "ok", "data": value], options: [.sortedKeys])
    data.append(10); return data
}
actor SyntheticRuntimeTransport: Transport {
    var requests: [Request] = []
    var snapshot: [String: Any]
    init(snapshot: [String: Any]) { self.snapshot = snapshot }
    func replace(_ snapshot: [String: Any]) { self.snapshot = snapshot }
    func send(_ request: Request, to endpoint: String) async throws -> Data {
        requests.append(request)
        if request == .configModel { return try syntheticReply(syntheticConfiguration(revision: "bootstrap-only")) }
        if request == .status { throw ClientError.invalid("Synthetic spy: runtime consumer must not request legacy status") }
        return try syntheticReply(snapshot)
    }
}
