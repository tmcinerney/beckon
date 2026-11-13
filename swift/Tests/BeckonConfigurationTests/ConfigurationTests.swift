import XCTest
@testable import BeckonConfiguration

let configJSON = ##"{"config_version":3,"bindings":["key1","review","key11"],"devices":[{"id":"desk","path":"/fixture/config.d/desk.toml","definition":{"adapter":"creator-micro","options":{"layers":{"work":{"source":"rex"}}},"keys":{"joined":{"members":["desk:work/16","desk:work/17"],"action":"dictate","effects":"red"}},"actions":{"dictate":{"kind":"chord","keys":["ctrl","d"],"mode":"tap-hold","tap_ms":300}},"effects":{"red":{"held":{"color":"#FF0000","brightness":1,"motion":"steady"}}}}}],"display":{"theme":"night","themes":{"night":{"idle":{"color":"#123456","brightness":0.4,"motion":"steady"}},"day":{"idle":{"color":"#FFFFFF","brightness":1,"motion":"breathe"}}}}}"##
let statusJSON = #"{"bindings":[],"panes":[{"source":"rex","session":"work","pane_id":"same","title":"Review","binding":"","agent_status":"working","focused":false},{"source":"herdr","session":"work","pane_id":"same","title":"Other","binding":"","agent_status":"idle","focused":false}]}"#
let identity = AgentIdentity(source: "rex", session: "work", pane_id: "same")
func ok(_ json: String) -> Data { Data("{\"type\":\"ok\",\"data\":\(json)}\n".utf8) }

