import SwiftUI
import BeckonConfiguration

struct TreatmentEditor: View {
    @State private var draft: Treatment
    let original: Treatment
    let label: String
    let supported: Bool
    let editable: Bool
    let save: (Treatment) -> Void

    init(treatment: Treatment, label: String, supported: Bool, editable: Bool, save: @escaping (Treatment) -> Void) {
        _draft = State(initialValue: treatment); original = treatment
        self.label = label; self.supported = supported; self.editable = editable; self.save = save
    }
    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text(supported ? "Draft · changes reach the daemon only when saved" : "Local preview only · daemon does not advertise this capability")
                .font(.caption).foregroundStyle(.secondary)
            TreatmentFields(draft: $draft, label: label)
            HStack {
                Button("Save appearance") { save(draft) }
                    .disabled(!supported || !editable || draft == original || (try? draft.validate()) == nil)
                    .accessibilityLabel("Save \(label)")
                Button("Discard draft") { draft = original }.disabled(draft == original)
                    .accessibilityLabel("Discard \(label) draft")
            }
        }
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
    @State private var draft: Feedback
    let original: Feedback
    let label: String
    let supported: Bool
    let editable: Bool
    let save: (Feedback) -> Void
    // Visible draft seed when an absent state is explicitly enabled; never sent until Save.
    private let seed = Treatment(color: "#FFFFFF", brightness: 1, motion: .steady)

    init(effects: Feedback, label: String, supported: Bool, editable: Bool, save: @escaping (Feedback) -> Void) {
        _draft = State(initialValue: effects); original = effects
        self.label = label; self.supported = supported; self.editable = editable; self.save = save
    }
    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Save replaces all three feedback states together. Disabled states return to normal binding/base rendering.")
                .font(.caption).foregroundStyle(.secondary)
            if !supported { Text("Local preview only · daemon does not advertise effect editing.").font(.caption) }
            ForEach(FeedbackState.allCases, id: \.self) { state in
                Toggle("\(state.rawValue) treatment", isOn: Binding(
                    get: { draft[state] != nil }, set: { draft[state] = $0 ? (draft[state] ?? seed) : nil }
                )).accessibilityLabel("\(label) \(state.rawValue) treatment enabled")
                if draft[state] != nil {
                    TreatmentFields(draft: Binding(get: { draft[state] ?? seed }, set: { draft[state] = $0 }),
                                    label: "\(label) \(state.rawValue)")
                }
            }
            if (try? draft.validate()) == nil { Text("Keep at least one valid feedback treatment.").foregroundStyle(.red) }
            HStack {
                Button("Save feedback") { save(draft) }
                    .disabled(!supported || !editable || draft == original || (try? draft.validate()) == nil)
                    .accessibilityLabel("Save \(label) feedback")
                Button("Discard draft") { draft = original }.disabled(draft == original)
                    .accessibilityLabel("Discard \(label) feedback draft")
            }
        }
    }
}
