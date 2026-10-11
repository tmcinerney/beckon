// swift-tools-version: 5.9
import Foundation
import PackageDescription

let wlKitDependency: Package.Dependency
if let path = ProcessInfo.processInfo.environment["BECKON_WLKIT_PATH"] {
    wlKitDependency = .package(name: "WLTools", path: path)
} else {
    wlKitDependency = .package(name: "WLTools", url: "https://github.com/schacon/micro-manager.git",
                              revision: "32caf5a429cd0618dbb8c8354e34223d601ee235")
}
let package = Package(
    name: "BeckonCreatorMicro",
    platforms: [.macOS(.v13)],
    products: [
        .library(name: "BeckonCreatorMicro", targets: ["BeckonCreatorMicro"]),
        .executable(name: "beckon-creator-micro-preview", targets: ["CreatorMicroPreview"]),
    ],
    dependencies: [.package(path: "../../swift"), wlKitDependency],
    targets: [
        .target(name: "BeckonCreatorMicro", dependencies: [
            .product(name: "BeckonPanelModel", package: "swift"),
            .product(name: "WLKit", package: "WLTools"),
        ], resources: [.process("Fixtures")]),
        .executableTarget(name: "CreatorMicroPreview", dependencies: [
            "BeckonCreatorMicro",
            .product(name: "BeckonPanelUI", package: "swift"),
        ]),
        .testTarget(name: "BeckonCreatorMicroTests", dependencies: ["BeckonCreatorMicro"]),
    ]
)
