import SwiftUI
import BeckonConfiguration

/// Read-only inspection. Lifecycle and polling belong to the single shared model, never a view task.
@MainActor public struct DeviceStatusView: View {
    @ObservedObject private var model: ConfigurationModel
    public init(model: ConfigurationModel) { self.model = model }
    public var body: some View {
        TimelineView(.periodic(from: .now, by: 1)) { timeline in
            Form {
                Section("Beckon health") {
                    HealthSummaryContent(summary: model.healthSummary(at: timeline.date))
                    ConnectionButtons(model: model)
                    if let error = model.runtimeError { Text(error).foregroundStyle(.secondary).textSelection(.enabled) }
                    if let error = model.error { Text("Client operation: \(error)").foregroundStyle(.red).textSelection(.enabled) }
                    Text("Read-only sampling every \(ConfigurationModel.clientRefreshInterval, format: .number) seconds after Connect. Socket reachability is not hardware or permission health.")
                        .font(.caption).foregroundStyle(.secondary)
                    if let sample = model.runtime {
                        Text("Daemon \(sample.build.version) · revision \(sample.build.revision ?? "unavailable") · dirty \(sample.build.dirty.map { $0 ? "yes" : "no" } ?? "unavailable")")
                            .font(.caption).textSelection(.enabled)
                        Text("Instance \(sample.instance_id) · sample \(sample.sequence)").font(.caption).textSelection(.enabled)
                        Text("Posting preflight: \(sample.posting.permission.rawValue) · cleanup \(sample.posting.cleanup_pending ? "pending" : "not reported pending")")
                            .font(.caption)
                        if let error = sample.posting.last_error { DiagnosticContent(events: [error]) }
                    }
                }
                Section("Binding legend") {
                    BindingLegendContent(rows: model.bindingLegend(at: timeline.date))
                }
                if let config = model.configuration {
                    if config.devices.isEmpty { Section("Devices") { Text("No devices configured.") } }
                    ForEach(config.devices) { device in
                        Section("\(device.id) · \(device.definition.adapter)") {
                            DeviceInspection(device: device, model: model, now: timeline.date)
                        }
                    }
                } else { Section("Devices") { Text("Connect explicitly to load configuration. No automatic connection at startup.") } }
                Section("Recent diagnostics") { DiagnosticContent(events: model.runtime?.events ?? []) }
                Section("Agent source observations") {
                    if model.runtime?.sources.isEmpty != false { Text("Source observations unavailable; configured source names are not live evidence.").foregroundStyle(.secondary) }
                    if let sample = model.runtime, let received = model.runtimeReceivedAt {
                        let evidence = RuntimeEvidence(snapshot: sample, receivedAt: received, now: timeline.date)
                        ForEach(sample.sources) { source in
                            Text("\(source.id): \(source.availability.rawValue)\(model.connectedEndpoint != nil && !model.stale && evidence.fresh(source.observed_age_ms) ? "" : " · stale / unknown evidence")")
                            if let error = source.last_error { DiagnosticContent(events: [error]) }
                        }
                    }
                }
            }.formStyle(.grouped)
        }
    }
}

