import Foundation
import CoreFoundation
import CryptoKit

/// Follow Input's fs.readbin contract; one bounded exact-file read, never a write.
@MainActor enum OneKeyFileReader {
    struct Result {let bytes:Data,chunks:Int}
    private struct Metadata:Equatable {let size:Int,checksum:String}
    private static func integer(_ raw:Any?) -> Int? {
        guard let n=raw as? NSNumber,CFGetTypeID(n) != CFBooleanGetTypeID(),
              n.doubleValue.isFinite,n.doubleValue>=0,n.doubleValue<=262144,
              n.doubleValue==Double(n.intValue) else {return nil}
        return n.intValue
    }
    private static func metadata(_ raw:Any?) throws -> Metadata {
        guard let files=raw as? [[String:Any]],files.count<=1024 else {
            throw InputPilotError.invalid("fs.list expected bounded file array; \(OneKeyUSBReply.summary(raw))")
        }
        let matches=files.filter {$0["name"] as? String=="keymap.json"}
        guard matches.count==1 else {throw InputPilotError.invalid("fs.list exact keymap.json entries=\(matches.count); no alternate path tried")}
        let entry=matches[0]
        let size=(entry["size"] as? String).flatMap {Int($0)} ?? integer(entry["size"])
        guard let size,(1...262144).contains(size),let hash=entry["checksum"] as? String,
              hash.count==40,hash.allSatisfy({"0123456789abcdefABCDEF".contains($0)}) else {
            throw InputPilotError.invalid("fs.list keymap.json needs bounded size and SHA-1 checksum; \(OneKeyUSBReply.summary(entry))")
        }
        return Metadata(size:size,checksum:hash.lowercased())
    }
    static func read(deadline:Double,clock:()->Double,valid:()->Bool,
                     call:(String,[String:Any]) async throws -> Any?) async throws -> Result {
        func check() throws {
            try Task.checkCancellation()
            guard valid(),deadline.isFinite,clock().isFinite,clock()<deadline else {
                throw InputPilotError.invalid("Chunked keymap observation retired or deadline expired")
            }
        }
        func invoke(_ method:String,_ params:[String:Any]) async throws -> Any? {
            try check();let result=try await call(method,params);try check();return result
        }
        let before=try metadata(await invoke("fs.list",["checksum":true,"rec":false]))
        var bytes=Data(),chunks=0
        while bytes.count<before.size {
            guard chunks<96 else {throw InputPilotError.invalid("Chunked keymap request budget exhausted")}
            let length=min(3072,before.size-bytes.count)
            let raw=try await invoke("fs.readbin",["file":"keymap.json","offset":bytes.count,"len":length])
            guard let reply=raw as? [String:Any],integer(reply["total_size"])==before.size,
                  let encoded=reply["data"] as? String,encoded.utf8.count<=4096,
                  let chunk=Data(base64Encoded:encoded),chunk.base64EncodedString()==encoded,
                  !chunk.isEmpty,chunk.count<=length else {
                throw InputPilotError.invalid("fs.readbin invalid chunk at offset \(bytes.count); expected total_size=\(before.size); \(OneKeyUSBReply.summary(raw))")
            }
            bytes.append(chunk);chunks+=1
            // Input uses 3072-byte chunks and 50 ms pacing; never issue a zero-length EOF call.
            try await Task.sleep(nanoseconds:50_000_000)
        }
        let after=try metadata(await invoke("fs.list",["checksum":true,"rec":false]))
        guard after==before else {throw InputPilotError.invalid("keymap.json listing changed during chunked read")}
        let digest=Insecure.SHA1.hash(data:bytes).map {String(format:"%02x",$0)}.joined()
        guard digest==before.checksum else {throw InputPilotError.invalid("Chunked keymap SHA-1 mismatch; snapshot rejected")}
        try check();return Result(bytes:bytes,chunks:chunks)
    }
}
