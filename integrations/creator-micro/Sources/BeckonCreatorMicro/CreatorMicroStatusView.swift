import SwiftUI
import BeckonPanelModel

/// On-screen device geometry. It consumes the shared observer; it has no device transport.
@MainActor public struct CreatorMicroStatusView: View {
    @ObservedObject private var model: PanelModel
    public init(model: PanelModel) {self.model = model}

    public var body: some View {
        TimelineView(.periodic(from: .now, by: 0.5)) {timeline in
            let now = timeline.date.timeIntervalSince1970
            let frame = StatusAdapter.prepare(model: model, now: now)
            VStack(alignment: .leading, spacing: 16) {
                HStack {
                    VStack(alignment: .leading, spacing: 4) {
                        Text("Creator Micro").font(.title2.bold())
                        Text("On-screen preview · assignments stay in memory")
                            .font(.callout).foregroundStyle(.secondary)
                    }
                    Spacer()
                    Text(observation(now: now)).font(.callout).foregroundStyle(.secondary)
                }
                HStack(spacing: 12) {
                    key(slot: 0, frame: frame, now: now)
                    key(slot: 1, frame: frame, now: now)
                }
                HStack(spacing: 12) {
                    ForEach(2..<6, id: \.self) {key(slot: $0, frame: frame, now: now)}
                }
                HStack(spacing: 20) {
                    if !frame.liveWriterAvailable {Label("Hardware unavailable", systemImage: "keyboard")}
                    if !frame.focusAvailable {Label("Focus unavailable", systemImage: "cursorarrow")}
                    Label("Saving disabled", systemImage: "lock")
                }.font(.caption).foregroundStyle(.secondary)
                Text("These indicators do not light the physical keys. Green is Herdr-reported done/unread; amber is blocked or needs attention, without identifying a question or approval.")
                    .font(.caption).foregroundStyle(.secondary)
                DisclosureGroup("Prepared frame · developer details") {
                    Text(wire(frame)).font(.system(.caption, design: .monospaced))
                        .textSelection(.enabled).frame(maxWidth: .infinity, alignment: .leading)
                    Text("Proposed data only; no device write is sent.").font(.caption)
                }.font(.caption)
            }
            .padding(20)
            .background(.quaternary, in: RoundedRectangle(cornerRadius: 18))
        }
    }

    private func observation(now: Double) -> String {
        guard let snapshot = model.snapshot else {
            return model.health == "Not observed" ? "Waiting for first observation" : "Source unavailable / awaiting observation"
        }
        guard now.isFinite, snapshot.observedAt.isFinite, now >= snapshot.observedAt,
              now - snapshot.observedAt <= 10 else {return "Observation expired"}
        return model.busy ? "Refreshing…" : "Observed \(Int(now - snapshot.observedAt))s ago"
    }

    private func key(slot: Int, frame: StatusFrame, now: Double) -> some View {
        let led = frame.slots[slot]
        let native = model.configuration.slots[slot] ?? model.configuration.coordinators[slot]?.native
        let record = try? model.record(slot: slot, now: now)
        let status = native == nil ? "Unassigned · off" : led.presentation?.label ?? "Unavailable · off"
        let tint = color(led.presentation?.color)
        return VStack(alignment: .leading, spacing: 10) {
            HStack {
                Text(model.configuration.slotTitle(slot)).font(.headline)
                Spacer()
                Circle().fill(tint).frame(width: 12, height: 12)
                    .overlay(Circle().strokeBorder(Color.secondary.opacity(0.5), lineWidth: 1))
                    .opacity(led.presentation?.pulse == true ? 0.45 + 0.55 * abs(sin(now * 2.5)) : 1)
                    .accessibilityHidden(true)
            }
            Text(record?.label ?? (native == nil ? "Choose an agent below" : "No matching fresh agent"))
                .font(.callout.weight(.medium)).lineLimit(2)
            Text(status).font(.caption.weight(.semibold))
            if let native {
                Text("\(native.harness) · \(native.sessionID)")
                    .font(.system(.caption2, design: .monospaced)).lineLimit(2)
                    .textSelection(.enabled).help("\(native.harness):\(native.sessionID)")
                if let reason = led.unavailableReason {
                    Text(reason).font(.caption2).foregroundStyle(.secondary).lineLimit(3).help(reason)
                }
            }
            Text("Key \(led.thread.id) · slot \(slot + 1)").font(.caption2).foregroundStyle(.secondary)
        }
        .padding(14).frame(maxWidth: .infinity, minHeight: 160, alignment: .topLeading)
        .background(Color.primary.opacity(0.035), in: RoundedRectangle(cornerRadius: 12))
        .overlay(RoundedRectangle(cornerRadius: 12).strokeBorder(tint.opacity(0.4), lineWidth: 1))
        .accessibilityElement(children: .combine)
    }
    private func wire(_ frame: StatusFrame) -> String {
        guard let data = try? frame.encodedParams(), let value = String(data: data, encoding: .utf8) else {
            return "Frame encoding unavailable"
        }
        return value
    }
    private func color(_ hex: String?) -> Color {
        switch hex {
        case "#3BA0FF": return Color(red: 0.23, green: 0.63, blue: 1)
        case "#FFFFFF": return .white
        case "#22C55E": return Color(red: 0.1, green: 0.6, blue: 0.3)
        case "#FFB020": return Color(red: 0.65, green: 0.4, blue: 0)
        default: return .secondary
        }
    }
}
