import SwiftUI
import Foundation
import WLKit
import BeckonPanelModel

@MainActor public final class OneKeyLiveModel:ObservableObject {
    public let launch:OneKeyLaunch
    @Published public private(set) var status="Stopped · explicitly select to observe"
    @Published public private(set) var deviceStatus="Disconnected · firmware capabilities unverified"
    @Published public private(set) var usbDiagnostics="No USB replies captured"
    @Published public private(set) var receipt:AG03Receipt?
    @Published public private(set) var snapshot:InputSnapshot?
    @Published public private(set) var layerObservedAt:Double?
    @Published public private(set) var layerIndex:Int?
    @Published public private(set) var running=false
    @Published public private(set) var usbRunning=false
    private var observation=OneKeyObservation(),events=AG03Observer()
    private var backendTask:Task<Void,Never>?,usbTask:Task<Void,Never>?
    private var backendRun=0,usbRun=0
    private var viewOwners:Set<UUID>=[]
    private let port:OneKeyUSBPort
    public init(launch:OneKeyLaunch) {self.launch=launch;port=OneKeyUSBPort(selector:launch.selector)}
    public var currentFrame:OneKeyFrame? {observation.current(now:ProcessInfo.processInfo.systemUptime)}
    public var reselectionRequired:Bool {observation.requiresReselection}
    public var ledStatus:String {"LED writer DISARMED · ownership, reply drain, omitted-ID preservation, layer scope and cleanup remain unverified"}
    public func selectAndObserve() {
        stopBackend();observation.select();running=true;backendRun+=1;let run=backendRun
        backendTask=Task { [weak self] in
            guard let self else {return}
            while !Task.isCancelled,self.backendRun==run {
                let start=ProcessInfo.processInfo.systemUptime
                do {
                    let data=try await PilotSocketReader.read(path:launch.socket)
                    guard !Task.isCancelled,self.backendRun==run else {return}
                    try observation.accept(OneKeyFrame.decode(data),target:launch.target,now:ProcessInfo.processInfo.systemUptime,requestStartedAt:start)
                    status="Observed f4 · \(currentFrame?.raw_status ?? "unavailable") · IDs do not prove runtime lifetime"
                } catch {
                    guard !Task.isCancelled,self.backendRun==run else {return}
                    observation.unavailable();status=error.localizedDescription
                    if observation.requiresReselection {running=false;return}
                }
                let wait=max(0.05,1-(ProcessInfo.processInfo.systemUptime-start))
                try? await Task.sleep(nanoseconds:UInt64(wait*1_000_000_000))
            }
        }
    }
    public func stopBackend() {backendRun+=1;backendTask?.cancel();backendTask=nil;observation.unavailable();running=false;status="Stopped"}
    public func importSnapshot(_ url:URL) {
        do {snapshot=try InputSnapshotReader.read(url,deviceID:launch.selector.serial);deviceStatus="Disk import only · no device/layer authority"}
        catch {snapshot=nil;deviceStatus=error.localizedDescription}
    }
    func recordLayerStatus(_ reply:[String:Any]?,observedAt:Double) {
        layerIndex=reply?["layer_index"] as? Int;layerObservedAt=observedAt
    }
    public func observeUSB(probe:Bool=false) {
        guard !usbRunning else {return}
        // No reconnect loop, no constructor I/O and no automatic native permission response.
        do {try port.connect()} catch {deviceStatus=error.localizedDescription;return}
        usbRun+=1;let run=usbRun,generation=port.generation
        events.connection(generation);usbRunning=true;receipt=nil;usbDiagnostics="Connected · no USB replies captured in this observation"
        port.device.onNotification={ [weak self] method,params in
            guard let self,self.usbRun==run,self.usbRunning,self.port.generation==generation else {return}
            if let receipt=self.events.receive(method:method,params:params,generation:generation,now:ProcessInfo.processInfo.systemUptime) {self.receipt=receipt}
        }
        port.device.onDisconnect={ [weak self] reason in self?.stopUSB(reason) }
        usbTask=Task { [weak self] in
            guard let self else {return}
            let deadline=ProcessInfo.processInfo.systemUptime+60
            var phase="sys.version",snapshotReadInThisRun=false,fileEvidence:String?
            do {
                let version=try await port.read("sys.version") as? [String:Any]
                guard usbRun==run,!Task.isCancelled else {return}
                guard version?["version"] as? String=="0.6.3" else {throw InputPilotError.invalid("Unsupported or missing firmware version")}
                usbDiagnostics="sys.version accepted firmware 0.6.3"
                while !Task.isCancelled,usbRun==run,ProcessInfo.processInfo.systemUptime<deadline {
                    phase="device.status"
                    let statusReply=try await port.read("device.status")
                    guard usbRun==run,!Task.isCancelled,port.generation==generation else {return}
                    let statusObservedAt=ProcessInfo.processInfo.systemUptime
                    let rawStatus=statusReply as? [String:Any]
                    recordLayerStatus(rawStatus,observedAt:statusObservedAt)
                    let statusEvidence="device.status: \(OneKeyUSBReply.summary(statusReply)); layerIndex=\(rawStatus?["layer_index"] as? Int ?? -1)"
                    usbDiagnostics="firmware 0.6.3 · \(statusEvidence)"+(fileEvidence.map {" · "+$0} ?? "")
                    if snapshotReadInThisRun {
                        try? await Task.sleep(nanoseconds:1_000_000_000);continue
                    }
                    if probe {
                        phase="EXPERIMENTAL single prefix read"
                        let result=try await port.read("keymap.probe") as? [String:Any]
                        guard usbRun==run,!Task.isCancelled,port.generation==generation else {return}
                        usbDiagnostics="firmware 0.6.3 · \(statusEvidence) · experimental prefix: requested=\(result?["requested"] as? Int ?? -1); received=\(result?["received"] as? Int ?? -1); total=\(result?["total"] as? Int ?? -1); full checksum/schema NOT verified"
                        stopUSB("Experimental prefix read completed; not a keymap snapshot or fix")
                        return
                    }
                    phase="fs.list / fs.readbin keymap.json"
                    let rawKeymap=try await port.read("keymap.readbin")
                    guard usbRun==run,!Task.isCancelled,port.generation==generation else {return}
                    fileEvidence="fs.readbin assembled: \(OneKeyUSBReply.summary(rawKeymap)); chunks=\((rawKeymap as? [String:Any])?["chunks"] as? Int ?? 0); checksum verified"
                    usbDiagnostics="firmware 0.6.3 · \(statusEvidence) · \(fileEvidence!)"
                    let text=try OneKeyUSBReply.keymapText(rawKeymap)
                    phase="keymap JSON"
                    try OneKeyUSBReply.validateJSON(text)
                    phase="keymap schema"
                    snapshot=try InputSnapshot.decode(Data(text.utf8),deviceID:launch.selector.serial)
                    snapshotReadInThisRun=true
                    deviceStatus="USB reports observed · correlation unverified until owner/reset/drain proof · no layer attribution or LED writes"
                    try? await Task.sleep(nanoseconds:1_000_000_000)
                }
                if usbRun==run {stopUSB("60-second observation complete")}
            } catch {if usbRun==run {stopUSB("\(phase): \(error.localizedDescription)")}}
        }
    }
    public func stopUSB(_ reason:String="Stopped") {
        usbRun+=1;usbTask?.cancel();usbTask=nil;usbRunning=false
        port.device.onNotification=nil;port.device.onDisconnect=nil;port.disconnect()
        layerIndex=nil;layerObservedAt=nil;deviceStatus=reason+" · writer disarmed; no hardware cleanup claimed"
    }
    public func retainView(_ id:UUID) {viewOwners.insert(id)}
    public func releaseView(_ id:UUID) {guard viewOwners.remove(id) != nil else {return};if viewOwners.isEmpty {stop()}}
    public func stop() {stopBackend();stopUSB()}
}
@MainActor public struct OneKeyPilotView:View {
    @ObservedObject private var model:OneKeyLiveModel
    @State private var importing=false
    @State private var viewOwner=UUID()
    public init(model:OneKeyLiveModel) {self.model=model}
    public var body:some View {
        VStack(alignment:.leading,spacing:18) {
            Text(model.launch.selector.serial=="STATUS-ONLY-NOT-A-DEVICE" ? "Herdr · Status-only panel":"Creator Micro · USB observation").font(.largeTitle)
            Text("USB serial \(model.launch.selector.serial) · product \(String(format:"0x%04X",model.launch.selector.productID))").textSelection(.enabled)
            Text("Physical slot 4 · firmware id3 / AG03 · ledger f4").font(.headline)
            Text("\(model.launch.target.session) · \(model.launch.target.pane_id) · \(model.launch.target.terminal_id)").textSelection(.enabled)
            HStack {
                Button("Select target and observe backend") {model.selectAndObserve()}
                Button("Stop backend") {model.stopBackend()}.disabled(!model.running)
                Button("Import read-only Input snapshot") {importing=true}
            }
            TimelineView(.periodic(from:.now,by:1)) {_ in
                VStack(alignment:.leading) {
                    if let frame=model.currentFrame,let render=frame.render {
                        let rgb=Int(render.color.dropFirst(),radix:16) ?? 0x808080
                        RoundedRectangle(cornerRadius:16).fill(Color(red:Double((rgb>>16)&255)/255,green:Double((rgb>>8)&255)/255,blue:Double(rgb&255)/255).opacity(render.brightness))
                            .frame(width:110,height:85).overlay(Text("f4\nslot 4").font(.headline))
                    }
                    Text(model.currentFrame.map {"Status: \($0.raw_status ?? "unknown") · \($0.render?.motion ?? "unknown")"} ?? "Status unavailable / expired")
                    Text(model.status).foregroundStyle(.secondary)
                }
            }
            Divider()
            HStack {
                Button("Observe exact USB device for 60 seconds") {model.observeUSB()}.disabled(model.usbRunning || model.launch.selector.serial=="STATUS-ONLY-NOT-A-DEVICE")
                Button("Experimental single ≤192-byte prefix read") {model.observeUSB(probe:true)}.disabled(model.usbRunning || model.launch.selector.serial=="STATUS-ONLY-NOT-A-DEVICE")
                Button("Stop USB") {model.stopUSB()}.disabled(!model.usbRunning)
            }
            Text(model.deviceStatus).textSelection(.enabled)
            Text(model.usbDiagnostics).font(.caption).textSelection(.enabled)
            if let snapshot=model.snapshot {
                Text("Snapshot: profile ID \(snapshot.profiles[snapshot.activeProfileIndex].id) · imported \(snapshot.fingerprint.prefix(12))")
                if let layer=model.layerIndex,let at=model.layerObservedAt {
                    TimelineView(.periodic(from:.now,by:1)) {_ in Text("Last reported layer index \(layer) · age \(Int(max(0,ProcessInfo.processInfo.systemUptime-at))) s · event-time layer UNKNOWN")}
                }
            }
            if let receipt=model.receipt {
                Text("UN-ATTRIBUTED AG03 observation #\(receipt.sequence) · connection \(receipt.connectionGeneration)").font(.headline)
                Text("Physical input observed; no agent selected, focused or answered.")
            } else {Text("No AG03 press observed")}
            Divider()
            Text(model.ledStatus).foregroundStyle(.secondary)
            Text("Pulse/flicker use steady fallback in any later approved LED experiment. No key bindings, approvals or native actions are dispatched.")
            Spacer()
        }.padding(28).frame(minWidth:900,minHeight:580)
        .fileImporter(isPresented:$importing,allowedContentTypes:[.json],allowsMultipleSelection:false) {result in
            if case .success(let urls)=result,let url=urls.first {
                let scope=url.startAccessingSecurityScopedResource();defer {if scope {url.stopAccessingSecurityScopedResource()}}
                model.importSnapshot(url)
            }
        }
        .onAppear {model.retainView(viewOwner)}
        .onDisappear {model.releaseView(viewOwner)}
    }
}
