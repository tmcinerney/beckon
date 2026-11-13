import SwiftUI
import BeckonConfiguration
import BeckonConfigurationUI

@main struct BeckonApp: App {
    @StateObject private var model = ConfigurationModel()
    var body: some Scene {
        WindowGroup("Beckon") {
            TabView {
                ConfigurationView(model: model)
                    .tabItem { Label("Configuration", systemImage: "slider.horizontal.3") }
            }.frame(minWidth: 720, minHeight: 540)
        }.defaultSize(width: 960, height: 800)
    }
}
