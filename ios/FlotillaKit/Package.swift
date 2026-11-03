// swift-tools-version: 6.2
// Flotilla's protocol client: the wire, the replica of the views, pairing, and the
// connection. No UI, so it builds and tests on the Mac (`swift test`) as well as on iOS.

import PackageDescription

let package = Package(
    name: "FlotillaKit",
    platforms: [.iOS(.v26), .macOS(.v26)],
    products: [
        .library(name: "FlotillaKit", targets: ["FlotillaKit"]),
    ],
    targets: [
        .target(
            name: "FlotillaKit",
            swiftSettings: [.enableUpcomingFeature("ExistentialAny")]
        ),
        .testTarget(
            name: "FlotillaKitTests",
            dependencies: ["FlotillaKit"]
        ),
    ],
    swiftLanguageModes: [.v6]
)
