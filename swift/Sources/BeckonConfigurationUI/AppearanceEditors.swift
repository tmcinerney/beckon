import SwiftUI
import BeckonConfiguration

struct TreatmentEditor: View {
    @State private var draft: AppearanceDraft<Treatment>
    @State private var saving = false
    let current: Treatment
    let revision: String?
    let label: String
    let supported: Bool
    let editable: Bool
    let save: (Treatment, String) async -> (Treatment, String)?

    init(treatment: Treatment, revision: String?, label: String, supported: Bool, editable: Bool,
         save: @escaping (Treatment, String) async -> (Treatment, String)?) {
        _draft = State(initialValue: AppearanceDraft(treatment, revision: revision)); current = treatment; self.revision = revision
        self.label = label; self.supported = supported; self.editable = editable; self.save = save
    }
    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text(supported ? "Draft · changes reach the daemon only when saved" : "Local preview only · daemon does not advertise this capability")
                .font(.caption).foregroundStyle(.secondary)
            TreatmentFields(draft: $draft.value, label: label)
            if !draft.matches(revision) {
                Text("Configuration changed. Your draft is retained. Review the current saved value before reconciling.")
                    .foregroundStyle(.orange)
                TreatmentView(label: "Current", treatment: current)
                Button("Keep draft against current revision") { draft.reconcile(using: current, revision: revision) }
                    .disabled(!editable).accessibilityLabel("Reconcile \(label) draft")
            }
            HStack {
                Button("Save appearance") {
                    guard let expectedRevision = draft.revision else { return }
                    let value = draft.value
                    saving = true
                    Task {
                        defer { saving = false }
                        if let (canonical, revision) = await save(value, expectedRevision) {
                            draft.discard(using: canonical, revision: revision)
                        }
                    }
                }
                    .disabled(!supported || !editable || !draft.hasChanges || !draft.matches(revision) || (try? draft.value.validate()) == nil)
                    .accessibilityLabel("Save \(label)")
                Button("Discard draft / use current") { draft.discard(using: current, revision: revision) }
                    .disabled(!draft.hasChanges && draft.matches(revision))
                    .accessibilityLabel("Discard \(label) draft")
            }
        }.disabled(saving)
    }
}

struct TreatmentFields: View {
    @Binding var draft: Treatment
    let label: String
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
        VStack(alignment: .leading, spacing: 8) {
            ColorPicker("Colour", selection: color, supportsOpacity: false).accessibilityLabel("\(label) colour")
            TextField("Hex colour", text: $draft.color).accessibilityLabel("\(label) hex colour")
            Slider(value: $draft.brightness, in: 0...1) { Text("Brightness") }
                .accessibilityLabel("\(label) brightness")
            Picker("Animation", selection: $draft.motion) {
                ForEach(Motion.allCases, id: \.self) { Text($0.rawValue).tag($0) }
            }.accessibilityLabel("\(label) animation")
            TreatmentView(label: "Draft", treatment: draft)
            if (try? draft.validate()) == nil { Text("Use #RRGGBB and brightness between 0 and 1.").foregroundStyle(.red) }
        }
    }
}

struct EffectEditor: View {
    @State private var draft: AppearanceDraft<Feedback>
    @State private var saving = false
    let current: Feedback
    let revision: String?
    let label: String
    let supported: Bool
    let editable: Bool
    let save: (Feedback, String) async -> (Feedback, String)?
    // Visible draft seed when an absent state is explicitly enabled; never sent until Save.
    private let seed = Treatment(color: "#FFFFFF", brightness: 1, motion: .steady)

    init(effects: Feedback, revision: String?, label: String, supported: Bool, editable: Bool,
         save: @escaping (Feedback, String) async -> (Feedback, String)?) {
        _draft = State(initialValue: AppearanceDraft(effects, revision: revision)); current = effects; self.revision = revision
        self.label = label; self.supported = supported; self.editable = editable; self.save = save
    }
    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Save replaces all three feedback states together. Disabled states return to normal binding/base rendering.")
                .font(.caption).foregroundStyle(.secondary)
            if !supported { Text("Local preview only · daemon does not advertise effect editing.").font(.caption) }
            ForEach(FeedbackState.allCases, id: \.self) { state in
                Toggle("\(state.rawValue) treatment", isOn: Binding(
                    get: { draft.value[state] != nil }, set: { draft.value[state] = $0 ? (draft.value[state] ?? seed) : nil }
                )).accessibilityLabel("\(label) \(state.rawValue) treatment enabled")
                if draft.value[state] != nil {
                    TreatmentFields(draft: Binding(get: { draft.value[state] ?? seed }, set: { draft.value[state] = $0 }),
                                    label: "\(label) \(state.rawValue)")
                }
            }
            if (try? draft.value.validate()) == nil { Text("Keep at least one valid feedback treatment.").foregroundStyle(.red) }
            if !draft.matches(revision) {
                Text("Configuration changed. Your draft is retained; review all current feedback states before reconciling.")
                    .foregroundStyle(.orange)
                ForEach(FeedbackState.allCases, id: \.self) { state in
                    if let treatment = current[state] { TreatmentView(label: "Current \(state.rawValue)", treatment: treatment) }
                    else { Text("Current \(state.rawValue): normal binding/base rendering") }
                }
                Button("Keep draft against current revision") { draft.reconcile(using: current, revision: revision) }
                    .disabled(!editable).accessibilityLabel("Reconcile \(label) feedback draft")
            }
            HStack {
                Button("Save feedback") {
                    guard let expectedRevision = draft.revision else { return }
                    let value = draft.value
                    saving = true
                    Task {
                        defer { saving = false }
                        if let (canonical, revision) = await save(value, expectedRevision) {
                            draft.discard(using: canonical, revision: revision)
                        }
                    }
                }
                    .disabled(!supported || !editable || !draft.hasChanges || !draft.matches(revision) || (try? draft.value.validate()) == nil)
                    .accessibilityLabel("Save \(label) feedback")
                Button("Discard draft / use current") { draft.discard(using: current, revision: revision) }
                    .disabled(!draft.hasChanges && draft.matches(revision))
                    .accessibilityLabel("Discard \(label) feedback draft")
            }
        }.disabled(saving)
    }
}