struct HealthSummaryContent: View {
    let summary: HealthSummary
    var body: some View {
        Label {
            VStack(alignment: .leading) {
                Text(summary.title).font(.headline)
                Text(summary.detail).font(.callout).textSelection(.enabled)
            }
        } icon: { Image(systemName: summary.symbol) }
            .accessibilityElement(children: .ignore)
            .accessibilityLabel("Beckon health")
            .accessibilityValue(summary.text)
    }
}
@MainActor struct ConnectionButtons: View {
    @ObservedObject var model: ConfigurationModel
    var body: some View {
        HStack {
            Button(model.connectedEndpoint == nil ? "Connect" : "Reconnect") { Task { await model.connect() } }.disabled(model.busy)
            Button("Disconnect") { model.disconnect() }.disabled(model.connectedEndpoint == nil)
            Button("Refresh") { Task { await model.refresh() } }.disabled(model.connectedEndpoint == nil || model.busy)
            if model.busy { ProgressView().controlSize(.small).accessibilityLabel("Reading Beckon") }
        }
    }
}
struct BindingLegendContent: View {
    let rows: [BindingLegendRow]
    var body: some View {
        if rows.isEmpty { Text("Complete binding ledger unavailable. Missing owners are not free slots.").foregroundStyle(.secondary) }
        ForEach(rows) { row in
            VStack(alignment: .leading, spacing: 3) {
                Text("\(row.key)\(row.configured ? "" : " · orphaned ledger key")").font(.headline)
                Text(row.summary).textSelection(.enabled)
            }.accessibilityElement(children: .combine)
        }
    }
}
struct DiagnosticContent: View {
    let events: [DiagnosticEvent]
    var body: some View {
        if events.isEmpty { Text("No recent diagnostics reported; this does not establish hardware health.").foregroundStyle(.secondary) }
        ForEach(Array(events.reversed())) { event in
            VStack(alignment: .leading) {
                Text(event.summary).font(.callout)
                Text("\(event.recovery.guidance) · reported age \(event.age_ms) ms · count \(event.count)")
                    .font(.caption).foregroundStyle(.secondary)
            }.textSelection(.enabled).accessibilityElement(children: .combine)
        }
    }
}
@MainActor private struct DeviceInspection: View {
    let device: Device
    @ObservedObject var model: ConfigurationModel
    let now: Date
    private var observation: DeviceHealth? { model.runtime?.devices.first { $0.id == device.id } }
    private var evidence: RuntimeEvidence? {
        guard let sample = model.runtime, let received = model.runtimeReceivedAt else { return nil }
        return RuntimeEvidence(snapshot: sample, receivedAt: received, now: now)
    }
    private var retained: Bool {
        model.connectedEndpoint == nil || model.stale || model.runtime?.configuration_revision != model.configuration?.revision
    }
    var body: some View {
        let legend = Dictionary(uniqueKeysWithValues: model.bindingLegend(at: now).map { ($0.key, $0.summary) })
        if let observation {
            let fresh = !retained && evidence?.fresh(observation.observed_age_ms) == true
            Text("Observed availability: \(observation.observation_supported ? observation.connectivity.rawValue : "unavailable capability")\(fresh ? "" : " · stale / unknown evidence")")
                .font(.headline).accessibilityLabel("\(device.id) availability: \(observation.observation_supported ? observation.connectivity.rawValue : "unavailable")\(fresh ? "" : ", stale or unknown")")
            if let layer = observation.layer {
                Text("Observed layer: \(layer.label)\(!retained && evidence?.fresh(layer.observed_age_ms) == true ? "" : " · stale")")
            } else { Text("Observed layer unavailable; configured mappings below are not current-layer evidence.").foregroundStyle(.secondary) }
            Text("Connection evidence age: \(evidence?.age(observation.observed_age_ms).map { "\(Int(min($0, Double(Int.max / 2)))) ms" } ?? "unknown")")
                .font(.caption)
            if let error = observation.last_error { DiagnosticContent(events: [error]) }
        } else { Text("Device observations unavailable on this daemon; configuration is not connectivity evidence.").foregroundStyle(.secondary) }
        ForEach(device.definition.keys.keys.sorted(), id: \.self) { name in
            if let control = device.definition.keys[name] {
                VStack(alignment: .leading, spacing: 4) {
                    Text(name).font(.headline)
                    ForEach(control.members, id: \.self) { address in
                        Text(observation?.member_descriptors.first { $0.address == address }?.label ?? "\(address) · physical name unavailable")
                            .font(.caption).textSelection(.enabled)
                    }
                    if let binding = control.binding {
                        Text("\(binding) → \(legend[binding] ?? "Ownership unavailable")")
                    }
                    if let action = control.action {
                        Text("\(action): \(device.definition.actions[action]?.legend ?? "Action definition unavailable")")
                        Text("Timing/output is managed read-only in the device action TOML; appearance settings cannot change it.")
                            .font(.caption).foregroundStyle(.secondary)
                    }
                    if let effectName = control.effects, let effect = device.definition.effects[effectName] {
                        DisclosureGroup("Feedback \(effectName) · host inference only") {
                            ForEach(FeedbackState.allCases, id: \.self) { state in
                                if let treatment = effect[state] { TreatmentView(label: state.rawValue, treatment: treatment) }
                                else { Text("\(state.rawValue): normal layer/base feedback").font(.caption) }
                            }
                            if let error = effect.error { Text("Actual-error cue duration: \(error.duration_ms) ms (presentation only)") }
                            if !effect.unknownStates.isEmpty { Text("Unsupported feedback states · inspect-only\n\(JSONValue.object(effect.unknownStates).formatted)") }
                        }
                    }
                    if let phase = observation?.controls.first(where: { $0.id == name }) {
                        Text("Reported phase: \(phase.phase) · pressed \(phase.pressed ? "yes" : "no")\(phase.cleanup_pending ? " · cleanup pending" : "")\(retained || evidence?.fresh() != true ? " · stale" : "")")
                            .font(.caption)
                    }
                }.padding(.vertical, 4)
            }
        }
        Text("Host gesture/posting observations never prove that the receiving app accepted a key. Appearance editing remains in Configuration.")
            .font(.caption).foregroundStyle(.secondary)
    }
}

@MainActor public struct MenuHealthView: View {
    @ObservedObject private var model: ConfigurationModel
    public init(model: ConfigurationModel) { self.model = model }
    public var body: some View {
        TimelineView(.periodic(from: .now, by: 1)) { timeline in
            ScrollView {
                VStack(alignment: .leading, spacing: 12) {
                    HealthSummaryContent(summary: model.healthSummary(at: timeline.date))
                    ConnectionButtons(model: model)
                    if let error = model.runtimeError { Text(error).font(.caption) }
                    Divider()
                    Text("Binding legend").font(.headline)
                    BindingLegendContent(rows: model.bindingLegend(at: timeline.date))
                    Divider()
                    DiagnosticContent(events: Array((model.runtime?.events ?? []).suffix(5)))
                }.padding()
            }.frame(width: 460, height: 480)
        }
    }
}

@MainActor public struct MenuHealthLabel: View {
    @ObservedObject private var model: ConfigurationModel
    public init(model: ConfigurationModel) { self.model = model }
    public var body: some View {
        TimelineView(.periodic(from: .now, by: 1)) { timeline in
            let health = model.healthSummary(at: timeline.date)
            Label("Beckon: \(health.title)", systemImage: health.symbol)
                .accessibilityLabel("Beckon health").accessibilityValue(health.text)
                .help(health.text)
        }
    }
}
