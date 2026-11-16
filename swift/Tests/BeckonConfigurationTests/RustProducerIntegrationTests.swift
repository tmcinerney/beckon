import CryptoKit
import XCTest
@testable import BeckonConfiguration

/// Actual Rust producer-v3 bytes, deliberately separate from synthetic consumer inputs.
enum RustProducerFixture {
    static let names = ["runtime-v1", "runtime-v1-pending", "runtime-v1-fired", "runtime-v1-denied",
                        "runtime-v1-recovered", "runtime-v1-held", "runtime-v1-latched", "runtime-v1-stale",
                        "runtime-v1-invalid-ledger", "runtime-unsupported"]
    static let manifestHash = "8954064ff7869cbf0869f19dcf7c75fca619bf3b3a496a0b598c80ac723bded9"
    static func digest(_ data: Data) -> String { SHA256.hash(data: data).map { String(format: "%02x", $0) }.joined() }
    static func expectedHashes() throws -> [String: String] {
        let url = try XCTUnwrap(Bundle.module.url(forResource: "producer-manifest", withExtension: "sha256", subdirectory: "RustProducerV3"))
        let bytes = try Data(contentsOf: url)
        guard digest(bytes) == manifestHash else { throw ClientError.invalid("Rust producer manifest identity changed.") }
        return Dictionary(uniqueKeysWithValues: String(decoding: bytes, as: UTF8.self).split(separator: "\n").compactMap { line in
            let fields = line.split(maxSplits: 1, whereSeparator: \.isWhitespace)
            guard fields.count == 2 else { return nil }
            let path = fields[1].trimmingCharacters(in: .whitespaces)
            guard path.hasPrefix("producer-v3/tests/fixtures/") else { return nil }
            return (URL(fileURLWithPath: path).deletingPathExtension().lastPathComponent, String(fields[0]))
        })
    }
    static func bytes(_ name: String) throws -> Data {
        let url = try XCTUnwrap(Bundle.module.url(forResource: name, withExtension: "json", subdirectory: "RustProducerV3/fixtures"))
        let bytes = try Data(contentsOf: url)
        let expected = try expectedHashes()[name]
        guard digest(bytes) == expected else { throw ClientError.invalid("Rust producer fixture bytes changed: \(name)") }
        return bytes
    }
    static func snapshot(_ name: String = "runtime-v1") throws -> RuntimeSnapshot {
        let snapshot = try JSONDecoder().decode(Reply<RuntimeSnapshot>.self, from: bytes(name)).value()
        try snapshot.validate()
        return snapshot
    }
}

actor RustProducerTransport: Transport {
    private var runtimeBytes: Data
    private let bootstrap: Data
    private(set) var requests: [Request] = []
    init(_ name: String) throws {
        runtimeBytes = try RustProducerFixture.bytes(name)
        // Adapter-only bootstrap, not advertised as real Rust config_model envelope bytes.
        struct Bootstrap: Encodable { let type = "ok"; let data: Configuration }
        let sample = try RustProducerFixture.snapshot(name == "runtime-unsupported" ? "runtime-v1" : name)
        bootstrap = try JSONEncoder().encode(Bootstrap(data: sample.configuration))
    }
    func replay(_ name: String) throws { runtimeBytes = try RustProducerFixture.bytes(name) }
    func send(_ request: Request, to endpoint: String) async throws -> Data {
        requests.append(request)
        switch request {
        case .configModel: return bootstrap
        case .runtimeSnapshot: return runtimeBytes
        default: throw ClientError.invalid("Forbidden non-runtime read/write during actual Rust producer replay")
        }
    }
}

final class RustProducerIntegrationTests: XCTestCase {
    @MainActor func testActualPhaseProgressionAndLateOlderSampleUseExistingOrderingFences() async throws {
        let transport = try RustProducerTransport("runtime-v1-pending"), scheduler = ManualRefreshScheduler()
        let model = ConfigurationModel(endpoint: "/rust-producer-fixture-only.sock", transport: transport,
                                       clock: { Date(timeIntervalSince1970: 100) }, scheduler: scheduler)
        await model.connect(); await settleTasks()
        XCTAssertEqual(model.runtime?.sequence, 1)
        for (name, sequence) in [("runtime-v1-fired", 2), ("runtime-v1-denied", 3), ("runtime-v1-recovered", 4),
                                 ("runtime-v1-held", 5), ("runtime-v1-latched", 6)] {
            try await transport.replay(name); await scheduler.advance(); await settleTasks()
            XCTAssertEqual(model.runtime?.sequence, UInt64(sequence), name)
            XCTAssertEqual(model.runtime?.configuration_revision, model.configuration?.revision, name)
        }
        let revision = model.configuration?.revision
        try await transport.replay("runtime-v1-pending"); await scheduler.advance(); await settleTasks()
        XCTAssertEqual(model.runtime?.sequence, 6)
        XCTAssertEqual(model.configuration?.revision, revision)
        XCTAssertNotNil(model.runtimeError)
        XCTAssertFalse(model.canMutate)
        let requests = await transport.requests
        XCTAssertEqual(requests, [.configModel] + Array(repeating: .runtimeSnapshot, count: 7))
        model.disconnect()
    }

