import Foundation
import Combine

@MainActor public final class PanelModel: ObservableObject {
    @Published public private(set) var configuration: PanelConfiguration
    @Published public private(set) var snapshot: AgentSnapshot?
    @Published public private(set) var health = "Not observed"
    @Published public private(set) var preview = ""
    @Published public private(set) var busy = false
    @Published public private(set) var metadataHealth = "No board selected"
    @Published public private(set) var directorySnapshot:CoordinatorSnapshot?
    public private(set) var generation:UInt64=0
    public private(set) var sourceGeneration:UInt64=0
    public let sourceReader:any AgentStateSource
    public let store:ConfigStore?
    private let directory:CoordinatorDirectory
    private var observation:Task<AgentSnapshot,Error>?
    private var polling:Task<Void,Never>?
    private var metadataTask:Task<CoordinatorSnapshot,Error>?
    private var metadataGeneration:UInt64=0
    private var metadataPending:BoardSelection?
    private var metadataBusy=false
    private var failures=0
    private var closed=false
    public init(configuration:PanelConfiguration,source:any AgentStateSource=SocketAgentSource(),store:ConfigStore?=nil,
                directory:CoordinatorDirectory=CoordinatorDirectory()) {
        self.configuration=configuration;self.sourceReader=source;self.store=store;self.directory=directory
    }
    public func startObserving() {
        guard !closed,polling==nil else {return}
        polling=Task { [weak self] in
            while !Task.isCancelled {
                guard let self else {return}
                await self.refresh()
                let delay=self.failures==0 ? 2.5 : min(10,pow(2,Double(min(self.failures-1,4))))
                do {try await Task.sleep(nanoseconds:UInt64(delay*1e9))} catch {return}
            }
        }
    }
    public func stopObserving() {
        polling?.cancel();polling=nil;observation?.cancel()
    }
    private func observed(source:SourceKey) async throws -> AgentSnapshot {
        let reader=sourceReader
        let task=Task {try await reader.read(source:source,budget:2)}
        observation=task
        defer {observation=nil}
        let start=ProcessInfo.processInfo.systemUptime
        let result=try await withTaskCancellationHandler(operation:{try await task.value},onCancel:{task.cancel()})
        try Task.checkCancellation()
        guard result.source==source,ProcessInfo.processInfo.systemUptime-start<8 else {
            throw PanelError.unavailable("Wrong source or expired preview")
        }
        // Reject a delayed/out-of-order success rather than rendering it as healthy.
        let now=Date().timeIntervalSince1970
        guard result.observedAt<=now,now-result.observedAt<=10 else {throw PanelError.unavailable("Stale observation")}
        if let previous=snapshot,previous.source==source,result.observedAt<previous.observedAt {
            throw PanelError.unavailable("Out-of-order observation")
        }
        return result
    }
    private func disconnected(_ error:Error) {
        snapshot=nil;sourceGeneration &+= 1;preview="";failures+=1
        health="Unavailable: \(error.localizedDescription)"
    }
    public func refresh() async {
        guard !closed,!busy else {return}
        busy=true
        defer {busy=false}
        let g=generation, s=sourceGeneration, source=configuration.source
        do {
            let result=try await observed(source:source)
            guard !closed,g==generation,s==sourceGeneration else {return}
            snapshot=result;failures=0;health="Connected · read-only"
        } catch {
            guard !closed,g==generation,s==sourceGeneration else {return}
            disconnected(error)
        }
    }
    public func preview(slot:Int) async throws {
        guard !closed,!busy else {throw PanelError.unavailable("Busy")}
        guard (0..<6).contains(slot),let native=configuration.slots[slot] ?? configuration.coordinators[slot]?.native else {
            throw PanelError.unavailable("Unassigned")
        }
        busy=true;preview=""
        defer {busy=false}
        let g=generation,s=sourceGeneration,m=metadataGeneration,source=configuration.source
        let coordinator=configuration.coordinators[slot]
        var readSucceeded=false
        do {
            let result=try await observed(source:source)
            readSucceeded=true
            guard !closed,g==generation,s==sourceGeneration,m==metadataGeneration,
                  native==(configuration.slots[slot] ?? configuration.coordinators[slot]?.native),
                  coordinator==configuration.coordinators[slot] else {throw PanelError.cancelled}
            let record:AgentRecord
            var qualifier=""
            if let coordinator {
                guard let directorySnapshot else {throw PanelError.unavailable("Refresh registered board metadata first")}
                record=try directorySnapshot.match(coordinator,snapshot:result,now:Date().timeIntervalSince1970)
                qualifier="\nLive surface matched (provider command not revalidated)."
            } else {record=try result.resolve(native,now:Date().timeIntervalSince1970)}
            snapshot=result;health="Connected · read-only";failures=0
            preview="Would target \(native.harness):\(native.sessionID)\n\(record.label) · \(record.paneID ?? "?") · revision \(record.revision.map(String.init) ?? "unknown")\(qualifier)\nLive focus unavailable: atomic identity-targeted transport not implemented."
        } catch {
            if !closed,g==generation,s==sourceGeneration,m==metadataGeneration {
                if !readSucceeded {disconnected(error)}
                preview="Preview unavailable: \(error.localizedDescription)"
                // A validation failure is not an agent crash or an idle observation.
                if snapshot?.source != source {snapshot=nil}
            }
            throw error
        }
    }
    public func edit(_ candidate:PanelConfiguration,expectedGeneration:UInt64) throws {
        guard !closed,expectedGeneration==generation else {throw PanelError.cancelled}
        try candidate.validate()
        try store?.save(candidate)
        let sourceChanged=candidate.source != configuration.source
        configuration=candidate;generation &+= 1;preview="";observation?.cancel()
        if sourceChanged {
            sourceGeneration &+= 1;snapshot=nil;health="Source changed · awaiting observation"
            metadataGeneration &+= 1;metadataTask?.cancel();metadataPending=nil;directorySnapshot=nil
        }
    }
    public func refreshMetadata(board:BoardSelection) async {
        guard !closed else {return}
        metadataGeneration &+= 1
        observation?.cancel();preview="";directorySnapshot=nil
        if metadataBusy {
            metadataPending=board;metadataTask?.cancel();metadataHealth="Replacing metadata read"
            return
        }
        metadataBusy=true
        var selected:BoardSelection?=board
        while let current=selected,!closed {
            metadataPending=nil
            let g=metadataGeneration,reader=directory
            let task=Task {try await reader.read(board:current)}
            metadataTask=task;metadataHealth="Reading local metadata"
            do {
                let result=try await task.value
                if !closed,g==metadataGeneration {directorySnapshot=result;metadataHealth="Registered metadata loaded · no provider execution"}
            } catch {
                if !closed,g==metadataGeneration {metadataHealth="Metadata unavailable: \(error.localizedDescription)"}
            }
            metadataTask=nil
            selected=metadataPending
        }
        metadataPending=nil;metadataBusy=false
    }
    public func record(slot:Int,now:Double=Date().timeIntervalSince1970) throws -> AgentRecord {
        guard (0..<6).contains(slot),let snapshot else {throw PanelError.unavailable("No fresh observation")}
        if let coordinator=configuration.coordinators[slot] {
            guard let directorySnapshot else {throw PanelError.unavailable("Registered metadata not refreshed")}
            return try directorySnapshot.match(coordinator,snapshot:snapshot,now:now)
        }
        guard let id=configuration.slots[slot] else {throw PanelError.unavailable("Unassigned")}
        return try snapshot.resolve(id,now:now)
    }
    public func reconcileMetadataCleanup() async {
        do {try await directory.runner.reconcileCleanup();metadataHealth="Cleanup reconciled; refresh explicitly"}
        catch {metadataHealth="Cleanup unavailable: \(error.localizedDescription)"}
    }
    public func useDemoMetadata(_ result:CoordinatorSnapshot) throws {
        guard sourceReader is DemoSource,!closed else {throw PanelError.invalid("Demo-only metadata")}
        metadataGeneration &+= 1;preview="";observation?.cancel()
        directorySnapshot=result;metadataHealth="Demo registered metadata"
    }
    public func shutdownAndWait() async throws {
        let read=observation,metadata=metadataTask
        shutdown()
        _ = try? await read?.value
        _ = try? await metadata?.value
        guard !(await directory.runner.hasUnconfirmedCleanup) else {
            health="Quit refused: cleanup unconfirmed. Reconcile metadata cleanup, then Quit again."
            metadataHealth="Supervision retained; metadata locked"
            throw PanelError.cleanupUnconfirmed
        }
    }
    public func shutdown() {
        closed=true;generation &+= 1;sourceGeneration &+= 1;metadataGeneration &+= 1
        stopObserving();metadataTask?.cancel();metadataPending=nil;snapshot=nil;preview=""
    }
}
