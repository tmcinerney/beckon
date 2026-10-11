// swift-tools-version: 5.9
import PackageDescription

let package = Package(
    name: "BeckonControl",
    platforms: [.macOS(.v13)],
    products: [
        .library(name: "BeckonControl", targets: ["BeckonControl"]),
        .library(name: "BeckonPanelModel", targets: ["BeckonPanelModel"]),
        .library(name: "BeckonPanelUI", targets: ["BeckonPanelUI"]),
        .executable(name: "beckon-control-preview", targets: ["BeckonPreview"]),
        .executable(name: "beckon-control-panel", targets: ["BeckonPanel"]),
    ],
    targets: [
        .target(name: "BeckonControl"),
        .target(name: "PanelSupport"),
        .target(name: "BeckonPanelModel", dependencies: ["PanelSupport"]),
        .executableTarget(name: "BeckonPreview", dependencies: ["BeckonControl"]),
        .target(name: "BeckonPanelUI", dependencies: ["BeckonPanelModel"]),
        .executableTarget(name: "BeckonPanel", dependencies: ["BeckonPanelUI"]),
        .testTarget(name: "BeckonControlTests", dependencies: ["BeckonControl"]),
        .testTarget(name: "BeckonPanelModelTests", dependencies: ["BeckonPanelModel"]),
    ]
)
