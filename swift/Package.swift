// swift-tools-version: 5.9
import PackageDescription

let package = Package(
    name: "BeckonControl",
    platforms: [.macOS(.v13)],
    products: [
        .library(name: "BeckonControl", targets: ["BeckonControl"]),
        .executable(name: "beckon-control-preview", targets: ["BeckonPreview"]),
    ],
    targets: [
        .target(name: "BeckonControl"),
        .executableTarget(name: "BeckonPreview", dependencies: ["BeckonControl"]),
        .testTarget(name: "BeckonControlTests", dependencies: ["BeckonControl"]),
    ]
)
