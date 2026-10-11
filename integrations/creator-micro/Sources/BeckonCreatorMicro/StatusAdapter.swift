import Foundation
import BeckonPanelModel
import WLKit

public struct SlotLED: Sendable {
    public let slot: Int
    public let presentation: ActivityPresentation?
    public let unavailableReason: String?
    public let thread: OAI.Thread
}

/// A proposed frame, never a device write or native-session authority receipt.
public struct StatusFrame: Sendable {
    public let slots: [SlotLED]
    public var threads: [OAI.Thread] { slots.map(\.thread) }
    public let focusAvailable = false
    public let liveWriterAvailable = false
    public func encodedParams() throws -> Data {
        try JSONSerialization.data(withJSONObject: OAI.threadsParams(threads), options: [.sortedKeys])
    }
}

public enum StatusAdapter {
    private static func off(slot: Int, id: Int, reason: String) -> SlotLED {
        SlotLED(slot: slot, presentation: nil, unavailableReason: reason,
                thread: OAI.Thread(id: id, color: 0, brightness: 0, effect: .off))
    }
    private static func offFrame(_ reason: String) -> StatusFrame {
        StatusFrame(slots: Pad.agentKeyIDs.enumerated().map { off(slot: $0, id: $1, reason: reason) })
    }

    // AIDEV-NOTE: Fresh Herdr reports are observational status, not authority to focus or approve.
    // No transport is owned here; even an off frame must not be sent after layer/owner revocation.
    public static func prepare(configuration: PanelConfiguration, snapshot: AgentSnapshot?,
                               directory: CoordinatorSnapshot? = nil, now: Double) -> StatusFrame {
        do { try configuration.validate() }
        catch { return offFrame(error.localizedDescription) }
        guard let snapshot else { return offFrame("Disconnected / no observation") }
        guard snapshot.source == configuration.source else { return offFrame("Wrong observation source") }
        guard now.isFinite, snapshot.observedAt.isFinite, now >= snapshot.observedAt,
              now - snapshot.observedAt <= 10 else { return offFrame("Stale or invalid observation") }
        return StatusFrame(slots: Pad.agentKeyIDs.enumerated().map { slot, id in
            do {
                let record = try SlotResolver.resolve(slot: slot, configuration: configuration,
                    snapshot: snapshot, directory: directory, now: now)
                let presentation = record.presentation
                let color = Int(presentation.color.dropFirst(), radix: 16) ?? 0x808080
                return SlotLED(slot: slot, presentation: presentation, unavailableReason: nil,
                    thread: OAI.Thread(id: id, color: color, brightness: 0.5,
                        effect: presentation.pulse ? .breath : .solid, speed: 0.5))
            } catch {
                return off(slot: slot, id: id, reason: error.localizedDescription)
            }
        })
    }
}
