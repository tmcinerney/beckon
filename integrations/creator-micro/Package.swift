// swift-tools-version: 5.9
import Foundation
import PackageDescription

let wlKitDependency: Package.Dependency
if let path = ProcessInfo.processInfo.environment["BECKON_WLKIT_PATH"] {
    wlKitDependency = .package(name: "WLTools", path: path)
} else {
    wlKitDependency = .package(name: "WLTools", url: "https://github.com/tmcinerney/micro-manager.git",
                              revision: "8ea3f5a7fa8901fcf1fd6175092d8e41c41cf45e")
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
