import Foundation
import Combine
import CoreFoundation
import WLKit
import BeckonPanelModel

public struct OneKeyTarget:Codable,Equatable {
    public struct Native:Codable,Equatable {public let agent:String,kind:String,value:String}
    public let session:String,socket:String,pane_id:String,terminal_id:String
    public let agent_session:Native?
    public func validate() throws {
        guard [session,pane_id,terminal_id].allSatisfy({!$0.isEmpty && $0.utf8.count<=256 && !$0.unicodeScalars.contains(where:{CharacterSet.controlCharacters.contains($0)})}),
              socket.hasPrefix("/"),socket.utf8.count<104,
              agent_session.map({$0.kind=="id" && !$0.agent.isEmpty && !$0.value.isEmpty}) ?? true else {
            throw InputPilotError.invalid("Exact native target identity required")
        }
    }
}
public struct OneKeyLaunch {
    public let socket:String,selector:PilotDeviceSelector,target:OneKeyTarget
    public let profileID:Int,layerID:Int,allowLEDs:Bool
    public static func parse(_ args:[String]) throws -> OneKeyLaunch? {
        guard args.contains("--input-one-key-live") else {return nil}
        var values:[String:String]=[:],allow=false,i=0
        let valued:Set<String>=["--backend-socket","--device-serial","--device-product-id","--profile-id","--layer-id","--expected-target"]
        while i<args.count {
            let a=args[i]
            if a=="--input-one-key-live" {guard args.filter({$0==a}).count==1 else {throw InputPilotError.invalid("Duplicate live flag")};i+=1}
            else if a=="--allow-one-key-leds" {guard !allow else {throw InputPilotError.invalid("Duplicate arm flag")};allow=true;i+=1}
            else if valued.contains(a),i+1<args.count,values[a]==nil {values[a]=args[i+1];i+=2}
            else {throw InputPilotError.invalid("Live pilot does not accept other preview/autostart flags")}
        }
        guard let socket=values["--backend-socket"],socket.hasPrefix("/"),socket.utf8.count<104,!socket.utf8.contains(0),
              let serial=values["--device-serial"],
              let product=values["--device-product-id"],["0x8297","0x8298"].contains(product),
              let productID=Int(product.dropFirst(2),radix:16),let expected=values["--expected-target"],expected.utf8.count<=4096,
              values["--profile-id"]=="2",values["--layer-id"]=="0" else {throw InputPilotError.invalid("Explicit backend, USB serial/product ID (0x8297 or 0x8298), profile 2/layer 0 and target JSON required")}
        let target=try JSONDecoder().decode(OneKeyTarget.self,from:Data(expected.utf8));try target.validate()
        return OneKeyLaunch(socket:socket,selector:try PilotDeviceSelector(serial:serial,productID:productID),target:target,profileID:2,layerID:0,allowLEDs:allow)
    }
}
/// Diagnose reply envelopes without retaining keymap contents or other field values.
enum OneKeyUSBReply {
    static func kind(_ raw:Any?) -> String {
        guard let raw else {return "nil"}
        if raw is NSNull {return "null"}
        if raw is String {return "string"}
        if raw is [String:Any] {return "object"}
        if raw is [Any] {return "array"}
        if raw is NSNumber {return "number"}
        return "other"
    }
    static func summary(_ raw:Any?) -> String {
        guard let dict=raw as? [String:Any] else {return "replyType=\(kind(raw))"}
        let keys=dict.keys.sorted().prefix(12).map {key in
            String(key.unicodeScalars.filter {!CharacterSet.controlCharacters.contains($0)}.prefix(48))
        }.joined(separator:",")
        let size=(dict["data"] as? String).map {"; dataBytes=\($0.utf8.count)"} ?? ""
        return "replyType=object; fields=[\(keys)]; dataType=\(kind(dict["data"]))\(size)"
    }
    static func keymapText(_ raw:Any?) throws -> String {
        guard let dict=raw as? [String:Any] else {
            throw InputPilotError.invalid("fs.readbin keymap.json wrong response type; \(summary(raw))")
        }
        guard let data=dict["data"] else {
            throw InputPilotError.invalid("fs.readbin keymap.json missing data field; \(summary(raw))")
        }
        guard let text=data as? String else {
            throw InputPilotError.invalid("fs.readbin keymap.json wrong data type; \(summary(raw))")
        }
        guard text.utf8.count<=262_144 else {
            throw InputPilotError.invalid("fs.readbin keymap.json data exceeds 262144-byte limit: \(text.utf8.count) bytes")
        }
        return text
    }
    static func validateJSON(_ text:String) throws {
        do {_ = try JSONSerialization.jsonObject(with:Data(text.utf8))}
        catch {throw InputPilotError.invalid("fs.readbin keymap.json malformed JSON; dataBytes=\(text.utf8.count)")}
    }
}
public struct OneKeyFrame:Decodable {
    public struct Binding:Decodable {let key:String,session:String,pane_id:String}
    public struct Source:Decodable,Equatable {let session:String,socket:String,generation:UInt64,refresh:UInt64,age_ms:Double}
    public struct Render:Decodable {let key:String,state:String,color:String,brightness:Double,motion:String}
    let version:String,epoch:String,available:Bool
    let binding:Binding?,source:Source?,terminal_id:String?,agent_session:OneKeyTarget.Native?,raw_status:String?,render:Render?
    static func decode(_ data:Data) throws -> OneKeyFrame {
        guard data.count<=262_144 else {throw InputPilotError.invalid("Projection exceeds 256 KiB")}
        struct Envelope:Decodable {let type:String;let data:OneKeyFrame?}
        let e=try JSONDecoder().decode(Envelope.self,from:data)
        guard e.type=="ok",let frame=e.data else {throw InputPilotError.invalid("Backend unavailable")}
        return frame
    }
}
/// Neither IDs nor a pair of Herdr reads establish an immutable runtime lifetime.
/// This guard detects observed changes only; it never authorizes native actions.
public struct OneKeyObservation {
    public private(set) var frame:OneKeyFrame?
    private var receivedAt:Double=0, expiresAt:Double=0
    private var epoch:String?,generation:UInt64?,refresh:UInt64=0
    public private(set) var requiresReselection=false
    public mutating func select() {frame=nil;epoch=nil;generation=nil;refresh=0;requiresReselection=false}
    public mutating func accept(_ input:OneKeyFrame,target:OneKeyTarget,now:Double,requestStartedAt:Double) throws {
        frame=nil
        guard now.isFinite,now>=0,requestStartedAt.isFinite,requestStartedAt>=0,requestStartedAt<=now,!requiresReselection,input.version=="beckon.pilot.v1",!input.epoch.isEmpty else {throw InputPilotError.invalid("Pilot requires explicit selection")}
        if let epoch,epoch != input.epoch {requiresReselection=true;throw InputPilotError.invalid("Backend epoch changed; explicitly reselect")}
        guard input.available,let source=input.source,let binding=input.binding,let render=input.render,
              source.age_ms.isFinite,(0..<10_000).contains(source.age_ms),source.generation>0,source.refresh>0 else {throw InputPilotError.invalid("Native observation unavailable or stale")}
        if let generation,generation != source.generation {requiresReselection=true;throw InputPilotError.invalid("Herdr connection changed; explicitly reselect")}
        guard binding.key=="f4",binding.session==target.session,binding.pane_id==target.pane_id,
              source.session==target.session,source.socket==target.socket,input.terminal_id==target.terminal_id,
              input.agent_session==target.agent_session else {requiresReselection=true;throw InputPilotError.invalid("f4 or observed native identity changed; explicitly reselect")}
        guard source.refresh>=refresh,now>=receivedAt,let status=input.raw_status,
              ["unknown","idle","working","blocked","done"].contains(render.state),
              (target.agent_session != nil || status=="unknown"),render.state==(["idle","working","blocked","done"].contains(status) ? status:"unknown") else {throw InputPilotError.invalid("Out-of-order or inconsistent observation")}
        _ = try OneKeyLED.record(render)
        // Count the entire request duration conservatively against source freshness.
        let deadline=requestStartedAt+(10_000-source.age_ms)/1000
        guard now<deadline else {throw InputPilotError.invalid("Native observation expired in transit")}
        expiresAt=source.refresh==refresh ? min(expiresAt,deadline):deadline
        epoch=input.epoch;generation=source.generation;refresh=source.refresh;receivedAt=now;frame=input
    }
    public func current(now:Double)->OneKeyFrame? {guard now.isFinite,now>=receivedAt,now<expiresAt,!requiresReselection else {return nil};return frame}
    public mutating func unavailable() {frame=nil}
}
public struct AG03Receipt:Equatable {
    public let sequence:Int,connectionGeneration:UInt64,observedAt:Double
    public let attribution="UN-ATTRIBUTED"
}
public struct AG03Observer {
    private var generation:UInt64?,held=false,needsRelease=false,sequence=0
    public init() {}
    public mutating func connection(_ value:UInt64) {
        needsRelease=generation != nil;generation=value;held=false
    }
    public mutating func receive(method:String,params:Any?,generation:UInt64,now:Double)->AG03Receipt? {
        guard self.generation==generation,now.isFinite,now>=0,method=="v.oai.hid",let params=params as? [String:Any],
              params["k"] as? String=="AG03",let action=params["act"] as? NSNumber,
              CFGetTypeID(action) != CFBooleanGetTypeID(),action.doubleValue==Double(action.intValue),[0,1].contains(action.intValue) else {return nil}
        if action.intValue==0 {held=false;needsRelease=false;return nil}
        guard !held,!needsRelease else {return nil};held=true;sequence+=1
        return AG03Receipt(sequence:sequence,connectionGeneration:generation,observedAt:now)
    }
}
/// Firmware proof is deliberately absent in the production pilot until physical acceptance.
/// ACKs, Input disk snapshots and emulator behavior cannot manufacture this evidence.
public struct OneKeyFirmwareProof {
    let omittedIDsPreserved:Bool,layerScoped:Bool,cleanupRestoresOwner:Bool,coexistenceVerified:Bool,wireDrained:Bool
    public var accepted:Bool {omittedIDsPreserved && layerScoped && cleanupRestoresOwner && coexistenceVerified && wireDrained}
}
public struct OneKeyLease:Equatable {
    let generation:UInt64,profileID:Int,layerID:Int,layerIndex:Int,firmware:String,keyCode:String
    var acceptable:Bool {profileID==2 && layerID==0 && layerIndex==3 && firmware=="0.6.3" && keyCode=="KV_OAI_AG03"}
}
public enum OneKeyLED {
    static func record(_ render:OneKeyFrame.Render) throws -> OAI.Thread {
        guard render.key=="f4",render.brightness.isFinite,(0...1).contains(render.brightness),
              render.color.count==7,render.color.first=="#",let color=Int(render.color.dropFirst(),radix:16),
              ["steady","breathe","pulse","flicker"].contains(render.motion) else {throw InputPilotError.invalid("Invalid f4 treatment")}
        return OAI.Thread(id:3,color:color,brightness:min(render.brightness,0.2),effect:render.motion=="breathe" ? .breath:.solid,speed:0.5,syncKeys:false,syncAmbient:false)
    }
    static let cleanup=OAI.Thread(id:3,color:0,brightness:0,effect:.off,speed:0,syncKeys:false,syncAmbient:false)
}
@MainActor public protocol OneKeyDevicePort:AnyObject {
    var generation:UInt64 {get}
    func connect() throws
    func read(_ method:String) async throws -> Any?
    func paint(_ record:OAI.Thread) async throws
    func disconnect()
}
@MainActor final class OneKeyUSBPort:OneKeyDevicePort {
    let device:WLDevice
    private var connectedAt:Double?
    init(selector:PilotDeviceSelector) {device=WLDevice(pilotSelector:selector)}
    var generation:UInt64 {device.connectionGeneration}
    func connect() throws {try device.connect();connectedAt=ProcessInfo.processInfo.systemUptime}
    func read(_ method:String) async throws -> Any? {
        let expected=generation
        if method=="keymap.readbin" {
            guard let connectedAt else {throw InputPilotError.invalid("USB observation not connected")}
            let result=try await OneKeyFileReader.read(deadline:connectedAt+60,clock:{ProcessInfo.processInfo.systemUptime},
                valid:{self.device.isConnected && self.generation==expected}) {method,params in
                    try await self.device.callAsync(method,params:params,expectedGeneration:expected)
                }
            guard let text=String(data:result.bytes,encoding:.utf8) else {throw InputPilotError.invalid("Chunked keymap is not UTF-8")}
            return ["data":text,"chunks":result.chunks,"checksum_verified":true]
        }
        guard ["sys.version","device.status"].contains(method) else {throw InputPilotError.invalid("Pilot read refused")}
        return try await device.callAsync(method,params:nil,expectedGeneration:expected)
    }
    func paint(_ record:OAI.Thread) async throws {
        guard record.id==3,record.syncKeys==false,record.syncAmbient==false else {throw InputPilotError.invalid("Only unsynchronized id3 allowed")}
        _ = try await device.callAsync(OAI.methodThreads,params:OAI.threadsParams([record]),expectedGeneration:generation)
    }
    func disconnect() {device.disconnect(reason:nil)}
}
/// One in-flight paint; a pending latest desire is retired before cleanup.
@MainActor final class OneKeyWriter {
    let port:OneKeyDevicePort
    private var lease:OneKeyLease?,desired:OAI.Thread?,busy=false,stopping=false
    private var safe:()->OneKeyLease?
    private(set) var cleanupConfirmed=false
    private(set) var cleanupAcknowledged=false
    init(port:OneKeyDevicePort,safe:@escaping ()->OneKeyLease?) {self.port=port;self.safe=safe}
    func arm(_ lease:OneKeyLease,proof:OneKeyFirmwareProof?,allowed:Bool) throws {
        guard allowed,proof?.accepted==true,lease.acceptable,lease.generation==port.generation,safe()==lease else {throw InputPilotError.invalid("Physical ownership, drain and firmware capabilities unverified")}
        self.lease=lease;stopping=false;cleanupConfirmed=false;cleanupAcknowledged=false
    }
    func update(_ record:OAI.Thread) async {
        guard !stopping,let lease,safe()==lease,port.generation==lease.generation else {return}
        guard record.id==3,record.syncKeys==false,record.syncAmbient==false,
              let brightness=record.brightness,brightness.isFinite,(0...0.2).contains(brightness),
              let color=record.color,(0...0xffffff).contains(color),
              record.effect == .solid || record.effect == .breath else {return}
        desired=record;guard !busy else {return};busy=true
        while !stopping,let next=desired {desired=nil
            guard safe()==lease,port.generation==lease.generation else {self.lease=nil;break}
            do {try await port.paint(next)} catch {self.lease=nil;break}
        }
        busy=false
    }
    func stop() async {
        stopping=true;desired=nil
        let deadline=ProcessInfo.processInfo.systemUptime+2
        while busy,ProcessInfo.processInfo.systemUptime<deadline {try? await Task.sleep(nanoseconds:10_000_000)}
        if busy {lease=nil;port.disconnect();return}
        guard let lease,safe()==lease,port.generation==lease.generation else {port.disconnect();return}
        do {try await port.paint(OneKeyLED.cleanup);cleanupAcknowledged=true
            // ACK cannot prove that the native appearance returned rather than a black overlay.
            cleanupConfirmed=false} catch {cleanupConfirmed=false}
        self.lease=nil;port.disconnect()
    }
}
