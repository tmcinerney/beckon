import Foundation
import Darwin

/// Private, exclusive backup directory. Exact original bytes reach stable storage before USB mutation.
struct OneKeyConfigBackup {
    let directory:URL
    let serial:String
    private struct Manifest:Codable {
        let version:Int,serial:String,originalSHA256:String,candidateSHA256:String
    }
    private func canonicalDirectory() throws -> String {
        guard directory.isFileURL,directory.path.hasPrefix("/"),
              directory.resolvingSymlinksInPath().path==directory.path else {
            throw InputPilotError.invalid("Backup requires an absolute, non-symlink durable directory")
        }
        return directory.path
    }
    private func syncDirectory(_ path:String) throws {
        let fd=Darwin.open(path,O_RDONLY|O_NOFOLLOW)
        guard fd>=0 else {throw InputPilotError.invalid("Cannot open backup directory")}
        defer {Darwin.close(fd)}
        guard fsync(fd)==0 else {throw InputPilotError.invalid("Cannot sync backup directory")}
    }
    private func write(_ data:Data,name:String) throws {
        let path=directory.appendingPathComponent(name).path
        let fd=Darwin.open(path,O_WRONLY|O_CREAT|O_EXCL|O_NOFOLLOW,mode_t(0o600))
        guard fd>=0 else {throw InputPilotError.invalid("Backup file already exists or cannot be created: \(name)")}
        defer {Darwin.close(fd)}
        try data.withUnsafeBytes { bytes in
            var offset=0
            while offset<data.count {
                let count=Darwin.write(fd,bytes.baseAddress!.advanced(by:offset),data.count-offset)
                guard count>0 else {throw InputPilotError.invalid("Backup write failed")}
                offset+=count
            }
        }
        guard fsync(fd)==0 else {throw InputPilotError.invalid("Backup sync failed")}
    }
    func persist(_ patch:OneKeyConfigPatch) throws {
        let path=try canonicalDirectory()
        guard mkdir(path,mode_t(0o700))==0 else {throw InputPilotError.invalid("Backup directory must be new; existing backups are never overwritten")}
        // A partially written directory is retained for inspection and never accepted as recovery proof.
        try write(patch.original,name:"original-keymap.json")
        try write(patch.candidate,name:"candidate-keymap.json")
        let manifest=Manifest(version:1,serial:serial,originalSHA256:patch.originalSHA256,candidateSHA256:patch.candidateSHA256)
        try write(JSONEncoder().encode(manifest),name:"manifest.json")
        try write(Data(),name:"journal.txt")
        try syncDirectory(path)
        try syncDirectory(directory.deletingLastPathComponent().path)
        let verified=try load()
        guard verified.original==patch.original,verified.candidate==patch.candidate else {throw InputPilotError.invalid("Durable backup readback differs")}
    }
    private func read(_ name:String,cap:Int) throws -> Data {
        _ = try canonicalDirectory()
        let fd=Darwin.open(directory.appendingPathComponent(name).path,O_RDONLY|O_NOFOLLOW)
        guard fd>=0 else {throw InputPilotError.invalid("Backup file unavailable")}
        defer {Darwin.close(fd)}
        var info=stat()
        guard fstat(fd,&info)==0,(info.st_mode & S_IFMT)==S_IFREG,info.st_size>=0,info.st_size<=cap else {throw InputPilotError.invalid("Backup is not a bounded regular file")}
        var data=Data(),buffer=[UInt8](repeating:0,count:4096)
        while true {
            let count=Darwin.read(fd,&buffer,buffer.count)
            guard count>=0 else {throw InputPilotError.invalid("Backup read failed")}
            if count==0 {break}
            data.append(contentsOf:buffer.prefix(count))
            guard data.count<=cap else {throw InputPilotError.invalid("Backup grew beyond limit")}
        }
        return data
    }
    func load() throws -> OneKeyConfigPatch {
        let manifest=try JSONDecoder().decode(Manifest.self,from:read("manifest.json",cap:4096))
        guard manifest.version==1,manifest.serial==serial else {throw InputPilotError.invalid("Backup belongs to a different device")}
        let original=try read("original-keymap.json",cap:18432),candidate=try read("candidate-keymap.json",cap:18432)
        guard OneKeyConfigPatch.hash(original)==manifest.originalSHA256,OneKeyConfigPatch.hash(candidate)==manifest.candidateSHA256 else {throw InputPilotError.invalid("Backup checksum mismatch")}
        let patch=try OneKeyConfigPatch.make(original:original,deviceID:serial)
        guard patch.candidate==candidate else {throw InputPilotError.invalid("Backup is not the exact one-cell candidate")}
        return patch
    }
    func journal(_ state:String) throws {
        _ = try canonicalDirectory()
        guard state.utf8.count<=256,!state.contains("\n") else {throw InputPilotError.invalid("Invalid journal state")}
        let fd=Darwin.open(directory.appendingPathComponent("journal.txt").path,O_WRONLY|O_APPEND|O_NOFOLLOW)
        guard fd>=0 else {throw InputPilotError.invalid("Backup journal unavailable")}
        defer {Darwin.close(fd)}
        let bytes=Data((state+"\n").utf8)
        let count=bytes.withUnsafeBytes {Darwin.write(fd,$0.baseAddress,bytes.count)}
        guard count==bytes.count,fsync(fd)==0 else {throw InputPilotError.invalid("Backup journal sync failed")}
    }
}
