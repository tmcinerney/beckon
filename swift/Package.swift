// swift-tools-version: 5.9
import PackageDescription

let package = Package(
    name: "BeckonConfiguration",
    platforms: [.macOS(.v13)],
    products: [
        .library(name: "BeckonConfiguration", targets: ["BeckonConfiguration"]),
        .library(name: "BeckonConfigurationUI", targets: ["BeckonConfigurationUI"]),
        .executable(name: "beckon-configuration", targets: ["BeckonApp"]),
    ],
    targets: [
        .target(name: "SocketSupport"),
        .target(name: "BeckonConfiguration", dependencies: ["SocketSupport"]),
        .target(name: "BeckonConfigurationUI", dependencies: ["BeckonConfiguration"]),
        .executableTarget(name: "BeckonApp", dependencies: ["BeckonConfigurationUI"]),
        .testTarget(name: "BeckonConfigurationTests", dependencies: ["BeckonConfiguration"]),
    ]
)
