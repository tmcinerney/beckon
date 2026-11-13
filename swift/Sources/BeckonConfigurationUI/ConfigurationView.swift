import SwiftUI
import BeckonConfiguration

@MainActor public struct ConfigurationView: View {
    @ObservedObject private var model: ConfigurationModel
    @State private var theme = ""
    @State private var key = ""
    @State private var agent: AgentIdentity?
    private struct ReleaseSelection {
        let key: String
        let reference: AgentIdentity
        let endpoint: String
    }
    @State private var releaseSelection: ReleaseSelection?
    public init(model: ConfigurationModel) { self.model = model }

    public var body: some View {
        TimelineView(.periodic(from: .now, by: 1)) { timeline in
            Form {
                connection(now: timeline.date)
                if let configuration = model.configuration {
                    appearance(configuration, editable: model.canMutate && model.isFresh(at: timeline.date))
                    bindings(configuration, editable: model.canMutate && model.isFresh(at: timeline.date))
                    Section("Devices and interaction feedback") {
                        Text("Configured definitions, not live hardware observations. Held and latched feedback is inferred by the daemon, not receiver state.")
                            .foregroundStyle(.secondary)
                        if configuration.devices.isEmpty { Text("No devices configured.") }
                        ForEach(configuration.devices) { device in
                            DeviceView(device: device, config: configuration, model: model,
                                       editable: model.canMutate && model.isFresh(at: timeline.date))
                        }
                    }
                } else {
                    Section { Text("Connect to load the daemon’s configuration and agents.") }
                }
            }
            .formStyle(.grouped)
        }
        .confirmationDialog("Release binding \(releaseSelection?.key ?? "")?", isPresented: Binding(
            get: { releaseSelection != nil }, set: { if !$0 { releaseSelection = nil } }
        )) {
            if let selected = releaseSelection {
                Button("Release \(selected.key)", role: .destructive) {
                    Task { await model.release(key: selected.key, reference: selected.reference, endpoint: selected.endpoint) }
                    releaseSelection = nil
                }
            }
            Button("Cancel", role: .cancel) { releaseSelection = nil }
        } message: {
            Text("Release only \(releaseSelection?.reference.description ?? "the selected owner"). A changed owner will be rejected by the daemon.")
        }
    }
    private func connection(now: Date) -> some View {
        Section("Connection") {
            TextField("Beckon Unix socket", text: $model.endpoint)
                .textFieldStyle(.roundedBorder).accessibilityLabel("Beckon Unix socket path")
            HStack {
                Button("Use discovered endpoint") { model.endpoint = Endpoint.discover() }.disabled(model.busy)
                Button(model.connectedEndpoint == nil ? "Connect" : "Reconnect") { Task { await model.connect() } }
                    .disabled(model.busy)
                Button("Disconnect") { model.disconnect() }.disabled(model.connectedEndpoint == nil)
                Button("Refresh") { Task { await model.refresh() } }.disabled(model.busy || model.connectedEndpoint == nil)
                if model.busy { ProgressView().controlSize(.small).accessibilityLabel("Contacting Beckon") }
            }
            Text(model.health(at: now)).accessibilityLabel("Connection state: \(model.health(at: now))")
            if let path = model.connectedEndpoint { Text("Active endpoint: \(path)").font(.caption).textSelection(.enabled) }
            if let date = model.lastUpdated { Text("Last complete snapshot: \(date.formatted())").font(.caption) }
            if model.endpoint != model.connectedEndpoint, model.connectedEndpoint != nil {
                Text("Endpoint edited. Reconnect to apply it; changes are disabled.").foregroundStyle(.orange)
            }
            if let error = model.error { Text(error).foregroundStyle(.red).textSelection(.enabled).accessibilityLabel("Error: \(error)") }
            if let notice = model.notice { Text(notice).foregroundStyle(.secondary) }
        }
    }
    private func appearance(_ config: Configuration, editable: Bool) -> some View {
        Section("Appearance") {
            Text("Active theme: \(config.display.theme)").font(.headline)
            Picker("Theme", selection: $theme) {
                Text("Choose a theme").tag("")
                ForEach(config.display.themes.keys.sorted(), id: \.self) { Text($0).tag($0) }
            }.accessibilityLabel("Configured theme")
            Button("Apply theme") { Task { await model.setTheme(theme) } }
                .disabled(!editable || config.display.themes[theme] == nil || theme == config.display.theme)
            Text(config.supportsPresentation("theme_state")
                 ? "Edit a treatment, then Save. Unsaved drafts reset when the configuration revision changes."
                 : "This daemon does not advertise theme treatment editing. Local previews cannot be saved.")
                .font(.caption).foregroundStyle(.secondary)
            if let treatments = config.display.themes[config.display.theme] {
                ForEach(treatments.keys.sorted(), id: \.self) { state in
                    if let treatment = treatments[state] {
                        TreatmentView(label: state, treatment: treatment)
                        DisclosureGroup("Edit \(state) appearance") {
                            TreatmentEditor(treatment: treatment, label: "\(state) appearance",
                                            supported: config.supportsPresentation("theme_state") && ThemeState(rawValue: state) != nil,
                                            editable: editable) { draft in
                                if let revision = config.revision, let state = ThemeState(rawValue: state) {
                                    Task { await model.setPresentation(.themeState(theme: config.display.theme, state: state, treatment: draft), expectedRevision: revision) }
                                }
                            }.id("\(config.revision ?? "legacy")-\(config.display.theme)-\(state)-\(treatment)")
                        }
                    }
                }
            }
        }
    }
    private func bindings(_ config: Configuration, editable: Bool) -> some View {
        Section("Agent bindings") {
            Text("Choose an exact source, scope and agent, then bind explicitly. Existing owners are never replaced automatically.")
                .foregroundStyle(.secondary)
            Picker("Configured binding key", selection: $key) {
                Text("Choose a key").tag("")
                ForEach(config.bindings, id: \.self) { Text($0).tag($0) }
            }.accessibilityLabel("Configured binding key")
            Picker("Agent", selection: $agent) {
                Text("Choose an agent").tag(nil as AgentIdentity?)
                ForEach(model.status?.panes ?? []) { pane in
                    Text("\(pane.title) — \(pane.id.description)").tag(Optional(pane.id))
                }
            }.accessibilityLabel("Source-qualified agent")
            if let agent { Text(agent.description).font(.caption).textSelection(.enabled) }
            Button("Bind selected agent") {
                if let agent { Task { await model.bind(key: key, agent: agent) } }
            }.disabled(!editable || !config.bindings.contains(key) || agent == nil)
            ForEach(config.bindings, id: \.self) { bindingKey in
                HStack {
                    Text(bindingKey).font(.headline)
                    if let binding = model.status?.bindings.first(where: { $0.key == bindingKey }) {
                        VStack(alignment: .leading) {
                            Text(model.status?.panes.first(where: {
                                $0.source == binding.source && $0.session == binding.session && $0.pane_id == binding.pane.pane_id
                            })?.title ?? binding.pane.pane_id)
                            Text("\(binding.source) · \(binding.session) · \(binding.pane.pane_id) · \(binding.pane.agent_status)")
                                .font(.caption).textSelection(.enabled)
                        }
                    } else {
                        Text("No live owner reported").foregroundStyle(.secondary)
                    }
                    Spacer()
                    if let binding = model.status?.bindings.first(where: { $0.key == bindingKey }), let endpoint = model.connectedEndpoint {
                        Button("Release…") {
                            releaseSelection = ReleaseSelection(key: bindingKey, reference: binding.reference, endpoint: endpoint)
                        }.disabled(!editable || !model.canRelease).accessibilityLabel("Release binding \(bindingKey)")
                    }
                }
            }
            Text("Status lists live bindings only. A missing owner can be dormant; the daemon remains authoritative and rejects occupied keys. UI release requires an observed exact owner and never falls back to release_key.")
                .font(.caption).foregroundStyle(.secondary)
            if config.capabilities?.binding_release_guard != true {
                Text("Guarded release is unavailable on this daemon.").font(.caption).foregroundStyle(.secondary)
            }
        }
    }
}

