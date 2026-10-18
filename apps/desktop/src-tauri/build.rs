fn main() {
    println!("cargo:rerun-if-changed=../../../native/macos-audio/Sources");
    swift_rs::SwiftLinker::new("14.0")
        .with_package("HelmMeetAudio", "../../../native/macos-audio")
        .link();
    for framework in ["AVFoundation", "ScreenCaptureKit", "CoreMedia", "CoreAudio"] {
        println!("cargo:rustc-link-lib=framework={framework}");
    }
    tauri_build::try_build(tauri_build::Attributes::new().app_manifest(
        tauri_build::AppManifest::new().commands(&["get_system_status", "dispatch"]),
    ))
    .expect("desktop build setup failed");
}
