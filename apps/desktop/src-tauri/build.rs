fn main() {
    let target = std::env::var("TARGET").expect("Cargo target");
    let arch = match target.as_str() {
        "aarch64-apple-darwin" => "arm64",
        "x86_64-apple-darwin" => "x86_64",
        _ => panic!("Desktop supports macOS arm64 / x86_64 only"),
    };
    if std::env::var_os("CARGO_FEATURE_AUDIO_PROCESSING").is_some() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
        let library = root
            .join(".tools/audio-processing/targets")
            .join(&target)
            .join("lib");
        let result = std::process::Command::new(root.join(".tools/python/bin/python3"))
            .arg(root.join("native/audio-processing/build.py"))
            .args(["--target", &target])
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
            "build-support/x86_64.ini",
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
    if std::env::var("HOST").expect("Cargo host") == target {
        swift_rs::SwiftLinker::new("14.0")
            .with_package("HelmMeetAudio", "../../../native/macos-audio")
            .link();
    } else {
        // swift-rs 1.0.9 uses the host --arch for macOS packages. Build the
        // package with an explicit target triple instead; still use its runtime links.
        swift_rs::SwiftLinker::new("14.0").link();
        cross_build_audio(arch);
    }
    for framework in ["AVFoundation", "ScreenCaptureKit", "CoreMedia", "CoreAudio"] {
        println!("cargo:rustc-link-lib=framework={framework}");
    }
    tauri_build::try_build(tauri_build::Attributes::new().app_manifest(
        tauri_build::AppManifest::new().commands(&["get_system_status", "dispatch"]),
    ))
    .expect("desktop build setup failed");
}

fn cross_build_audio(arch: &str) {
    use std::{path::PathBuf, process::Command};
    let sdk = Command::new("xcrun")
        .args(["--sdk", "macosx", "--show-sdk-path"])
        .output()
        .expect("macOS SDK");
    assert!(sdk.status.success(), "macOS SDK unavailable");
    let sdk = String::from_utf8(sdk.stdout).expect("SDK path");
    let build =
        PathBuf::from(std::env::var_os("OUT_DIR").expect("Cargo output")).join("swift-audio");
    let configuration = if std::env::var("DEBUG").as_deref() == Ok("true") {
        "debug"
    } else {
        "release"
    };
    let triple = format!("{arch}-apple-macosx14.0");
    let command = || {
        let mut c = Command::new("swift");
        c.args([
            "build",
            "--package-path",
            "../../../native/macos-audio",
            "--build-path",
        ])
        .arg(&build)
        .args([
            "--sdk",
            sdk.trim(),
            "--triple",
            &triple,
            "--configuration",
            configuration,
        ]);
        c
    };
    let result = command()
        .args(["--product", "HelmMeetAudio"])
        .output()
        .expect("Swift audio build");
    assert!(
        result.status.success(),
        "Swift target build failed: {}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    let location = command()
        .arg("--show-bin-path")
        .output()
        .expect("Swift output path");
    assert!(location.status.success(), "Swift output path unavailable");
    let location = String::from_utf8(location.stdout).expect("Swift output path encoding");
    assert!(
        PathBuf::from(location.trim())
            .join("libHelmMeetAudio.a")
            .is_file(),
        "Swift archive missing"
    );
    println!("cargo:rustc-link-search=native={}", location.trim());
    println!("cargo:rustc-link-lib=static=HelmMeetAudio");
}
