// swift-tools-version: 5.9
import Foundation
import PackageDescription

let wlKitDependency: Package.Dependency
if let path = ProcessInfo.processInfo.environment["BECKON_WLKIT_PATH"] {
    wlKitDependency = .package(name: "WLTools", path: path)
} else {
    wlKitDependency = .package(name: "WLTools", url: "https://github.com/tmcinerney/micro-manager.git",
                              revision: "a71ebd3ee6ede5f5a39945148484a8b957d9a50a")
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
