import SwiftUI
import BeckonPanelModel

struct PanelView:View {
    @ObservedObject var model:PanelModel
    let demo:DemoSource?
    let notice:String?
    let writable:Bool
    @State private var socket=""
    @State private var editError=""
    @State private var demoLabel="Working / idle"
    @State private var board=BoardSelection(executable:"",config:"",registry:"",boardConfig:"",alias:"",uid:"",ledger:"")
    @State private var showMetadata=false
    var body:some View {
        ScrollView {
            VStack(alignment:.leading,spacing:16) {
                HStack {
                    VStack(alignment:.leading) {
                        Text("Agent status").font(.largeTitle.bold())
                        Text(model.health).foregroundStyle(.secondary)
                    }
                    Spacer()
                    Button("Refresh") {Task {await model.refresh()}}.disabled(model.busy)
                    if let demo {
                        Button("Next demo") {
                            Task {await demo.next();demoLabel=await demo.label;await model.refresh()}
                        }.disabled(model.busy)
                    }
                }
                if let notice {Text(notice).foregroundStyle(.orange)}
                if demo != nil {Text("Demo · \(demoLabel) · changes stay in memory").font(.callout)}
                if model.store==nil {Text("Nonpersistent configuration").font(.caption).foregroundStyle(.secondary)}
                HStack {
                    TextField("Absolute Herdr socket",text:$socket)
                    Button("Apply source") {
                        edit { $0.source=SourceKey(socket:socket) }
                        Task {await model.refresh()}
                    }.disabled(!writable || demo != nil)
                }
                Text("Read-only observations. Live focus unavailable: atomic identity-targeted transport not implemented.")
                    .font(.callout).foregroundStyle(.secondary)
                TimelineView(.periodic(from:.now,by:0.5)) { timeline in
                    LazyVGrid(columns:[GridItem(.flexible()),GridItem(.flexible()),GridItem(.flexible())],spacing:12) {
                        ForEach(0..<6,id:\.self) {slot in slotView(slot,now:timeline.date.timeIntervalSince1970)}
                    }
                }
                if !model.preview.isEmpty {
                    Text(model.preview).font(.system(.callout,design:.monospaced))
                        .textSelection(.enabled).padding().frame(maxWidth:.infinity,alignment:.leading)
                        .background(.quaternary,in:RoundedRectangle(cornerRadius:10))
                }
                if !editError.isEmpty {Text(editError).foregroundStyle(.orange)}
                Text("Green reflects Herdr's server-reported done/unread state. Blocked does not identify an approval request.")
                    .font(.caption).foregroundStyle(.secondary)
                DisclosureGroup("Registered Trellis coordinators",isExpanded:$showMetadata) {
                    VStack(alignment:.leading,spacing:8) {
                        Text("Choose one existing board explicitly. Reads only paths, board-context and board-agents.")
                            .font(.caption).foregroundStyle(.secondary)
                        Group {
                            TextField("Absolute Trellis executable",text:$board.executable)
                            TextField("Absolute workflow config",text:$board.config)
                            TextField("Absolute boards registry",text:$board.registry)
                            TextField("Absolute board config",text:$board.boardConfig)
                            TextField("Board alias",text:$board.alias)
                            TextField("Board UID",text:$board.uid)
                            TextField("Absolute ledger",text:$board.ledger)
                        }.textFieldStyle(.roundedBorder)
                        HStack {
                            Button("Select and refresh board") {
                                do {
                                    try board.validate()
                                    edit { $0.boards=[board] }
                                    if editError.isEmpty {Task {await model.refreshMetadata(board:board)}}
                                } catch {editError=error.localizedDescription}
                            }.disabled(!writable || demo != nil)
                            Text(model.metadataHealth).font(.caption).foregroundStyle(.secondary)
                        }
                        Text("Assign a registered coordinator explicitly to a named slot. Labels do not select or relink agents; missing roles remain unassigned.")
                            .font(.caption).foregroundStyle(.secondary)
                        Button("Reconcile metadata cleanup") {Task {await model.reconcileMetadataCleanup()}}.disabled(demo != nil)
                        if let directory=model.directorySnapshot {
                            ForEach(Array(directory.coordinators.enumerated()),id:\.offset) {_,coordinator in
                                HStack {
                                    Text(coordinator.role).font(.callout)
                                    Spacer()
                                    Menu("Assign coordinator") {
                                        ForEach(0..<6,id:\.self) {slot in
                                            Button(model.configuration.slotTitle(slot)) {
                                                do {
                                                    let assignment=try directory.assignment(id:coordinator.id)
                                                    edit { $0.slots[slot]=nil;$0.coordinators[slot]=assignment }
                                                } catch {editError=error.localizedDescription}
                                            }
                                        }
                                    }.disabled(!writable)
                                }
                            }
                        }
                    }.padding(.top,8)
                }
            }.padding(24)
        }
        .onAppear {
            socket=model.configuration.source.socket
            if let selected=model.configuration.boards.first {board=selected}
        }
    }
    @ViewBuilder private func slotView(_ slot:Int,now:Double)->some View {
        let native=model.configuration.slots[slot] ?? model.configuration.coordinators[slot]?.native
        let record=try? model.record(slot:slot,now:now)
        let presentation=record?.presentation
        VStack(alignment:.leading,spacing:9) {
            HStack {
                Circle().fill(color(presentation?.color,native:native))
                    .frame(width:14,height:14)
                    .opacity(presentation?.pulse==true ? 0.45+0.55*abs(sin(now*2.5)) : 1)
                Text(model.configuration.slotTitle(slot)).font(.headline)
                Spacer()
            }
            Text(record?.label ?? (native == nil ? "Unassigned" : "Unavailable")).font(.title3)
            Text(presentation?.label ?? (native == nil ? "Off" : "Unknown / stale / unmatched"))
                .font(.caption).foregroundStyle(.secondary)
            if let native {
                Text("\(native.harness) · \(native.sessionID)")
                    .font(.system(.caption2,design:.monospaced)).lineLimit(2).textSelection(.enabled)
                if model.configuration.coordinators[slot] != nil {
                    Text(record == nil ? "Registered · unavailable" : "Live surface matched · provider not revalidated")
                        .font(.caption2).foregroundStyle(.secondary)
                }
            }
            HStack {
                Menu("Assign") {
                    Button("Unassigned") {edit {$0.slots[slot]=nil;$0.coordinators[slot]=nil}}
                    ForEach(Array((model.snapshot?.agents ?? []).enumerated()),id:\.offset) {_,agent in
                        if let id=agent.native {
                            Button("\(agent.label) · \(id.harness) · \(id.sessionID)") {
                                edit {$0.slots[slot]=id;$0.coordinators[slot]=nil}
                            }.disabled((try? model.snapshot?.resolve(id,now:now)) == nil)
                        } else {Text("\(agent.label) · no native identity")}
                    }
                }.disabled(!writable)
                Spacer()
                Button("Preview") {
                    Task {do {try await model.preview(slot:slot)} catch {editError=error.localizedDescription}}
                }.disabled(model.busy || native==nil || record==nil)
            }
            Button("Focus unavailable") {}.disabled(true).font(.caption)
        }
        .padding(14).frame(maxWidth:.infinity,minHeight:190,alignment:.topLeading)
        .background(.quaternary,in:RoundedRectangle(cornerRadius:12))
    }
    private func edit(_ change:(inout PanelConfiguration)->Void) {
        do {
            var candidate=model.configuration;change(&candidate)
            try model.edit(candidate,expectedGeneration:model.generation)
            editError=""
        } catch {editError=error.localizedDescription}
    }
    private func color(_ hex:String?,native:NativeID?)->Color {
        guard native != nil else {return .black}
        switch hex {
        case "#FFFFFF":return .white
        case "#3BA0FF":return Color(red:0.23,green:0.63,blue:1)
        case "#22C55E":return .green
        case "#FFB020":return .orange
        default:return .gray
        }
    }
}
