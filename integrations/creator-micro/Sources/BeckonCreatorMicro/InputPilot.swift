import Foundation
import Combine
import CryptoKit
import Darwin
import WLKit

public struct InputLayer: Equatable {
    public let id: Int
    public let name: String
    public let index: Int
    public let keymap: [[String]]
}
public struct InputProfile: Equatable {
    public let id: Int
    public let name: String
    public let index: Int
    public let layers: [InputLayer]
}
/// An immutable file snapshot, never device/layer authority or an editable Input model.
public struct InputSnapshot: Equatable {
    public let deviceID: String
    public let fingerprint: String
    public let activeProfileIndex: Int
    public let profiles: [InputProfile]
    public static let maximumBytes = 262_144

    public static func decode(_ data: Data, deviceID: String) throws -> InputSnapshot {
        guard !data.isEmpty, data.count <= maximumBytes,
              !deviceID.isEmpty, deviceID.utf8.count <= 128,
              deviceID.unicodeScalars.allSatisfy({ CharacterSet.alphanumerics.contains($0) || "_-".unicodeScalars.contains($0) }) else {
            throw InputPilotError.invalid("Invalid device ID or oversized snapshot")
        }
        let raw = try JSONDecoder().decode(RawInput.self, from: data)
        guard raw.version == 1, (1...16).contains(raw.profiles.count),
              raw.profiles.indices.contains(raw.activeProfileId),
              Set(raw.profiles.map(\.id)).count == raw.profiles.count else {
            throw InputPilotError.invalid("Unsupported Input version, profile index or duplicate profile IDs")
        }
        let profiles = try raw.profiles.enumerated().map { index, profile in
            guard profile.id >= 0, text(profile.name), (1...32).contains(profile.layers.count),
                  Set(profile.layers.map(\.id)).count == profile.layers.count else {
                throw InputPilotError.invalid("Invalid profile or duplicate layer IDs")
            }
            let layers = try profile.layers.enumerated().map { position, layer in
                guard layer.id >= 0, text(layer.name), layer.layout.keymap.map(\.count) == [2, 4, 4, 3],
                      layer.layout.keymap.flatMap({ $0 }).allSatisfy({ text($0) }) else {
                    throw InputPilotError.invalid("Invalid layer or unsupported key geometry")
                }
                return InputLayer(id: layer.id, name: layer.name, index: position, keymap: layer.layout.keymap)
            }
            return InputProfile(id: profile.id, name: profile.name, index: index, layers: layers)
        }
        return InputSnapshot(deviceID: deviceID,
            fingerprint: SHA256.hash(data: data).map { String(format: "%02x", $0) }.joined(),
            activeProfileIndex: raw.activeProfileId, profiles: profiles)
    }
    private static func text(_ value: String) -> Bool {
        !value.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty && value.utf8.count <= 128
            && !value.unicodeScalars.contains { CharacterSet.controlCharacters.contains($0) }
    }
}
private struct RawInput: Decodable {
    let version: Int
    let activeProfileId: Int
    let profiles: [RawProfile]
}
private struct RawProfile: Decodable { let id: Int; let name: String; let layers: [RawLayer] }
private struct RawLayer: Decodable { let id: Int; let name: String; let layout: RawLayout }
private struct RawLayout: Decodable { let keymap: [[String]] }

