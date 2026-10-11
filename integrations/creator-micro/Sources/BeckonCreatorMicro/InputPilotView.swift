import SwiftUI
import UniformTypeIdentifiers

/// Offline file/fixture view. No observer, configuration writer or device connection is owned here.
@MainActor public struct InputPilotView: View {
    @StateObject private var model = InputPilotModel()
    @State private var deviceID = ""
    @State private var importing = false
    @State private var sourceURL: URL?
    @State private var sourceLabel = "No snapshot imported"
    @State private var fileError: String?
    public init() {}

    public var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 20) {
                Text("Creator Micro · Input pilot").font(.largeTitle.bold())
                Text("Offline preview · one fictional worker · Input controls stay unchanged")
                    .foregroundStyle(.secondary)
                GroupBox("Input selection · read only") {
                    VStack(alignment: .leading, spacing: 12) {
                        HStack {
                            TextField("Device directory ID for import", text: $deviceID).frame(maxWidth: 280)
                            Button("Import Input JSON…") { importing = true }.disabled(deviceID.isEmpty)
                            Button("Use sample snapshot") { loadExample() }
                            if let sourceURL {
                                Button("Reload file") { readFile(sourceURL) }.disabled(deviceID.isEmpty)
                            }
                        }
                        Text(sourceLabel).font(.caption).foregroundStyle(.secondary)
                        HStack {
                            Picker("Profile", selection: Binding(get: { model.profileID }, set: { model.selectProfile($0) })) {
                                Text("Select a profile").tag(Int?.none)
                                ForEach(model.snapshot?.profiles ?? [], id: \.id) { profile in
                                    Text("\(profile.index + 1) · \(profile.name) · ID \(profile.id)").tag(Optional(profile.id))
                                }
                            }.disabled(model.snapshot == nil)
                            Picker("Layer", selection: Binding(get: { model.layerID }, set: { model.selectLayer($0) })) {
                                Text("Select a layer").tag(Int?.none)
                                ForEach(model.selectedProfile?.layers ?? [], id: \.id) { layer in
                                    Text("\(layer.index + 1) · \(layer.name) · ID \(layer.id)").tag(Optional(layer.id))
                                }
                            }.disabled(model.selectedProfile == nil)
                        }
                        if let snapshot = model.snapshot {
                            Text("Device \(snapshot.deviceID) · file profile index \(snapshot.activeProfileIndex) · hardware/current layer unverified")
                                .font(.caption).foregroundStyle(.secondary)
                            Text("Snapshot \(snapshot.fingerprint.prefix(12)) · selections use IDs, not displayed order")
                                .font(.caption).foregroundStyle(.secondary)
                        }
                    }.padding(8)
                }
                if let error = fileError ?? model.error {
                    Text("Unavailable: \(error)").foregroundStyle(.red).textSelection(.enabled)
                }
                VStack(alignment: .leading, spacing: 12) {
                    Text("Six fixed positions").font(.title2.bold())
                    HStack(spacing: 12) {
                        ForEach(model.keys.prefix(2), id: \.slot) { key in card(key) }
                    }
                    HStack(spacing: 12) {
                        ForEach(model.keys.suffix(4), id: \.slot) { key in card(key) }
                    }
                }
                HStack {
                    Button("Simulate AG03 press") { _ = model.acknowledgeFakeAG("AG03") }
                        .disabled(!model.keys.contains { $0.slot == 4 && $0.enabled })
                    Text("Local acknowledgments: \(model.acknowledgmentCount)")
                        .font(.callout).foregroundStyle(.secondary)
                    Button("Reset render fixture") {
                        do { model.loadRender(try InputPilotExamples.renderPlan()) }
                        catch { fileError = error.localizedDescription }
                    }
                }
                if let receipt = model.lastAcknowledgment {
                    Text(receipt).font(.callout).textSelection(.enabled)
                }
                Text("Status comes from a bundled Beckon render fixture, not a running agent. Blocked means reported attention; it does not identify an approval request. No physical key, light, layer switch, binding, focus or answer is sent.")
                    .font(.callout).foregroundStyle(.secondary)
                Text("Input keeps AppSense, joystick, dial, microphone multi-actions, Smart Actions, layer order, backlight and underglow.")
                    .font(.caption).foregroundStyle(.secondary)
            }.padding(24)
        }
        .frame(minWidth: 840, minHeight: 640)
        .fileImporter(isPresented: $importing, allowedContentTypes: [.json], allowsMultipleSelection: false) { result in
            switch result {
            case .success(let urls):
                if let url = urls.first { readFile(url) }
            case .failure(let error): fileError = error.localizedDescription
            }
        }
    }

    private func card(_ key: InputPilotKey) -> some View {
        let treatment = key.treatment
        let tint = color(treatment?.color)
        return VStack(alignment: .leading, spacing: 10) {
            HStack {
                Text("\(key.slot) · \(key.title)").font(.headline)
                Spacer()
                Circle().fill(tint).frame(width: 12, height: 12).accessibilityHidden(true)
            }
            Text(key.enabled ? "Fixture: \(treatment?.state ?? "unknown")" : "Unassigned · off")
                .font(.callout.weight(.medium))
            if let treatment {
                Text("\(treatment.color) · \(Int(treatment.brightness * 100))% · \(treatment.motion)")
                    .font(.caption).foregroundStyle(.secondary)
            }
            Text("Current Input code: \(key.inputCode ?? "Select a layer")").font(.caption)
            Text("Firmware key \(key.physicalID) · matrix \(key.row),\(key.column)")
                .font(.caption2).foregroundStyle(.secondary)
            if key.slot == 4 { Text("Simulated AG03 only").font(.caption) }
        }
        .padding(14).frame(maxWidth: .infinity, minHeight: 155, alignment: .topLeading)
        .background(Color.primary.opacity(0.035), in: RoundedRectangle(cornerRadius: 12))
        .overlay(RoundedRectangle(cornerRadius: 12).strokeBorder(tint.opacity(0.5)))
        .accessibilityElement(children: .combine)
    }
    private func color(_ hex: String?) -> Color {
        guard let hex, let value = UInt32(hex.dropFirst(), radix: 16) else { return .secondary }
        return Color(red: Double((value >> 16) & 255) / 255,
                     green: Double((value >> 8) & 255) / 255, blue: Double(value & 255) / 255)
    }
    private func loadExample() {
        deviceID = "33432"; sourceURL = nil; sourceLabel = "Bundled sample · actual Input shape, fictional worker"
        do { model.loadSnapshot(try InputPilotExamples.inputSnapshot(), deviceID: deviceID); fileError = nil }
        catch { fileError = error.localizedDescription }
    }
    private func readFile(_ url: URL) {
        let scoped = url.startAccessingSecurityScopedResource()
        defer { if scoped { url.stopAccessingSecurityScopedResource() } }
        sourceURL = url; sourceLabel = "Imported file · \(url.lastPathComponent)"
        model.importSnapshot(url, deviceID: deviceID); fileError = nil
    }
}