struct TreatmentView: View {
    let label: String
    let treatment: Treatment
    var body: some View {
        HStack {
            Text(label).frame(minWidth: 80, alignment: .leading)
            Text(treatment.color).monospaced()
            Spacer()
            Text("Brightness \(treatment.brightness, format: .percent.precision(.fractionLength(0)))")
            Text(treatment.motion.rawValue)
        }.accessibilityElement(children: .combine)
    }
}
private struct DeviceView: View {
    let device: Device
    let config: Configuration
    @ObservedObject var model: ConfigurationModel
    let editable: Bool
    var body: some View {
        DisclosureGroup("\(device.id) · \(device.definition.adapter)") {
            VStack(alignment: .leading, spacing: 12) {
                Text(device.path).font(.caption).textSelection(.enabled)
                ForEach(device.definition.keys.keys.sorted(), id: \.self) { name in
                    if let key = device.definition.keys[name] {
                        VStack(alignment: .leading) {
                            Text(name).font(.headline)
                            Text(key.members.joined(separator: ", ")).textSelection(.enabled)
                            if let binding = key.binding { Text("Agent binding: \(binding)") }
                            if let action = key.action, let definition = device.definition.actions[action] {
                                Text("Action \(action): \(definition.keys.joined(separator: " + ")) · \(definition.mode)")
                                if let ms = definition.tap_ms { Text("Tap threshold: \(ms) ms") }
                            }
                            if let effect = key.effects { Text("Feedback: \(effect)") }
                        }
                    }
                }
                ForEach(device.definition.effects.keys.sorted(), id: \.self) { name in
                    if let effect = device.definition.effects[name] {
                        Text("Feedback \(name)").font(.headline)
                        ForEach(["idle", "held", "latched"], id: \.self) { state in
                            if let treatment = effect[state] { TreatmentView(label: state, treatment: treatment) }
                            else { Text("\(state): normal binding/base rendering").foregroundStyle(.secondary) }
                        }
                        DisclosureGroup("Edit feedback \(name)") {
                            EffectEditor(effects: effect, label: "\(device.id) \(name)",
                                         supported: config.supportsPresentation("device_effect"), editable: editable) { draft in
                                if let revision = config.revision {
                                    Task { await model.setPresentation(.deviceEffect(device: device.id, effect: name, effects: draft), expectedRevision: revision) }
                                }
                            }.id("\(config.revision ?? "legacy")-\(device.id)-\(name)")
                        }
                    }
                }
                ForEach(config.capabilities?.device_channels[device.id] ?? [], id: \.self) { channel in
                    if let treatment = device.definition.channelTreatment(channel) {
                        TreatmentView(label: channel, treatment: treatment)
                        DisclosureGroup("Edit \(channel) channel") {
                            TreatmentEditor(treatment: treatment, label: "\(device.id) \(channel)",
                                            supported: config.supportsPresentation("device_channel"), editable: editable) { draft in
                                if let revision = config.revision {
                                    Task { await model.setPresentation(.deviceChannel(device: device.id, channel: channel, treatment: draft), expectedRevision: revision) }
                                }
                            }.id("\(config.revision ?? "legacy")-\(device.id)-\(channel)")
                        }
                    } else { Text("\(channel): effective treatment unavailable; editing disabled.").foregroundStyle(.secondary) }
                }
                DisclosureGroup("Adapter presentation and layer options (read-only)") {
                    Text(JSONValue.object(device.definition.options).formatted)
                        .font(.system(.caption, design: .monospaced)).textSelection(.enabled)
                }
            }.padding(.vertical, 8)
        }
    }
}
