import Foundation
import CoreFoundation

/// SDK transaction flow for one file only. No reset, import, implicit retry or invented abort.
@MainActor enum OneKeyConfigTransfer {
    struct Result {let patch:OneKeyConfigPatch,readback:Data,restored:Bool}
    struct Failure:LocalizedError {
        let cause:String,liveOutcome:String,readbackSHA256:String?
        var errorDescription:String? {"\(cause); liveOutcome=\(liveOutcome); no automatic retry/rollback; durable backup retained"}
    }
    private static func classify(_ error:Error,patch:OneKeyConfigPatch,deadline:Double,clock:()->Double,valid:()->Bool,
                                 journal:(String) throws -> Void,call:(String,[String:Any]) async throws -> Any?) async -> Failure {
        var outcome="unreadable",hash:String?
        do {
            let bytes=try await OneKeyFileReader.read(deadline:deadline,clock:clock,valid:valid,call:call).bytes
            hash=OneKeyConfigPatch.hash(bytes)
            outcome=try patch.matchesOriginal(bytes) ? "original_verified":(patch.matchesCandidate(bytes) ? "candidate_verified":"divergent")
        } catch {}
        try? journal("failure_live_\(outcome)")
        return Failure(cause:String(error.localizedDescription.prefix(512)),liveOutcome:outcome,readbackSHA256:hash)
    }
    static func apply(deviceID:String,deadline:Double,clock:()->Double,valid:()->Bool,
                      persist:(OneKeyConfigPatch) throws -> Void,journal:(String) throws -> Void,
                      call:(String,[String:Any]) async throws -> Any?) async throws -> Result {
        let original=try await OneKeyFileReader.read(deadline:deadline,clock:clock,valid:valid,call:call).bytes
        let patch=try OneKeyConfigPatch.make(original:original,deviceID:deviceID)
        try persist(patch) // MUST durably retain exact original/candidate before any mutating RPC.
        do {
            let readback=try await transfer(payload:patch.candidate,deadline:deadline,clock:clock,valid:valid,journal:journal,call:call)
            _ = try InputSnapshot.decode(readback,deviceID:deviceID)
            guard try patch.matchesCandidate(readback) else {throw InputPilotError.invalid("Committed keymap readback differs from exact candidate")}
            try journal("applied_verified")
            return Result(patch:patch,readback:readback,restored:false)
        } catch {throw await classify(error,patch:patch,deadline:deadline,clock:clock,valid:valid,journal:journal,call:call)}
    }
    static func restore(patch:OneKeyConfigPatch,deviceID:String,deadline:Double,clock:()->Double,valid:()->Bool,
                        journal:(String) throws -> Void,call:(String,[String:Any]) async throws -> Any?) async throws -> Result {
        let current=try await OneKeyFileReader.read(deadline:deadline,clock:clock,valid:valid,call:call).bytes
        if try patch.matchesOriginal(current) {try journal("already_original_verified");return Result(patch:patch,readback:current,restored:true)}
        let payload=try patch.restorePayload(current:current)
        do {
            let readback=try await transfer(payload:payload,deadline:deadline,clock:clock,valid:valid,journal:journal,call:call)
            _ = try InputSnapshot.decode(readback,deviceID:deviceID)
            guard try patch.matchesOriginal(readback) else {throw InputPilotError.invalid("Restore readback differs from original")}
            try journal("restored_verified")
            return Result(patch:patch,readback:readback,restored:true)
        } catch {throw await classify(error,patch:patch,deadline:deadline,clock:clock,valid:valid,journal:journal,call:call)}
    }
    private static func transfer(payload:Data,deadline:Double,clock:()->Double,valid:()->Bool,
                                 journal:(String) throws -> Void,call:(String,[String:Any]) async throws -> Any?) async throws -> Data {
        func check() throws {
            try Task.checkCancellation()
            guard valid(),clock().isFinite,deadline.isFinite,clock()<deadline else {throw InputPilotError.invalid("Configuration transaction retired/expired; no automatic retry")}
        }
        func invoke(_ method:String,_ params:[String:Any]) async throws -> Any? {
            try check();let reply=try await call(method,params);try check();return reply
        }
        guard !payload.isEmpty,payload.count<=18432 else {throw InputPilotError.invalid("Configuration payload exceeds bounded transfer")}
        try journal("before_txbegin")
        let raw=try await invoke("fs.txbegin",[:])
        guard let dict=raw as? [String:Any],let tx=dict["tx"] as? NSNumber,
              CFGetTypeID(tx) != CFBooleanGetTypeID(),tx.doubleValue.isFinite,
              (0...Double(UInt32.max)).contains(tx.doubleValue),tx.doubleValue==Double(tx.int64Value) else {
            throw InputPilotError.invalid("fs.txbegin did not return bounded numeric tx; no file data sent")
        }
        let id=tx.int64Value
        try journal("staging_tx_\(id)")
        for offset in stride(from:0,to:payload.count,by:192) {
            let end=min(offset+192,payload.count)
            do {
                _ = try await invoke("fs.writebin",["file":"keymap.json","data":payload.subdata(in:offset..<end).base64EncodedString(),"append":true,"completed":end==payload.count,"offset":offset,"tx":id])
            } catch {
                try? journal("staging_failed_tx_\(id)_offset_\(offset)")
                throw InputPilotError.invalid("fs.writebin failed at offset \(offset); tx=\(id); no commit/retry/abort issued; staged transaction may remain: \(String(error.localizedDescription.prefix(256)))")
            }
            try await Task.sleep(nanoseconds:50_000_000)
        }
        try journal("before_commit_tx_\(id)")
        do {_ = try await invoke("fs.txcommit",["tx":id])}
        catch {try? journal("commit_outcome_uncertain_tx_\(id)");throw InputPilotError.invalid("Commit outcome uncertain; preserve backup, no automatic retry: \(String(error.localizedDescription.prefix(256)))")}
        try journal("committed_unverified_tx_\(id)")
        return try await OneKeyFileReader.read(deadline:deadline,clock:clock,valid:valid,call:call).bytes
    }
}
