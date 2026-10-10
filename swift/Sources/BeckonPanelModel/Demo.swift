import Foundation

public actor DemoSource:AgentStateSource {
    public private(set) var step=0
    public init() {}
    public func next() {step=(step+1)%8}
    public var label:String {
        ["Working / idle","Done / working","Blocked / done","Unknown / idle",
         "Disconnected","Reconnected","Reordered agents","Coordinator pane mismatch"][step]
    }
    public func read(source:SourceKey,budget:Double) async throws -> AgentSnapshot {
        if step==4 {throw PanelError.unavailable("Demo disconnected")}
        let statuses=[["working","idle"],["done","working"],["blocked","done"],["future-state","idle"],
                      ["unknown","unknown"],["idle","working"],["idle","working"],["working","idle"]][step]
        var records:[AgentRecord]=[]
        for i in 0..<2 {
            let suffix=i==0 ? "a" : "b"
            let harness=i==0 ? "codex" : "claude"
            let data=try JSONSerialization.data(withJSONObject:[
                "agent_session":["agent":harness,"kind":"id","source":"herdr:\(harness)","value":"demo-\(suffix)"],
                "terminal_id":"demo-terminal-\(suffix)","pane_id":step==7 && i==0 ? "moved-pane" : "demo-pane-\(suffix)",
                "agent_status":statuses[i],"name":i==0 ? "Demo coordinator" : "Demo feature worker",
                "revision":step+1,"completion_seq":i+1
            ])
            records.append(try JSONDecoder().decode(AgentRecord.self,from:data))
        }
        if step==6 {records.reverse()}
        return AgentSnapshot(source:source,observedAt:Date().timeIntervalSince1970,agents:records)
    }
    public static var board:BoardSelection {
        BoardSelection(executable:"/demo/trellis",config:"/demo/config",registry:"/demo/boards",boardConfig:"/demo/board",
                       alias:"demo",uid:"demo-board",ledger:"/demo/ledger")
    }
    public static func directorySnapshot() throws -> CoordinatorSnapshot {
        try CoordinatorSnapshot.decode(
            paths:Data(#"{"ok":true,"data":{"config":"/demo/config","ledger":"/demo/ledger"}}"#.utf8),
            context:Data(#"{"ok":true,"data":{"board":{"id":"demo-board"},"selection":{"name":"demo","registry":"/demo/boards","board_config":"/demo/board","db":"/demo/ledger"}}}"#.utf8),
            agents:Data(#"{"ok":true,"data":{"registered_coordinators":[{"id":"demo-coordinator","role":"Demo coordinator","link":{"provider":"herdr","stable_id":"demo-a","target":{"session":"default","agent":"demo-a"},"identity":{"agent_session":{"agent":"codex","kind":"id","source":"herdr:codex","value":"demo-a"},"terminal_id":"demo-terminal-a","pane_id":"demo-pane-a"}}}]}}"#.utf8),
            board:board)
    }
}
public struct PanelOptions:Equatable {
    public var demo=false
    public var noSave=false
    public var socket:String
    public var config:String
    public init(arguments:[String],home:String) throws {
        socket=home+"/.config/herdr/herdr.sock";config=home+"/.config/beckon/control-panel.json"
        var index=0
        while index<arguments.count {
            let flag=arguments[index]
            switch flag {
            case "--demo":demo=true
            case "--no-save":noSave=true
            case "--herdr-socket","--config":
                index+=1
                guard index<arguments.count,arguments[index].hasPrefix("/"),!arguments[index].utf8.contains(0) else {
                    throw PanelError.invalid("Expected an absolute path after \(flag)")
                }
                if flag=="--config" {config=arguments[index]} else {socket=arguments[index]}
            default:throw PanelError.invalid("Unknown option \(flag)")
            }
            index+=1
        }
        try SourceKey(socket:socket).validate()
    }
}
