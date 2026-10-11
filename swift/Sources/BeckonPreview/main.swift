import Foundation
import BeckonControl

do {
    let args=Array(CommandLine.arguments.dropFirst())
    let encoder=JSONEncoder()
    encoder.outputFormatting = [.prettyPrinted,.sortedKeys]
    switch args.first {
    case "trellis":
        guard args.count == 2 else { throw ControlError.invalidConfiguration }
        let page=try TrellisReader.decode(Data(contentsOf:URL(fileURLWithPath:args[1])))
        print(String(decoding:try encoder.encode(page),as:UTF8.self))
    case "route":
        guard args.count == 4, let key=Int(args[3]) else { throw ControlError.invalidConfiguration }
        let decoder=JSONDecoder(); decoder.keyDecodingStrategy = .convertFromSnakeCase
        let snapshot=try decoder.decode(Snapshot.self,from:Data(contentsOf:URL(fileURLWithPath:args[1])))
        let config=try decoder.decode(RoutingConfig.self,from:Data(contentsOf:URL(fileURLWithPath:args[2])))
        let intent=try Router(snapshot:snapshot,config:config).role(key:key,now:Date().timeIntervalSince1970)
        struct Preview: Encodable { let dryRun: Bool; let intent: FocusIntent }
        print(String(decoding:try encoder.encode(Preview(dryRun:true,intent:intent)),as:UTF8.self))
    default:
        FileHandle.standardError.write(Data("Usage: beckon-control-preview trellis COMPACT.json | route SNAPSHOT.json CONFIG.json KEY\nNo live actions or hardware access.\n".utf8))
        exit(2)
    }
} catch {
    FileHandle.standardError.write(Data("Preview unavailable: \(error)\n".utf8))
    exit(1)
}
