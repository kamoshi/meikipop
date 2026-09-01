// swift-tools-version: 5.10

import PackageDescription

let package = Package(
    name: "MeikiPopSwift",
    platforms: [
        .macOS(.v14),
    ],
    targets: [
        .systemLibrary(
            name: "CMeikiPopFFI",
            path: "CMeikiPopFFI"
        ),
        .executableTarget(
            name: "MeikiPopSwift",
            dependencies: ["CMeikiPopFFI"],
            linkerSettings: [
                .unsafeFlags(["-L", "../../crates/native-ffi/target/debug"]),
                .linkedLibrary("meikipop_native_ffi"),
                .linkedLibrary("c++"),
                .linkedFramework("SystemConfiguration"),
            ]
        ),
    ]
)
