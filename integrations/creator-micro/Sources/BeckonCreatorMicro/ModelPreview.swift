import Foundation
import BeckonPanelModel

extension StatusAdapter {
    /// Uses the same model as the shared panel, without a second reader or writer.
    @MainActor public static func prepare(model: PanelModel, now: Double) -> StatusFrame {
        prepare(configuration: model.configuration, snapshot: model.snapshot,
                directory: model.directorySnapshot, now: now)
    }
}
