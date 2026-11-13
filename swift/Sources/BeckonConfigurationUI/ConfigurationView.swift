import SwiftUI
import BeckonConfiguration

@MainActor public struct ConfigurationView: View {
    @ObservedObject private var model: ConfigurationModel
    @State private var theme = ""
    @State private var key = ""
    @State private var agent: AgentIdentity?
    @State private var releaseKey: String?
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
                        ForEach(configuration.devices) { device in DeviceView(device: device) }
                    }
                } else {
                    Section { Text("Connect to load the daemon’s configuration and agents.") }
                }
            }
            .formStyle(.grouped)
        }
        .confirmationDialog("Release binding \(releaseKey ?? "")?", isPresented: Binding(
            get: { releaseKey != nil }, set: { if !$0 { releaseKey = nil } }
        )) {
            if let selected = releaseKey {
                Button("Release \(selected)", role: .destructive) { Task { await model.release(key: selected) }; releaseKey = nil }
            }
            Button("Cancel", role: .cancel) { releaseKey = nil }
        } message: {
            Text("Releases the current owner of this key. The daemon’s release_key API does not support an expected-owner check.")
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
            Text("Colour, brightness and animation editing requires daemon appearance mutation support. Current values are read-only.")
                .font(.caption).foregroundStyle(.secondary)
            if let treatments = config.display.themes[config.display.theme] {
                ForEach(treatments.keys.sorted(), id: \.self) { state in
                    if let treatment = treatments[state] {
                        TreatmentView(label: state, treatment: treatment)
                        DisclosureGroup("Preview \(state) appearance") {
                            TreatmentEditor(treatment: treatment)
                                .id("\(config.display.theme)-\(state)-\(treatment)")
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
                    Button("Release…") { releaseKey = bindingKey }
                        .disabled(!editable).accessibilityLabel("Release binding \(bindingKey)")
                }
            }
            Text("Status lists live bindings only. A missing owner can be dormant; the daemon remains authoritative and rejects occupied keys.")
                .font(.caption).foregroundStyle(.secondary)
        }
    }
}

// Local draft only until the daemon advertises typed appearance mutation capabilities.
private struct TreatmentEditor: View {
    @State private var draft: Treatment
    init(treatment: Treatment) { _draft = State(initialValue: treatment) }
    private var color: Binding<Color> {
        Binding(get: {
            let value = UInt32(draft.color.dropFirst(), radix: 16) ?? 0
            return Color(.sRGB, red: Double((value >> 16) & 255) / 255,
                         green: Double((value >> 8) & 255) / 255, blue: Double(value & 255) / 255, opacity: 1)
        }, set: { value in
            guard let rgb = NSColor(value).usingColorSpace(.sRGB) else { return }
            draft.color = String(format: "#%02X%02X%02X", Int((rgb.redComponent * 255).rounded()),
                                 Int((rgb.greenComponent * 255).rounded()), Int((rgb.blueComponent * 255).rounded()))
        })
    }
    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text("Local preview only · not saved or sent to a device").font(.caption).foregroundStyle(.secondary)
            ColorPicker("Colour", selection: color, supportsOpacity: false).accessibilityLabel("Preview colour")
            TextField("Hex colour", text: $draft.color).accessibilityLabel("Preview hex colour")
            Slider(value: $draft.brightness, in: 0...1) { Text("Brightness") }
                .accessibilityLabel("Preview brightness")
            Picker("Animation", selection: $draft.motion) {
                ForEach(Motion.allCases, id: \.self) { Text($0.rawValue).tag($0) }
            }.accessibilityLabel("Preview animation")
            TreatmentView(label: "Draft", treatment: draft)
            if (try? draft.validate()) == nil { Text("Enter a valid #RRGGBB colour.").foregroundStyle(.red) }
            Button("Save appearance (unavailable)") {}.disabled(true)
                .help("The connected protocol has no typed treatment mutation endpoint yet.")
        }
    }
}

private struct TreatmentView: View {
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
                    }
                }
                DisclosureGroup("Adapter presentation and layer options (read-only)") {
                    Text(JSONValue.object(device.definition.options).formatted)
                        .font(.system(.caption, design: .monospaced)).textSelection(.enabled)
                }
            }.padding(.vertical, 8)
        }
    }
}
