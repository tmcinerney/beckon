import XCTest
@testable import BeckonConfiguration

final class PresentationTests: XCTestCase {
    private let treatment = Treatment(color: "#ABCDEF", brightness: 0.6, motion: .pulse)

    private func encoded(_ request: Request) throws -> JSONValue {
        try JSONDecoder().decode(JSONValue.self, from: JSONEncoder().encode(request))
    }
    private func json(_ text: String) throws -> JSONValue {
        try JSONDecoder().decode(JSONValue.self, from: Data(text.utf8))
    }
    func testExactThemeStateEnvelope() throws {
        let request = Request.setPresentation(expectedRevision: "opaque:42", change: .themeState(
            theme: "night", state: .working, treatment: treatment))
        XCTAssertEqual(try encoded(request), try json(##"{"op":"set_presentation","expected_revision":"opaque:42","change":{"kind":"theme_state","theme":"night","state":"working","treatment":{"color":"#ABCDEF","brightness":0.6,"motion":"pulse"}}}"##))
    }
    func testExactDeviceEffectReplacementIncludesNulls() throws {
        let request = Request.setPresentation(expectedRevision: "r", change: .deviceEffect(
            device: "desk", effect: "dictation", effects: Feedback(held: treatment)))
        XCTAssertEqual(try encoded(request), try json(##"{"op":"set_presentation","expected_revision":"r","change":{"kind":"device_effect","device":"desk","effect":"dictation","effects":{"idle":null,"held":{"color":"#ABCDEF","brightness":0.6,"motion":"pulse"},"latched":null}}}"##))
    }
    func testExactDeviceChannelEnvelope() throws {
        let request = Request.setPresentation(expectedRevision: "r", change: .deviceChannel(
            device: "desk", channel: "ring", treatment: treatment))
        XCTAssertEqual(try encoded(request), try json(##"{"op":"set_presentation","expected_revision":"r","change":{"kind":"device_channel","device":"desk","channel":"ring","treatment":{"color":"#ABCDEF","brightness":0.6,"motion":"pulse"}}}"##))
    }
    func testExactFencedReleaseEnvelope() throws {
        XCTAssertEqual(try encoded(.releaseBinding(key: "review", reference: identity)),
                       try json(#"{"op":"release_binding","key":"review","reference":{"source":"rex","session":"work","pane_id":"same"}}"#))
    }
    func testEveryPresentationVariantValidatesTreatments() {
        let invalid = Treatment(color: "#FFFFFF", brightness: -0.1, motion: .steady)
        for change in [PresentationChange.themeState(theme: "night", state: .idle, treatment: invalid),
                       .deviceEffect(device: "desk", effect: "dictation", effects: Feedback(latched: invalid)),
                       .deviceChannel(device: "desk", channel: "base", treatment: invalid)] {
            XCTAssertThrowsError(try change.validate())
        }
    }
}

private actor PresentationFixture: Transport {
    enum Outcome { case success, conflict, persistenceFailure, disconnected, malformed, missingRevision }
    var config: Configuration
    var requests: [Request] = []
    var outcome: Outcome = .success
    init() throws {
        config = try JSONDecoder().decode(Configuration.self, from: Data(configJSON.utf8))
        config.revision = "r1"
        config.capabilities = try JSONDecoder().decode(Capabilities.self, from: Data(capabilitiesJSON.utf8))
        config.devices[0].definition.options["base"] = try JSONDecoder().decode(JSONValue.self, from: Data(##"{"color":"#112233","brightness":0.2,"motion":"steady"}"##.utf8))
        config.devices[0].definition.options["ring"] = config.devices[0].definition.options["base"]
    }
    func setOutcome(_ value: Outcome) { outcome = value }
    func setCapabilities(_ capabilities: Capabilities?) { config.capabilities = capabilities }
    func send(_ request: Request, to endpoint: String) async throws -> Data {
        requests.append(request)
        switch request {
        case .configModel: return try encodedConfig()
        case .status: return ok(statusJSON)
        case .setPresentation(let revision, let change):
            guard revision == config.revision else { return Data(#"{"type":"error","message":"revision conflict"}"#.utf8) }
            switch outcome {
            case .conflict: return Data(#"{"type":"error","message":"revision conflict"}"#.utf8)
            case .persistenceFailure: return Data(#"{"type":"error","message":"settings persistence failed"}"#.utf8)
            case .disconnected: throw ClientError.unavailable("fixture disconnected")
            case .malformed: return ok("{}")
            case .missingRevision: config.revision = nil; return try encodedConfig()
            case .success:
                if case .themeState(let theme, let state, var treatment) = change {
                    // Canonicalization proves the model uses the reply rather than its draft.
                    treatment.color = treatment.color.uppercased()
                    config.display.themes[theme]?[state.rawValue] = treatment
                }
                if case .deviceEffect(let device, let effect, let effects) = change,
                   let index = config.devices.firstIndex(where: { $0.id == device }) {
                    config.devices[index].definition.effects[effect] = effects
                }
                if case .deviceChannel(let device, let channel, let treatment) = change,
                   let index = config.devices.firstIndex(where: { $0.id == device }) {
                    config.devices[index].definition.options[channel] = try JSONDecoder().decode(JSONValue.self, from: JSONEncoder().encode(treatment))
                }
                config.revision = "r2"
                return try encodedConfig()
            }
        default: throw ClientError.invalid("Unexpected fixture operation")
        }
    }
    private func encodedConfig() throws -> Data { ok(String(decoding: try JSONEncoder().encode(config), as: UTF8.self)) }
}
final class PresentationModelTests: XCTestCase {
    private var change: PresentationChange {
        .themeState(theme: "night", state: .idle, treatment: Treatment(color: "#abcdef", brightness: 0.5, motion: .breathe))
    }
    @MainActor func testCanonicalResponseAndRevisionReplaceDraftWithoutExtraReads() async throws {
        let fixture = try PresentationFixture()
        let model = ConfigurationModel(endpoint: "/fixture.sock", transport: fixture)
        await model.connect()
        let observed = model.lastUpdated
        await model.setPresentation(change, expectedRevision: "r1")
        XCTAssertEqual(model.configuration?.revision, "r2")
        XCTAssertEqual(model.configuration?.display.themes["night"]?["idle"]?.color, "#ABCDEF")
        XCTAssertEqual(model.lastUpdated, observed)
        XCTAssertNotNil(model.notice)
        let requests = await fixture.requests
        XCTAssertEqual(requests, [.configModel, .status, .setPresentation(expectedRevision: "r1", change: change)])
    }
    @MainActor func testConflictsPersistenceAndTransportFailuresNeverPublishSuccess() async throws {
        for outcome in [PresentationFixture.Outcome.conflict, .persistenceFailure, .disconnected, .malformed, .missingRevision] {
            let fixture = try PresentationFixture()
            let model = ConfigurationModel(endpoint: "/fixture.sock", transport: fixture)
            await model.connect(); await fixture.setOutcome(outcome)
            let previous = model.configuration
            await model.setPresentation(change, expectedRevision: "r1")
            XCTAssertEqual(model.configuration, previous)
            XCTAssertNil(model.notice); XCTAssertNotNil(model.error); XCTAssertFalse(model.canMutate)
            let requests = await fixture.requests
            XCTAssertEqual(requests.count, 3, "Writes must not retry or silently refresh conflicts")
        }
    }
    @MainActor func testStaleDraftInvalidTreatmentAndDisconnectedDoNotDispatch() async throws {
        let fixture = try PresentationFixture()
        let model = ConfigurationModel(endpoint: "/fixture.sock", transport: fixture)
        await model.setPresentation(change, expectedRevision: "r1")
        await model.connect()
        await model.setPresentation(change, expectedRevision: "obsolete")
        XCTAssertTrue(model.error?.contains("draft is stale") == true)
        await model.setPresentation(.themeState(theme: "night", state: .idle,
            treatment: Treatment(color: "bad", brightness: 2, motion: .steady)), expectedRevision: "r1")
        let requests = await fixture.requests
        XCTAssertEqual(requests, [.configModel, .status])
        XCTAssertNil(model.notice)
    }
    @MainActor func testCapabilityAndConfiguredTargetGuards() async throws {
        let fixture = try PresentationFixture()
        let model = ConfigurationModel(endpoint: "/fixture.sock", transport: fixture)
        await model.connect()
        let valid = Treatment(color: "#000000", brightness: 0, motion: .steady)
        for change in [PresentationChange.themeState(theme: "missing", state: .idle, treatment: valid),
                       .deviceEffect(device: "desk", effect: "missing", effects: Feedback(held: valid)),
                       .deviceChannel(device: "desk", channel: "unadvertised", treatment: valid),
                       .deviceChannel(device: "missing", channel: "ring", treatment: valid)] {
            await model.setPresentation(change, expectedRevision: "r1")
            XCTAssertTrue(model.error?.contains("not supported") == true)
        }
        let before = await fixture.requests
        XCTAssertEqual(before, [.configModel, .status])
        await fixture.setCapabilities(nil); await model.refresh()
        await model.setPresentation(change, expectedRevision: "r1")
        let after = await fixture.requests
        XCTAssertEqual(after, [.configModel, .status, .configModel, .status])
        XCTAssertFalse(model.canRelease)
    }
    @MainActor func testEffectAndChannelUseConfiguredNamesAndCanonicalModels() async throws {
        let fixture = try PresentationFixture()
        let model = ConfigurationModel(endpoint: "/fixture.sock", transport: fixture)
        await model.connect()
        let originalKeys = model.configuration?.devices[0].definition.keys
        let treatment = Treatment(color: "#555555", brightness: 0.2, motion: .flicker)
        await model.setPresentation(.deviceEffect(device: "desk", effect: "red", effects: Feedback(latched: treatment)), expectedRevision: "r1")
        XCTAssertNil(model.configuration?.devices[0].definition.effects["red"]?.held)
        XCTAssertEqual(model.configuration?.devices[0].definition.effects["red"]?.latched, treatment)
        await model.setPresentation(.deviceChannel(device: "desk", channel: "ring", treatment: treatment), expectedRevision: "r2")
        XCTAssertEqual(model.configuration?.devices[0].definition.channelTreatment("ring"), treatment)
        XCTAssertEqual(model.configuration?.devices[0].definition.keys, originalKeys)
        XCTAssertNotNil(model.notice)
    }
}
