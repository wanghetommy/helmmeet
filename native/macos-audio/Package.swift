// swift-tools-version: 5.9
import PackageDescription

let package = Package(
    name: "HelmMeetAudio",
    platforms: [.macOS(.v14)],
    products: [.library(name: "HelmMeetAudio", type: .static, targets: ["HelmMeetAudio"])],
    targets: [.target(name: "HelmMeetAudio")]
)