public enum InputPilotError: LocalizedError {
    case invalid(String)
    public var errorDescription: String? { if case .invalid(let reason) = self { return reason }; return nil }
}
public enum InputSnapshotReader {
    /// Read-only, nonblocking open; reject symlinks/nonregular files and detected concurrent edits.
    public static func read(_ url: URL, deviceID: String) throws -> InputSnapshot {
        guard url.isFileURL else { throw InputPilotError.invalid("Choose a local snapshot file") }
        let descriptor = Darwin.open(url.path, O_RDONLY | O_NONBLOCK | O_NOFOLLOW)
        guard descriptor >= 0 else { throw InputPilotError.invalid("Cannot open snapshot file") }
        let handle = FileHandle(fileDescriptor: descriptor, closeOnDealloc: true)
        defer { try? handle.close() }
        var before = stat(), after = stat(), currentPath = stat()
        guard fstat(descriptor, &before) == 0, before.st_mode & mode_t(S_IFMT) == mode_t(S_IFREG),
              before.st_size > 0, before.st_size <= InputSnapshot.maximumBytes else {
            throw InputPilotError.invalid("Snapshot must be a regular file of at most 256 KiB")
        }
        var data = Data()
        while data.count <= InputSnapshot.maximumBytes {
            guard let next = try handle.read(upToCount: InputSnapshot.maximumBytes + 1 - data.count), !next.isEmpty else { break }
            data.append(next)
        }
        guard fstat(descriptor, &after) == 0, lstat(url.path, &currentPath) == 0,
              before.st_dev == currentPath.st_dev, before.st_ino == currentPath.st_ino,
              before.st_size == after.st_size, data.count == before.st_size,
              before.st_mtimespec.tv_sec == after.st_mtimespec.tv_sec,
              before.st_mtimespec.tv_nsec == after.st_mtimespec.tv_nsec,
              before.st_ctimespec.tv_sec == after.st_ctimespec.tv_sec,
              before.st_ctimespec.tv_nsec == after.st_ctimespec.tv_nsec else {
            throw InputPilotError.invalid("Snapshot changed while reading; import it again")
        }
        return try InputSnapshot.decode(data, deviceID: deviceID)
    }
}
public enum InputPilotExamples {
    public static func inputSnapshot() throws -> Data { try data("input-keymap") }
    public static func renderPlan() throws -> Data { try data("render-plan") }
    private static func data(_ name: String) throws -> Data {
        guard let url = Bundle.module.url(forResource: name, withExtension: "json") else { throw InputPilotError.invalid("Missing fixture") }
        return try Data(contentsOf: url)
    }
}
public enum InputPilotLaunch {
    public static func isRequested(arguments: [String]) throws -> Bool {
        guard arguments.contains("--input-pilot") else { return false }
        guard arguments == ["--input-pilot"] else { throw InputPilotError.invalid("Input pilot accepts no live or persistence options") }
        return true
    }
}
public struct InputRenderKey: Decodable, Equatable {
    public let key: String
    public let state: String?
    public let color: String
    public let brightness: Double
    public let motion: String
}
/// Decode the existing display v1 render envelope; this fixture supplies no identity/freshness authority.
public struct InputRenderPlan: Equatable {
    public let keys: [InputRenderKey]
    public static func decode(_ data: Data) throws -> InputRenderPlan {
        guard !data.isEmpty, data.count <= InputSnapshot.maximumBytes else { throw InputPilotError.invalid("Oversized render fixture") }
        let message = try JSONDecoder().decode(RenderMessage.self, from: data)
        guard message.type == "render", message.protocol == "beckon.display", message.version == 1,
              message.plan.keys.count == 10, Set(message.plan.keys.map(\.key)) == Set((1...10).map { "f\($0)" }) else {
            throw InputPilotError.invalid("Unsupported render protocol or ambiguous logical keys")
        }
        for key in message.plan.keys {
            guard key.state == nil || ["idle", "working", "blocked", "done", "unknown"].contains(key.state!),
                  key.color.utf8.count == 7, key.color.first == "#",
                  key.color.dropFirst().allSatisfy({ $0.isASCII && $0.isHexDigit }),
                  key.brightness.isFinite, (0...1).contains(key.brightness),
                  ["steady", "breathe", "pulse", "flicker"].contains(key.motion) else {
                throw InputPilotError.invalid("Invalid render state, color, brightness or motion")
            }
        }
        return InputRenderPlan(keys: message.plan.keys)
    }
}
private struct RenderMessage: Decodable {
    let type: String
    let `protocol`: String
    let version: Int
    let sequence: UInt64
    let plan: RenderBody
}
private struct RenderBody: Decodable { let keys: [InputRenderKey] }
public struct InputPilotKey {
    public let slot: Int
    public let physicalID: Int
    public let row: Int
    public let column: Int
    public let title: String
    public let inputCode: String?
    public let treatment: InputRenderKey?
    public let enabled: Bool
}