    func testAllTenBundledProducerFilesKeepExactBytesHashesAndNewlineFraming() throws {
        XCTAssertEqual(Set(try RustProducerFixture.expectedHashes().keys), Set(RustProducerFixture.names))
        for name in RustProducerFixture.names {
            let bytes = try RustProducerFixture.bytes(name)
            XCTAssertEqual(bytes.last, 10, name)
            XCTAssertEqual(bytes.filter { $0 == 10 }.count, 1, name)
        }
    }

    func testActualSuccessEnvelopesDecodeActionsFeedbackConfigurationAndIdentifiers() throws {
        for name in RustProducerFixture.names where name != "runtime-unsupported" {
            let sample = try RustProducerFixture.snapshot(name)
            XCTAssertEqual(sample.version, 1, name)
            XCTAssertEqual(sample.configuration_revision, sample.configuration.revision, name)
            XCTAssertEqual(sample.configuration.bindings, (1...10).map { "key\($0)" }, name)
            XCTAssertEqual(sample.bindings.map(\.key), sample.configuration.bindings, name)
            XCTAssertEqual(sample.configuration.sources, ["herdr", "rex"], name)
            XCTAssertEqual(sample.configuration.capabilities?.runtime_snapshot_versions, [1], name)
            XCTAssertEqual(sample.configuration.capabilities?.status_read_only, true, name)
            XCTAssertEqual(Set(sample.configuration.capabilities?.feedback_states ?? []),
                           Set(["idle", "held", "latched", "pending", "hold_fired", "error"]), name)
            XCTAssertNil(sample.build.revision); XCTAssertNil(sample.build.dirty)
            let device = try XCTUnwrap(sample.configuration.devices.first)
            XCTAssertEqual(device.id, "creator-micro", name)
            XCTAssertEqual(device.definition.keys["microphone"]?.members.count, 8, name)
            XCTAssertEqual(device.definition.keys["send-or-newline"]?.members.count, 4, name)
            XCTAssertEqual(device.definition.actions["send-or-newline"],
                           .tapHoldChords(tapKeys: ["enter"], holdKeys: ["shift", "enter"], holdMilliseconds: 300), name)
            let mic = try XCTUnwrap(device.definition.actions["dictate"])
            let latchConfiguration = ["runtime-v1-held", "runtime-v1-latched"].contains(name)
            XCTAssertEqual(mic, .chord(keys: ["ctrl", "opt", "d"], mode: latchConfiguration ? "tap-hold" : "hold",
                                      tapMilliseconds: latchConfiguration ? 300 : nil), name)
            let effect = try XCTUnwrap(device.definition.effects["enter-cue"])
            XCTAssertNoThrow(try effect.validate(), name)
            XCTAssertNil(effect.idle); XCTAssertNil(effect.held); XCTAssertNil(effect.latched)
            XCTAssertEqual(effect.pending?.color, "#FFFFFF", name)
            XCTAssertEqual(try XCTUnwrap(effect.pending?.brightness), 0.12, accuracy: 0.00001, name)
            XCTAssertEqual(effect.hold_fired?.color, "#3388FF", name)
            XCTAssertEqual(effect.error?.treatment.color, "#FFB000", name)
            XCTAssertEqual(effect.error?.duration_ms, 650, name)
            XCTAssertFalse(sample.configuration.supports(.deviceEffect(device: device.id, effect: "enter-cue", effects: effect)), name)
            XCTAssertTrue(sample.configuration.supports(.deviceEffectV2(device: device.id, effect: "enter-cue", effects: effect)), name)
            if latchConfiguration {
                XCTAssertNotNil(device.definition.effects["dictation"]?.idle)
                XCTAssertEqual(device.definition.effects["dictation"]?.latched?.motion, .breathe)
            }
        }
    }

