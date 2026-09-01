// swift-tools-version: 5.10

import PackageDescription

let package = Package(
    name: "MeikiPopSwift",
    platforms: [
        .macOS(.v14),
    ],
    targets: [
        .executableTarget(name: "MeikiPopSwift"),
    ]
)
