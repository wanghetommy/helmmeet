#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
mod acoustic_probe;
mod activity;
mod analysis;
mod asr;
mod assistant;
mod assistant_context;
mod assistant_history;
mod audio;
mod calibration;
mod commands;
mod config;
mod credentials;
mod db;
#[cfg(feature = "audio-processing")]
mod echo;
mod enhancement;
mod exit_probe;
mod knowledge;
mod launch;
mod lifecycle;
#[path = "../../../../agent/prompts.rs"]
mod meeting_prompts;
mod memory;
mod models;
mod paragraphs;
#[cfg(feature = "audio-processing")]
mod processing;
mod product;
mod review;
mod routes;
mod voice;
mod workspace;

use serde::Serialize;
use serde_json::Value;
use std::sync::Arc;
static QUIT_APP: std::sync::OnceLock<tauri::AppHandle> = std::sync::OnceLock::new();
extern "C" {
    fn hm_application_install_quit_gate(callback: extern "C" fn()) -> i32;
}
extern "C" fn native_quit() {
    if let Some(app) = QUIT_APP.get() {
        app.exit(0);
    }
}
use tauri::{
    menu::{Menu, MenuItem},
    tray::TrayIconBuilder,
    Emitter, Manager,
};

#[derive(Serialize)]
struct SystemStatus {
    protocol_version: u32,
    runtime: &'static str,
    capture_available: bool,
    asr_available: bool,
    analysis_available: bool,
}
#[tauri::command]
fn get_system_status() -> SystemStatus {
    SystemStatus {
        protocol_version: 1,
        runtime: "tauri",
        capture_available: true,
        asr_available: true,
        analysis_available: true,
    }
}
#[tauri::command]
async fn dispatch(
    state: tauri::State<'_, Arc<db::Runtime>>,
    operation: commands::Request,
) -> Result<Value, String> {
    let runtime = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || commands::run(runtime, operation))
        .await
        .map_err(|_| "后台任务中断".to_string())?
}
fn main() {
    if std::env::args().any(|s| s == "--check-model-json") {
        let root = std::env::var_os("HELMMEET_SMOKE_ROOT")
            .map(std::path::PathBuf::from)
            .expect("check-model-json requires an isolated HELMMEET_SMOKE_ROOT");
        assert!(
            !root.join("app.sqlite").exists(),
            "model check requires a fresh isolated directory"
        );
        let result = (|| -> Result<Value, String> {
            let rt = db::Runtime::open_with_vault(
                root.clone(),
                Arc::new(credentials::FileStore::new(root)),
            )?;
            let dir = models::list(&rt)?;
            let pid = dir["default_profile_id"]
                .as_str()
                .ok_or("没有测试模型配置")?;
            let profile = dir["profiles"]
                .as_array()
                .and_then(|v| v.iter().find(|p| p["profile_id"] == pid))
                .ok_or("测试模型配置缺失")?;
            models::test(
                &rt,
                pid,
                profile["config_version"].as_u64().ok_or("配置版本缺失")? as u32,
            )
        })();
        match result {
            Ok(v) => println!("{v}"),
            Err(e) => {
                eprintln!("Structured model check failed: {e}");
                std::process::exit(1);
            }
        }
        return;
    }
    if std::env::args().any(|s| s == "--check-acoustic-aec") {
        match acoustic_probe::run() {
            Ok(result) => println!("{result}"),
            Err(error) => {
                eprintln!("Acoustic check failed: {error}");
                std::process::exit(1);
            }
        }
        return;
    }
    if std::env::args().any(|s| s == "--check-output-route") {
        println!("{}", audio::output_route_status());
        return;
    }
    if std::env::args().any(|s| s == "--check-audio-processing") {
        #[cfg(feature = "audio-processing")]
        match processing::synthetic_probe() {
            Ok(result) => println!("{result}"),
            Err(error) => {
                eprintln!("Audio processing probe failed: {error}");
                std::process::exit(1);
            }
        }
        #[cfg(not(feature = "audio-processing"))]
        {
            eprintln!("Build with --features audio-processing to enable the synthetic probe");
            std::process::exit(2);
        }
        #[cfg(feature = "audio-processing")]
        return;
    }
    if let Some(index) = std::env::args().position(|s| s == "--probe-whisper-metal") {
        let model = std::env::args()
            .nth(index + 1)
            .expect("--probe-whisper-metal requires a model path");
        let _context = whisper_rs::WhisperContext::new_with_params(
            &model,
            asr::context_parameters_for_probe(),
        )
        .expect("Metal model probe failed");
        return;
    }
    // No log backend enabled: suppress upstream logs which may contain token text.
    whisper_rs::install_logging_hooks();
    if std::env::args().any(|s| s == "--check-microphone") {
        match audio::check_microphone() {
            Ok(result) => println!("麦克风 3 秒电平检查通过（未保存、未转写、未联网）：{result}"),
            Err(error) => {
                eprintln!("麦克风检查失败：{error}");
                std::process::exit(1);
            }
        }
        return;
    }
    if std::env::args().any(|s| s == "--smoke-test") {
        let root = std::env::var_os("HELMMEET_SMOKE_ROOT")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::env::temp_dir().join(format!("helmmeet-test-{}", db::id())));
        let rt = db::Runtime::open(root).expect("smoke database");
        let meeting = rt.create("自动化合成音频检查", "general").expect("create");
        assert_eq!(
            rt.detail(meeting["id"].as_str().unwrap()).unwrap()["segments"],
            serde_json::json!([])
        );
        if let Some(path) = std::env::var_os("HELMMEET_TEST_WAV") {
            let cfg = rt.config.lock().unwrap().clone();
            let params = asr::context_parameters();
            let ctx = whisper_rs::WhisperContext::new_with_params(&cfg.asr.model_path, params)
                .expect("model load");
            let pcm = audio::read_wav(&std::path::PathBuf::from(path)).expect("synthetic wav");
            let began = std::time::Instant::now();
            let result = asr::decode(&ctx, &pcm, "en").expect("decode");
            assert!(!result.is_empty(), "synthetic speech must produce text");
            let mid = meeting["id"].as_str().unwrap();
            let duration = pcm.len() as u64 / 16;
            let dir = rt.meeting_dir(mid).join("audio");
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join("synthetic.wav");
            let mut wav = audio::WavFile::create(&path).unwrap();
            wav.write(&pcm).unwrap();
            wav.sync().unwrap();
            drop(wav);
            {
                let db = rt.db().unwrap();
                db.execute(
                    "UPDATE meetings SET state='completed',recording_duration_ms=? WHERE id=?",
                    rusqlite::params![duration, mid],
                )
                .unwrap();
                db.execute(
                    "INSERT INTO audio_chunks VALUES (?,?,'microphone',?,0,?,?,'sealed')",
                    rusqlite::params![
                        db::id(),
                        mid,
                        format!("meetings/{mid}/audio/synthetic.wav"),
                        duration,
                        pcm.len()
                    ],
                )
                .unwrap();
                db.execute(
                    "INSERT INTO asr_chunks VALUES (?,0,?)",
                    rusqlite::params![mid, duration],
                )
                .unwrap();
                for (a, b, text) in &result {
                    let seg = db::id();
                    db.execute(
                        "INSERT INTO transcript_segments VALUES (?,?,?,?,'mixed',1,'final')",
                        rusqlite::params![seg, mid, a, b],
                    )
                    .unwrap();
                    db.execute(
                        "INSERT INTO transcript_revisions VALUES (?,1,?,?,'asr',?)",
                        rusqlite::params![seg, mid, text, db::now()],
                    )
                    .unwrap();
                }
            }
            println!(
                "ASR smoke: {} real segments in {}ms for {}ms audio, whisper.cpp {}",
                result.len(),
                began.elapsed().as_millis(),
                duration,
                whisper_rs::WHISPER_CPP_VERSION
            );
            if std::env::var("HELMMEET_TEST_LLM").as_deref() == Ok("1") {
                commands::run(rt.clone(), commands::Request::TestModel).expect("model connection");
                commands::run(
                    rt.clone(),
                    commands::Request::Consent {
                        meeting_id: mid.into(),
                        expected_version: rt.detail(mid).unwrap()["version"].as_u64().unwrap(),
                        enabled: true,
                        interval_minutes: 0,
                    },
                )
                .unwrap();
                let submitted = analysis::submit(rt.clone(), mid, "minutes", "", "all")
                    .expect("analysis submit");
                let job = submitted["job_id"].as_str().unwrap();
                let until = std::time::Instant::now() + std::time::Duration::from_secs(180);
                loop {
                    let (state, error): (String, Option<String>) = rt
                        .db()
                        .unwrap()
                        .query_row(
                            "SELECT state,error_code FROM analysis_jobs WHERE id=?",
                            [job],
                            |r| Ok((r.get(0)?, r.get(1)?)),
                        )
                        .unwrap();
                    if state == "succeeded" {
                        break;
                    }
                    assert_ne!(
                        state,
                        "failed",
                        "analysis failed: {}",
                        error.unwrap_or_default()
                    );
                    assert!(std::time::Instant::now() < until, "analysis timed out");
                    std::thread::sleep(std::time::Duration::from_millis(500));
                }
                let detail = rt.detail(mid).unwrap();
                assert!(!detail["artifacts"].as_array().unwrap().is_empty());
                assert!(!detail["actions"].as_array().unwrap().is_empty());
                let exported = commands::export_text(&detail, "md");
                assert!(exported.contains("完整转写"));
                let playback = commands::run(
                    rt.clone(),
                    commands::Request::Playback {
                        meeting_id: mid.into(),
                        start_ms: 0,
                        audio_variant: "raw".into(),
                        end_ms: None,
                    },
                )
                .unwrap();
                assert!(!playback["samples"].as_array().unwrap().is_empty());
                println!("LLM smoke: real summary, {} action candidates, validated evidence, SQLite persistence, export and playback passed; synthetic content only",detail["actions"].as_array().unwrap().len());
            }
        }
        println!("Runtime smoke passed");
        return;
    }
    let exit_probe = std::env::args().any(|s| s == "--check-asr-exit");
    let app = tauri::Builder::default()
        .manage(launch::LaunchInbox::default())
        .setup(move |app| {
            let root = std::env::var_os("HELMMEET_DATA_ROOT")
                .map(std::path::PathBuf::from)
                .unwrap_or(app.path().app_data_dir()?);
            if exit_probe
                && (std::env::var_os("HELMMEET_DATA_ROOT").is_none()
                    || root.join("app.sqlite").exists())
            {
                return Err("退出检查必须使用显式指定且不含数据库的独立目录".into());
            }
            let rt = db::Runtime::open(root).map_err(std::io::Error::other)?;
            let probe = if exit_probe {
                let model = std::env::var_os("HELMMEET_TEST_MODEL")
                    .ok_or("退出检查需要 HELMMEET_TEST_MODEL")?;
                Some(
                    exit_probe::prepare(&rt, &std::path::PathBuf::from(model))
                        .map_err(std::io::Error::other)?,
                )
            } else {
                None
            };
            asr::recover(&rt).map_err(std::io::Error::other)?;
            commands::cleanup(&rt).map_err(std::io::Error::other)?;
            enhancement::recover(&rt).map_err(std::io::Error::other)?;
            app.manage(lifecycle::Shutdown::new(vec![
                asr::spawn(rt.clone()),
                voice::background(rt.clone()),
                activity::background(rt.clone()),
                workspace::background(rt.clone()),
                calibration::background(rt.clone()),
                enhancement::background(rt.clone()),
            ]));
            app.manage(rt.clone());
            let _ = QUIT_APP.set(app.handle().clone());
            if unsafe { hm_application_install_quit_gate(native_quit) } != 0 {
                return Err("无法安装原生退出保护，请保留日志排查".into());
            }
            if let Some(mid) = probe {
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.hide();
                }
                let phase = std::env::var("HELMMEET_EXIT_PHASE").unwrap_or_else(|_| "idle".into());
                exit_probe::request_native_quit(app.handle().clone(), rt, mid, phase);
            }
            let open = MenuItem::with_id(app, "open", "打开 HelmMeet", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
            let end = MenuItem::with_id(app, "end-meeting", "结束当前会议…", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&open, &end, &quit])?;
            TrayIconBuilder::new()
                .icon(tauri::include_image!("icons/tray-template.png"))
                .icon_as_template(true)
                .tooltip("HelmMeet · 会议助手")
                .menu(&menu)
                .on_menu_event(|app, event| {
                    if event.id.as_ref() == "open" {
                        if let Some(w) = app.get_webview_window("main") {
                            let _ = w.show();
                            let _ = w.set_focus();
                        }
                    }
                    if event.id.as_ref() == "end-meeting" {
                        if let Some(w) = app.get_webview_window("main") {
                            let _ = w.show();
                            let _ = w.set_focus();
                            let _ = w.emit("request-end-meeting", ());
                        }
                    }
                    if event.id.as_ref() == "quit" {
                        app.exit(0);
                    }
                })
                .build(app)?;
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_system_status,
            dispatch,
            launch::launch_request
        ])
        .build(tauri::generate_context!())
        .expect("启动 HelmMeet 失败");
    app.run(move |app, event| {
        #[cfg(target_os = "macos")]
        if let tauri::RunEvent::Opened { ref urls } = event {
            for url in urls {
                if url.scheme() != "helmmeet" {
                    continue;
                }
                match app.state::<launch::LaunchInbox>().receive(url) {
                    Ok(_) => {
                        let _ = app.emit("launch-link", ());
                    }
                    Err(message) => {
                        let _ = app.emit("launch-link-error", message);
                    }
                }
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.show();
                    let _ = w.unminimize();
                    let _ = w.set_focus();
                }
            }
        }
        if let tauri::RunEvent::ExitRequested { api, code, .. } = event {
            let shutdown = app.state::<lifecycle::Shutdown>();
            if shutdown.ready() {
                return;
            }
            api.prevent_exit();
            let rt = app.state::<Arc<db::Runtime>>();
            let capture = rt.capture.lock().unwrap_or_else(|e| e.into_inner());
            let active = capture.is_some();
            if active {
                let meeting = capture.as_ref().unwrap().meeting.clone();
                drop(capture);
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.show();
                    let _ = w.set_focus();
                }
                rt.error(&meeting, "仍在录制，请先在主窗口结束会议，再退出应用");
            } else if let Some(workers) = shutdown.begin() {
                // Serialize with audio::start/preview so a queued start cannot
                // acquire capture after the exit decision and begin recording.
                rt.shutting_down
                    .store(true, std::sync::atomic::Ordering::Release);
                drop(capture);
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.set_title("HelmMeet · 正在停止转写并释放模型…");
                }
                let handle = app.clone();
                std::thread::spawn(move || {
                    let result = lifecycle::stop_workers(workers);
                    handle.state::<lifecycle::Shutdown>().finish(result.is_ok());
                    match result {
                        Ok(()) => {
                            if exit_probe {
                                println!("ASR exit probe: workers joined before final exit");
                            }
                            handle.exit(code.unwrap_or(0));
                        }
                        Err(error) => {
                            eprintln!("退出清理未完成：{error}");
                            if let Some(w) = handle.get_webview_window("main") {
                                let _ = w.set_title("HelmMeet · 退出清理失败，请保留日志排查");
                            }
                        }
                    }
                });
            }
        }
    });
}
#[cfg(test)]
mod tests {
    #[test]
    fn ipc_status_matches_shared_fixture() {
        let v: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../tests/fixtures/system-status.json"
        ))
        .unwrap();
        assert_eq!(serde_json::to_value(super::get_system_status()).unwrap(), v);
    }
}