    @MainActor func testActualEnginePendingFiredErrorRecoveredHeldAndLatchedPhasesThroughModel() async throws {
        let cases: [(String, String, String, Bool)] = [
            ("runtime-v1-pending", "send-or-newline", "pending", true),
            ("runtime-v1-fired", "send-or-newline", "hold_fired", true),
            ("runtime-v1-denied", "send-or-newline", "error", true),
            ("runtime-v1-recovered", "send-or-newline", "idle", false),
            ("runtime-v1-held", "microphone", "held", true),
            ("runtime-v1-latched", "microphone", "latched", false)
        ]
        for (name, control, phase, pressed) in cases {
            let transport = try RustProducerTransport(name)
            let model = ConfigurationModel(endpoint: "/rust-producer-fixture-only.sock", transport: transport,
                                           clock: { Date(timeIntervalSince1970: 100) }, scheduler: ManualRefreshScheduler())
            await model.connect()
            let sample = try XCTUnwrap(model.runtime, name)
            let observed = try XCTUnwrap(sample.devices.first?.controls.first { $0.id == control }, name)
            XCTAssertEqual(observed.phase, phase, name)
            XCTAssertEqual(observed.pressed, pressed, name)
            XCTAssertFalse(observed.cleanup_pending, name)
            XCTAssertFalse(sample.devices[0].observation_supported, name)
            XCTAssertEqual(sample.devices[0].connectivity, .unknown, name)
            XCTAssertNil(model.error, name)
            let requests = await transport.requests
            XCTAssertEqual(requests, [.configModel, .runtimeSnapshot], name)
            model.disconnect()
        }
    }

    @MainActor func testActualLiveDormantScopeLabelsAndSourcesStayQualifiedWithoutReadSideEffects() async throws {
        let now = Date(timeIntervalSince1970: 100), transport = try RustProducerTransport("runtime-v1")
        let scheduler = ManualRefreshScheduler()
        let model = ConfigurationModel(endpoint: "/rust-producer-fixture-only.sock", transport: transport, clock: { now }, scheduler: scheduler)
        await model.connect(); await settleTasks()
        let rows = model.bindingLegend(at: now)
        XCTAssertEqual(rows.count, 10)
        XCTAssertEqual(rows[0].owner, AgentIdentity(source: "rex", session: "same-scope", pane_id: "same-pane"))
        XCTAssertEqual(rows[1].owner, AgentIdentity(source: "herdr", session: "same-scope", pane_id: "same-pane"))
        XCTAssertEqual(rows[0].availability, .live); XCTAssertFalse(rows[0].labelsStale)
        XCTAssertEqual(rows[0].scopeTitle, "Engineering"); XCTAssertEqual(rows[0].title, "Build engine")
        XCTAssertEqual(rows[1].availability, .dormant); XCTAssertTrue(rows[1].labelsStale)
        XCTAssertTrue(rows[1].summary.contains("Historic workspace"))
        XCTAssertEqual(model.runtime?.bindings[0].labels_age_ms, 100)
        XCTAssertEqual(model.runtime?.bindings[1].labels_age_ms, 24_000)
        XCTAssertEqual(model.bindCandidates(at: now).map(\.id), [AgentIdentity(source: "rex", session: "same-scope", pane_id: "same-pane")])
        XCTAssertEqual(model.healthSummary(at: now).level, .unavailable, "Fresh configuration is not hardware health")
        XCTAssertEqual(model.runtime?.posting.permission, .unknown)
        XCTAssertNil(model.runtime?.posting.last_checked_age_ms)
        XCTAssertNil(model.runtime?.devices.first?.layer)
        XCTAssertTrue(model.canMutate, "Configuration freshness does not assert hardware or permission health")
        await model.refresh(); await model.connect(); await settleTasks()
        await scheduler.advance(); await settleTasks()
        let requests = await transport.requests
        XCTAssertEqual(requests, [.configModel, .runtimeSnapshot, .configModel, .runtimeSnapshot,
                                  .configModel, .runtimeSnapshot, .runtimeSnapshot])
        model.disconnect()
    }

    @MainActor func testActualPostingDenialAndRecoveryAreActiveErrorVersusRetainedHistory() async throws {
        let now = Date(timeIntervalSince1970: 100)
        for name in ["runtime-v1-denied", "runtime-v1-recovered"] {
            let transport = try RustProducerTransport(name)
            let model = ConfigurationModel(endpoint: "/rust-producer-fixture-only.sock", transport: transport,
                                           clock: { now }, scheduler: ManualRefreshScheduler())
            await model.connect()
            let sample = try XCTUnwrap(model.runtime), error = try XCTUnwrap(sample.posting.last_error)
            let denied = name == "runtime-v1-denied"
            XCTAssertEqual(sample.posting.permission, denied ? .denied : .allowed)
            XCTAssertEqual(error.code, .posting_denied)
            XCTAssertEqual(error.active, denied)
            XCTAssertEqual(error.recovery, .grant_posting_permission)
            XCTAssertNil(error.device); XCTAssertNil(error.source)
            XCTAssertEqual(sample.events.first?.active, denied)
            XCTAssertEqual(model.healthSummary(at: now).level, denied ? .fault : .unavailable)
            if !denied {
                XCTAssertEqual(error.age_ms, 701)
                XCTAssertEqual(sample.posting.last_success_age_ms, 1)
                XCTAssertFalse(error.summary.contains("active"))
            }
            model.disconnect()
        }
    }

