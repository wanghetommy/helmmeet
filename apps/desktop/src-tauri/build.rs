fn main() {
    if std::env::var_os("CARGO_FEATURE_AUDIO_PROCESSING").is_some() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
        let library = root.join(".tools/audio-processing/lib");
        let result = std::process::Command::new(root.join(".tools/python/bin/python3"))
            .arg(root.join("native/audio-processing/build.py"))
            .current_dir(&root)
            .output()
            .expect("APM requires the project-local Python toolchain");
        assert!(
            result.status.success(),
            "Offline APM build failed: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        for file in [
            "build.py",
            "dependencies.lock.json",
            "build-support/arm64.ini",
            "src",
            "include",
            "tests/probe.cpp",
        ] {
            println!(
                "cargo:rerun-if-changed={}",
                root.join("native/audio-processing").join(file).display()
            );
        }
        println!("cargo:rustc-link-search=native={}", library.display());
        println!("cargo:rustc-link-lib=static=helmmeet_apm");
        println!("cargo:rustc-link-lib=c++");
        println!("cargo:rustc-link-lib=framework=CoreFoundation");
    }
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