actor MockTransport: Transport {
    var requests: [Request] = []
    var mutationError = false
    var readError = false
    var theme = "night"
    var occupied = false
    var failAfterMutation = false
    var malformedReceipt = false
    func setMutationError() { mutationError = true }
    func setReadError() { readError = true }
    func setOccupied() { occupied = true }
    func setFailAfterMutation() { failAfterMutation = true }
    func setMalformedReceipt() { malformedReceipt = true }
    func send(_ request: Request, to endpoint: String) async throws -> Data {
        requests.append(request)
        switch request {
        case .configModel:
            if readError { throw ClientError.timeout }
            return ok(configJSON.replacingOccurrences(of: #""theme":"night""#, with: "\"theme\":\"\(theme)\""))
        case .status:
            if occupied {
                return ok(statusJSON.replacingOccurrences(of: #""bindings":[]"#, with: #""bindings":[{"key":"key1","source":"rex","session":"work","pane":{"source":"rex","session":"work","pane_id":"same","agent_status":"working"}}]"#))
            }
            return ok(statusJSON)
        default:
            if mutationError { return Data(#"{"type":"error","message":"key is occupied"}"#.utf8) }
            if malformedReceipt { return ok("{}") }
            if failAfterMutation { readError = true }
            switch request {
            case .setTheme(let name): theme = name; return ok("{\"theme\":\"\(name)\"}")
            case .bind(let key, let agent):
                return ok("{\"key\":\"\(key)\",\"session\":\"\(agent.session)\",\"pane_id\":\"\(agent.pane_id)\",\"changed\":true}")
            case .releaseKey(let key): return ok("{\"key\":\"\(key)\",\"session\":null,\"pane_id\":null,\"changed\":false}")
            default: return ok("{}")
            }
        }
    }
}

final class ConfigurationTests: XCTestCase {
    func testArbitraryDeviceAndBindingCounts() throws {
        var config = try JSONDecoder().decode(Configuration.self, from: Data(configJSON.utf8))
        let device = config.devices[0]
        config.devices = (0..<19).map { index in var copy = device; copy.id = "pad-\(index)"; return copy }
        config.bindings = (0..<47).map { "configured-\($0)" }
        try config.validate()
        let roundTrip = try JSONDecoder().decode(Configuration.self, from: JSONEncoder().encode(config))
        XCTAssertEqual(roundTrip.devices.count, 19); XCTAssertEqual(roundTrip.bindings.count, 47)
    }
    func testConfiguredNamesGroupingAndIdentityAreData() throws {
        let config = try JSONDecoder().decode(Configuration.self, from: Data(configJSON.utf8))
        try config.validate()
        XCTAssertEqual(config.bindings, ["key1", "review", "key11"])
        XCTAssertEqual(config.devices[0].definition.keys["joined"]?.members.count, 2)
        XCTAssertEqual(config.devices[0].definition.actions["dictate"]?.tap_ms, 300)
        let status = try JSONDecoder().decode(Status.self, from: Data(statusJSON.utf8))
        XCTAssertNotEqual(status.panes[0].id, status.panes[1].id)
        let data = try JSONEncoder().encode(Request.bind(key: "review", agent: identity))
        let json = try XCTUnwrap(JSONSerialization.jsonObject(with: data) as? [String: String])
        XCTAssertEqual(json, ["op":"bind", "key":"review", "source":"rex", "session":"work", "pane_id":"same"])
    }
    func testEndpointDiscoveryMatchesDaemon() throws {
        XCTAssertEqual(Endpoint.discover(environment: ["XDG_RUNTIME_DIR":"/runtime", "HOME":"/home"]), "/runtime/beckon.sock")
        XCTAssertEqual(Endpoint.discover(environment: ["XDG_STATE_HOME":"/state", "HOME":"/home"]), "/state/beckon/beckon.sock")
        XCTAssertEqual(Endpoint.discover(environment: ["HOME":"/home"]), "/home/.local/state/beckon/beckon.sock")
        XCTAssertEqual(Endpoint.discover(environment: [:], temporary: "/temporary"), "/temporary/beckon/beckon.sock")
        for path in ["relative", "/bad\0path", "/" + String(repeating: "x", count: 104)] { XCTAssertThrowsError(try Endpoint.validate(path)) }
    }
    func testInvalidTreatmentsAndEnvelopes() throws {
        XCTAssertThrowsError(try Treatment(color: "red", brightness: 1, motion: .steady).validate())
        XCTAssertThrowsError(try Treatment(color: "#FFFFFF", brightness: .nan, motion: .steady).validate())
        XCTAssertThrowsError(try Treatment(color: "#FFFFFF", brightness: 1.01, motion: .steady).validate())
        XCTAssertThrowsError(try JSONDecoder().decode(Reply<Configuration>.self, from: Data(#"{"type":"ok"}"#.utf8)))
        XCTAssertThrowsError(try JSONDecoder().decode(Reply<JSONValue>.self, from: Data(#"{"type":"wat","data":{}}"#.utf8)))
    }
    @MainActor func testConnectMutationAcknowledgementAndReconnect() async {
        let mock = MockTransport()
        let client = ConfigurationModel(endpoint: "/fixture.sock", transport: mock)
        XCTAssertFalse(client.canMutate)
        await client.connect()
        XCTAssertTrue(client.canMutate)
        await client.setTheme("day")
        XCTAssertEqual(client.configuration?.display.theme, "day")
        XCTAssertNotNil(client.notice)
        client.disconnect()
        XCTAssertFalse(client.canMutate)
        XCTAssertEqual(client.configuration?.display.theme, "day")
        await client.connect()
        XCTAssertTrue(client.canMutate)
    }
    @MainActor func testFailureHasNoOptimisticSuccessAndStaleBlocksWrites() async {
        let mock = MockTransport()
        let model = ConfigurationModel(endpoint: "/fixture.sock", transport: mock)
        await model.connect(); await mock.setMutationError(); await model.setTheme("day")
        XCTAssertEqual(model.configuration?.display.theme, "night")
        XCTAssertNil(model.notice); XCTAssertNotNil(model.error); XCTAssertFalse(model.canMutate)
        let count = await mock.requests.count
        await model.bind(key: "review", agent: identity)
        let after = await mock.requests.count
        XCTAssertEqual(count, after)
        await model.setTheme("missing")
        XCTAssertEqual(model.error, "Select a configured theme.")
    }
    @MainActor func testOccupiedAndAlreadyBoundOwnersAreNotMoved() async {
        let mock = MockTransport(); await mock.setOccupied()
        let model = ConfigurationModel(endpoint: "/fixture.sock", transport: mock)
        await model.connect()
        await model.bind(key: "key1", agent: identity)
        XCTAssertTrue(model.error?.contains("occupied") == true)
        await model.bind(key: "review", agent: identity)
        XCTAssertTrue(model.error?.contains("already owns") == true)
        let requests = await mock.requests
        XCTAssertEqual(requests, [.configModel, .status])
    }
    @MainActor func testExpiryEndpointEditAndRefreshFailureDisableWrites() async {
        var now = Date(timeIntervalSince1970: 100)
        let mock = MockTransport()
        let client = ConfigurationModel(endpoint: "/fixture.sock", transport: mock, clock: { now })
        await client.connect(); now.addTimeInterval(16)
        XCTAssertFalse(client.canMutate)
        await client.bind(key: "review", agent: identity)
        XCTAssertNotNil(client.error)
        await client.refresh(); XCTAssertTrue(client.canMutate)
        client.endpoint = "/other.sock"; XCTAssertFalse(client.canMutate)
        client.endpoint = "/fixture.sock"; await mock.setReadError(); await client.refresh()
        XCTAssertTrue(client.stale); XCTAssertNotNil(client.configuration)
        XCTAssertFalse(client.canMutate)
    }
    @MainActor func testAcknowledgedButFailedReadbackIsNotVerifiedSuccess() async {
        let mock = MockTransport()
        let client = ConfigurationModel(endpoint: "/fixture.sock", transport: mock)
        await client.connect(); await mock.setFailAfterMutation(); await client.setTheme("day")
        XCTAssertNil(client.notice); XCTAssertTrue(client.error?.contains("verification failed") == true)
        XCTAssertEqual(client.configuration?.display.theme, "night")
    }
    @MainActor func testInvalidSelectionAndMalformedAcknowledgementSendNoFollowupMutation() async {
        let mock = MockTransport()
        let model = ConfigurationModel(endpoint: "/fixture.sock", transport: mock)
        await model.connect()
        await model.bind(key: "not-configured", agent: identity)
        await model.bind(key: "review", agent: AgentIdentity(source: "missing", session: "work", pane_id: "same"))
        await model.release(key: "not-configured")
        let before = await mock.requests
        XCTAssertEqual(before, [.configModel, .status])
        await mock.setMalformedReceipt(); await model.setTheme("day")
        XCTAssertEqual(model.configuration?.display.theme, "night")
        XCTAssertTrue(model.error?.contains("outcome unknown") == true); XCTAssertNil(model.notice)
    }
    @MainActor func testExplicitReleaseAndBindUseOnlySelectedKey() async {
        let mock = MockTransport()
        let client = ConfigurationModel(endpoint: "/fixture.sock", transport: mock)
        await client.connect(); await client.bind(key: "review", agent: identity)
        XCTAssertNotNil(client.notice)
        await client.release(key: "review")
        let requests = await mock.requests
        XCTAssertEqual(requests, [.configModel, .status, .bind(key: "review", agent: identity), .configModel, .status,
                                  .releaseKey("review"), .configModel, .status])
    }
}

private actor SuspendedTransport: Transport {
    var continuation: CheckedContinuation<Data, Never>?
    var started = false
    func send(_ request: Request, to endpoint: String) async throws -> Data {
        if request == .configModel {
            started = true
            return await withCheckedContinuation { continuation = $0 }
        }
        return ok(statusJSON)
    }
    func complete() { continuation?.resume(returning: ok(configJSON)); continuation = nil }
}
final class LifecycleTests: XCTestCase {
    @MainActor func testLateReplyCannotReconnectOrPublishAfterDisconnect() async {
        let mock = SuspendedTransport()
        let model = ConfigurationModel(endpoint: "/fixture.sock", transport: mock)
        let connect = Task { await model.connect() }
        while !(await mock.started) { await Task.yield() }
        model.disconnect(); await mock.complete(); await connect.value
        XCTAssertNil(model.configuration); XCTAssertNil(model.connectedEndpoint)
        XCTAssertNil(model.notice); XCTAssertFalse(model.busy)
    }
}
