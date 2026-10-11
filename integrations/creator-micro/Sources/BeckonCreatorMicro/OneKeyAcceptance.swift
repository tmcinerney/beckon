import Foundation
import CoreFoundation
import WLKit

/// Immutable snapshot from this run's verified USB transfer; display/import state is separate.
struct OneKeyVerifiedScope {
    private let deviceSnapshot:InputSnapshot
    let generation:UInt64
    init(deviceSnapshot:InputSnapshot,generation:UInt64) {self.deviceSnapshot=deviceSnapshot;self.generation=generation}
    func resolve(status:[String:Any],generation:UInt64) throws -> OneKeySlotScope {
        guard self.generation==generation else {throw InputPilotError.invalid("Verified device snapshot retired")}
        return try OneKeySlotScope.resolve(snapshot:deviceSnapshot,status:status)
    }
}

/// Explicit short human-owned acceptance; not production coexistence or firmware proof.
struct OneKeySlotScope:Equatable {
    let profileIndex:Int,layerIndex:Int
    static func integer(_ raw:Any?) -> Int? {
        guard let n=raw as? NSNumber,CFGetTypeID(n) != CFBooleanGetTypeID(),n.doubleValue.isFinite,
              (0...31).contains(n.intValue),n.doubleValue==Double(n.intValue) else {return nil}
        return n.intValue
    }
    static func resolve(snapshot:InputSnapshot,status:[String:Any]) throws -> OneKeySlotScope {
        guard status["version"] as? String=="0.6.3",
              let pi=integer(status["profile_index"]),let li=integer(status["layer_index"]),
              pi==snapshot.activeProfileIndex,snapshot.profiles.indices.contains(pi) else {
            throw InputPilotError.invalid("Acceptance profile/firmware mismatch")
        }
        let profile=snapshot.profiles[pi]
        guard profile.id==2,profile.layers.indices.contains(li),profile.layers[li].id==0,
              profile.layers[li].keymap[1][1]=="KV_OAI_AG03" else {
            throw InputPilotError.invalid("Only profile2 Herdr layer0 slot4 AG03 may be armed")
        }
        return .init(profileIndex:pi,layerIndex:li)
    }
    func matches(_ status:Any?) -> Bool {
        guard let status=status as? [String:Any] else {return false}
        return status["version"] as? String=="0.6.3" && Self.integer(status["profile_index"])==profileIndex && Self.integer(status["layer_index"])==layerIndex
    }
}

@MainActor final class OneKeyAcceptance {
    let scope:OneKeySlotScope,generation:UInt64,deadline:Double
    private let port:OneKeyDevicePort,clock:()->Double
    private var stopped=false,busy=false,painted=false
    private(set) var cleanupAcknowledged=false
    static let cleanupReserve=10.0
    static let focusUnavailable="Focus disabled: installed Herdr lacks atomic expected-terminal/native-agent focus"
    init(port:OneKeyDevicePort,scope:OneKeySlotScope,allowed:Bool,deadline:Double,clock:@escaping ()->Double) throws {
        let effectiveDeadline=min(deadline,port.transportDeadline-Self.cleanupReserve)
        guard allowed,clock().isFinite,effectiveDeadline.isFinite,effectiveDeadline>clock(),
              effectiveDeadline<=clock()+60,port.remainingRequests>2 else {throw InputPilotError.invalid("Explicit one-key acceptance authorization required")}
        self.port=port;self.scope=scope;self.clock=clock;generation=port.generation;self.deadline=effectiveDeadline
    }
    private func check() throws {
        guard !stopped,clock().isFinite,clock()<deadline,port.remainingRequests>2,port.generation==generation else {throw InputPilotError.invalid("Slot4 acceptance retired or expired")}
        try Task.checkCancellation()
    }
    private func verifyLayer() async throws {
        try check();let status=try await port.read("device.status");try check()
        guard scope.matches(status) else {throw InputPilotError.invalid("Layer/profile changed; slot4 acceptance stopped")}
    }
    func update(_ render:()->OneKeyFrame.Render?) async throws {
        guard !busy else {return};busy=true;defer {busy=false}
        try await verifyLayer()
        let record=try render().map(OneKeyLED.record) ?? OneKeyLED.cleanup
        try check();painted=true;try await port.paint(record);try check()
    }
    func acceptEvent(_ receipt:AG03Receipt) async throws -> String {
        guard receipt.connectionGeneration==generation else {throw InputPilotError.invalid("Retired AG03 event")}
        try await verifyLayer()
        // No read-then-unfenced focus fallback; this server contract is genuinely missing.
        return Self.focusUnavailable
    }
    func stop() async {
        stopped=true
        let drain=ProcessInfo.processInfo.systemUptime+2
        while busy,ProcessInfo.processInfo.systemUptime<drain {try? await Task.sleep(nanoseconds:10_000_000)}
        guard !busy,port.generation==generation else {return}
        // Skip cleanup on an observed foreign layer. Status is not apply-time authority;
        // the human must keep Herdr selected throughout this exclusive short run.
        if painted,let status=try? await port.cleanupStatus(),scope.matches(status),port.generation==generation {
            do {try await port.cleanupPaint();cleanupAcknowledged=true} catch {}
        }
        // Input owner restores the one-cell binding/appearance; ACK is not restoration proof.
    }
}