@MainActor public final class InputPilotModel: ObservableObject {
    @Published public private(set) var snapshot: InputSnapshot?
    @Published public private(set) var profileID: Int?
    @Published public private(set) var layerID: Int?
    @Published public private(set) var render: InputRenderPlan?
    @Published public private(set) var acknowledgmentCount = 0
    @Published public private(set) var lastAcknowledgment: String?
    @Published public private(set) var error: String?
    public init() {
        do { loadRender(try InputPilotExamples.renderPlan()) }
        catch { self.error = error.localizedDescription }
    }
    public var selectedProfile: InputProfile? { snapshot?.profiles.first { $0.id == profileID } }
    public var selectedLayer: InputLayer? { selectedProfile?.layers.first { $0.id == layerID } }
    public var keys: [InputPilotKey] {
        let titles = ["Chief of Staff", "Project Coordinator", "Releases", "Worker 4", "Worker 5", "Worker 6"]
        return Pad.agentKeyIDs.enumerated().map { index, physical in
            let at = Pad.position(of: physical)!
            let treatment = index == 3 && selectedLayer != nil ? render?.keys.first { $0.key == "f4" } : nil
            return InputPilotKey(slot: index + 1, physicalID: physical, row: at.row, column: at.column,
                title: titles[index], inputCode: selectedLayer?.keymap[at.row][at.column], treatment: treatment,
                enabled: treatment?.state != nil)
        }
    }
    public func loadSnapshot(_ data: Data, deviceID: String) {
        do { apply(try InputSnapshot.decode(data, deviceID: deviceID)) }
        catch { invalidateSnapshot(error) }
    }
    public func importSnapshot(_ url: URL, deviceID: String) {
        do { apply(try InputSnapshotReader.read(url, deviceID: deviceID)) }
        catch { invalidateSnapshot(error) }
    }
    private func apply(_ candidate: InputSnapshot) {
        clearAcknowledgment()
        if snapshot?.deviceID != candidate.deviceID { profileID = nil; layerID = nil }
        snapshot = candidate; error = nil
        if selectedProfile == nil { profileID = nil; layerID = nil }
        if selectedLayer == nil { layerID = nil }
    }
    private func invalidateSnapshot(_ failure: Error) {
        snapshot = nil; profileID = nil; layerID = nil; clearAcknowledgment(); error = failure.localizedDescription
    }
    public func selectProfile(_ id: Int?) {
        profileID = snapshot?.profiles.contains { $0.id == id } == true ? id : nil
        layerID = nil; clearAcknowledgment()
    }
    public func selectLayer(_ id: Int?) {
        layerID = selectedProfile?.layers.contains { $0.id == id } == true ? id : nil
        clearAcknowledgment()
    }
    public func loadRender(_ data: Data) {
        clearAcknowledgment()
        do { render = try InputRenderPlan.decode(data); error = nil }
        catch { render = nil; self.error = error.localizedDescription }
    }
    /// A simulated local button only. No event listener, binding, focus, device or prompt action.
    public func acknowledgeFakeAG(_ name: String) -> Bool {
        guard name == "AG03", keys.contains(where: { $0.slot == 4 && $0.enabled }),
              let snapshot, let profile = selectedProfile, let layer = selectedLayer else { return false }
        acknowledgmentCount += 1
        lastAcknowledgment = "Simulated AG03 → worker 4 · device \(snapshot.deviceID) · profile \(profile.id) · layer \(layer.id)"
        return true
    }
    private func clearAcknowledgment() { acknowledgmentCount = 0; lastAcknowledgment = nil }
}