    @MainActor func testActualStaleSourceAgesAndCachedReplyCannotAcquireFreshMutationAuthority() async throws {
        var now = Date(timeIntervalSince1970: 100)
        let transport = try RustProducerTransport("runtime-v1-stale")
        let model = ConfigurationModel(endpoint: "/rust-producer-fixture-only.sock", transport: transport,
                                       clock: { now }, scheduler: ManualRefreshScheduler())
        await model.connect()
        let sample = try XCTUnwrap(model.runtime)
        // Producer captured at recovered_at+1ms and replied at recovered_at+16000ms:
        // sample age is 15999ms; Rex's separate 1501ms row/label age makes its evidence 17500ms old.
        XCTAssertEqual(sample.age_ms, 15_999)
        XCTAssertEqual(sample.bindings[0].labels_age_ms, 1_501)
        XCTAssertTrue(model.bindCandidates(at: now).isEmpty)
        XCTAssertEqual(model.bindingLegend(at: now)[0].availability, .unknown)
        XCTAssertTrue(model.bindingLegend(at: now)[0].labelsStale)
        XCTAssertFalse(model.canMutate); XCTAssertFalse(model.canRelease)
        now.addTimeInterval(0.402)
        XCTAssertFalse(model.canMutate); XCTAssertFalse(model.canRelease)
        XCTAssertEqual(model.healthSummary(at: now).level, .stale)
        await model.refresh()
        XCTAssertEqual(model.runtimeReceivedAt, Date(timeIntervalSince1970: 100))
        XCTAssertFalse(model.canMutate, "Receiving the identical cached sample cannot reset its evidence clock")
        model.disconnect()
    }

    @MainActor func testActualUnsupportedVersionEnvelopeLatchesUnavailableAndNeverFallsBackToStatus() async throws {
        let bytes = try RustProducerFixture.bytes("runtime-unsupported")
        XCTAssertThrowsError(try JSONDecoder().decode(Reply<RuntimeSnapshot>.self, from: bytes).value()) {
            XCTAssertEqual($0 as? ClientError, .daemon("unsupported runtime snapshot version"))
        }
        let transport = try RustProducerTransport("runtime-unsupported"), scheduler = ManualRefreshScheduler()
        let model = ConfigurationModel(endpoint: "/rust-producer-fixture-only.sock", transport: transport,
                                       clock: { Date(timeIntervalSince1970: 100) }, scheduler: scheduler)
        await model.connect(); await settleTasks()
        XCTAssertNil(model.runtime); XCTAssertNotNil(model.configuration)
        XCTAssertFalse(model.canMutate); XCTAssertFalse(model.canRelease)
        XCTAssertTrue(model.bindingLegend(at: Date(timeIntervalSince1970: 100)).isEmpty)
        XCTAssertEqual(model.healthSummary(at: Date(timeIntervalSince1970: 100)).level, .unavailable)
        await scheduler.advance(); await settleTasks()
        let requests = await transport.requests
        XCTAssertEqual(requests, [.configModel, .runtimeSnapshot, .configModel])
        model.disconnect()
    }

    @MainActor func testActualInvalidLedgerDisablesGuardedReleaseAndNeverWritesOnRead() async throws {
        let transport = try RustProducerTransport("runtime-v1-invalid-ledger")
        let model = ConfigurationModel(endpoint: "/rust-producer-fixture-only.sock", transport: transport,
                                       clock: { Date(timeIntervalSince1970: 100) }, scheduler: ManualRefreshScheduler())
        await model.connect()
        XCTAssertEqual(model.runtime?.ledger_available, false)
        XCTAssertEqual(model.runtime?.events.first?.code, .ledger_invalid)
        XCTAssertEqual(model.healthSummary(at: Date(timeIntervalSince1970: 100)).level, .fault)
        XCTAssertFalse(model.canRelease, "An invalid actual ledger cannot advertise guarded release eligibility")
        XCTAssertTrue(model.bindingLegend(at: Date(timeIntervalSince1970: 100)).allSatisfy { $0.availability == .unknown })
        let owner = AgentIdentity(source: "rex", session: "same-scope", pane_id: "same-pane")
        await model.bind(key: "key3", agent: owner)
        await model.release(key: "key1", reference: owner, endpoint: model.endpoint)
        let requests = await transport.requests
        XCTAssertEqual(requests, [.configModel, .runtimeSnapshot])
        model.disconnect()
    }
}
