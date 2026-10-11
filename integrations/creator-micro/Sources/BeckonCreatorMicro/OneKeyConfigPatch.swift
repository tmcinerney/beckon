import Foundation
import CryptoKit

/// Full native file retained; only the explicitly empty Herdr slot4 cell changes.
struct OneKeyConfigPatch {
    let original:Data,candidate:Data
    let originalSHA256:String,candidateSHA256:String
    private let originalCanonical:Data,candidateCanonical:Data
    private static func canonical(_ data:Data) throws -> Data {
        let object=try JSONSerialization.jsonObject(with:data)
        return try JSONSerialization.data(withJSONObject:object,options:[.sortedKeys])
    }
    static func hash(_ data:Data)->String {SHA256.hash(data:data).map {String(format:"%02x",$0)}.joined()}
    static func make(original:Data,deviceID:String) throws -> OneKeyConfigPatch {
        guard original.count<=18432 else {throw InputPilotError.invalid("Original cannot fit bounded transfer")}
        let snapshot=try InputSnapshot.decode(original,deviceID:deviceID)
        let pi=snapshot.activeProfileIndex
        guard snapshot.profiles[pi].id==2,
              let li=snapshot.profiles[pi].layers.firstIndex(where:{$0.id==0}),
              snapshot.profiles[pi].layers[li].keymap[1][1]=="KC_NONE" else {
            throw InputPilotError.invalid("Only empty profile2 Herdrlayer0 slot4 can be assigned")
        }
        guard var object=try JSONSerialization.jsonObject(with:original) as? [String:Any],
              var profiles=object["profiles"] as? [[String:Any]],
              var layers=profiles[pi]["layers"] as? [[String:Any]],
              var layout=layers[li]["layout"] as? [String:Any],
              var keys=layout["keymap"] as? [[String]] else {throw InputPilotError.invalid("Native keymap shape unavailable")}
        keys[1][1]="KV_OAI_AG03";layout["keymap"]=keys;layers[li]["layout"]=layout;profiles[pi]["layers"]=layers;object["profiles"]=profiles
        let candidate=try JSONSerialization.data(withJSONObject:object,options:[.sortedKeys])
        guard candidate.count<=18432 else {throw InputPilotError.invalid("Candidate cannot fit bounded transfer")}
        _ = try InputSnapshot.decode(candidate,deviceID:deviceID)
        // Reverse the same single cell and prove every unknown field/definition survived.
        keys[1][1]="KC_NONE";layout["keymap"]=keys;layers[li]["layout"]=layout;profiles[pi]["layers"]=layers;object["profiles"]=profiles
        let reversed=try JSONSerialization.data(withJSONObject:object,options:[.sortedKeys])
        let originalCanonical=try canonical(original)
        guard reversed==originalCanonical else {throw InputPilotError.invalid("Patch changed unrelated native metadata")}
        return .init(original:original,candidate:candidate,originalSHA256:hash(original),candidateSHA256:hash(candidate),originalCanonical:originalCanonical,candidateCanonical:try canonical(candidate))
    }
    func matchesOriginal(_ data:Data) throws -> Bool {try Self.canonical(data)==originalCanonical}
    func matchesCandidate(_ data:Data) throws -> Bool {try Self.canonical(data)==candidateCanonical}
    func restorePayload(current:Data) throws -> Data {
        guard try matchesCandidate(current) else {throw InputPilotError.invalid("Current keymap differs from installed candidate; refusing to overwrite unrelated edits")}
        return original
    }
}
