#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
mod analysis;
mod asr;
mod audio;
mod commands;
mod config;
mod db;
mod models;

use serde::Serialize;
use serde_json::Value;
use std::sync::Arc;
use tauri::{
    menu::{Menu, MenuItem},
    tray::TrayIconBuilder,
    Manager,
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
fn background(rt: Arc<db::Runtime>) {
    std::thread::spawn(move || loop {
        std::thread::sleep(std::time::Duration::from_secs(10));
        let _ = commands::cleanup(&rt);
        let meetings = match rt.list() {
            Ok(v) => v,
            Err(_) => continue,
        };
        for meeting in meetings.as_array().into_iter().flatten() {
            let Some(mid) = meeting["id"].as_str() else {
                continue;
            };
            let detail = match rt.detail(mid) {
                Ok(d) => d,
                Err(_) => continue,
            };
            if detail["analysis_mode"] == "transcript_only" {
                continue;
            }
            let running = detail["jobs"].as_array().is_some_and(|j| {
                j.iter()
                    .any(|v| v["state"] == "running" || v["state"] == "pending")
            });
            if running {
                continue;
            }
            let duration = detail["duration_ms"].as_u64().unwrap_or(0);
            let interval = detail["auto_summary_minutes"].as_u64().unwrap_or(5) * 60000;
            let completed = detail["state"] == "completed";
            if completed {
                let attempted = detail["jobs"]
                    .as_array()
                    .is_some_and(|j| j.iter().any(|v| v["type"] == "final_map"));
                if !attempted {
                    let _ = analysis::submit(rt.clone(), mid, "minutes", "", "all");
                }
            } else if interval > 0 && duration >= interval {
                let last = rt
                    .transient
                    .lock()
                    .ok()
                    .and_then(|t| t[mid]["auto_at"].as_u64())
                    .unwrap_or(0);
                let chars: usize = detail["segments"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|s| s["text"].as_str().unwrap_or("").chars().count())
                    .sum();
                if duration >= last + interval && chars >= 200 {
                    if let Ok(mut t) = rt.transient.lock() {
                        t[mid]["auto_at"] = serde_json::json!(duration);
                    }
                    let _ = analysis::submit(rt.clone(), mid, "stage", "", "all");
                }
            }
        }
    });
}
fn main() {
    // No log backend enabled: suppress upstream logs which may contain token text.
    whisper_rs::install_logging_hooks();
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
            let mut params = whisper_rs::WhisperContextParameters::default();
            params.use_gpu(true);
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
    let app = tauri::Builder::default()
        .setup(|app| {
            let root = std::env::var_os("HELMMEET_DATA_ROOT")
                .map(std::path::PathBuf::from)
                .unwrap_or(app.path().app_data_dir()?);
            let rt = db::Runtime::open(root).map_err(std::io::Error::other)?;
            asr::recover(&rt).map_err(std::io::Error::other)?;
            commands::cleanup(&rt).map_err(std::io::Error::other)?;
            asr::spawn(rt.clone());
            background(rt.clone());
            app.manage(rt);
            let open = MenuItem::with_id(app, "open", "打开 HelmMeet", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&open, &quit])?;
            TrayIconBuilder::new()
                .icon(app.default_window_icon().cloned().unwrap_or_else(|| {
                    tauri::image::Image::new_owned(vec![50; 16 * 16 * 4], 16, 16)
                }))
                .tooltip("HelmMeet · 会议助手")
                .menu(&menu)
                .on_menu_event(|app, event| {
                    if event.id.as_ref() == "open" {
                        if let Some(w) = app.get_webview_window("main") {
                            let _ = w.show();
                            let _ = w.set_focus();
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
        .invoke_handler(tauri::generate_handler![get_system_status, dispatch])
        .build(tauri::generate_context!())
        .expect("启动 HelmMeet 失败");
    app.run(|app, event| {
        if let tauri::RunEvent::ExitRequested { api, .. } = event {
            let rt = app.state::<Arc<db::Runtime>>();
            let active = rt.capture.lock().map(|c| c.is_some()).unwrap_or(true);
            if active {
                api.prevent_exit();
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.show();
                    let _ = w.set_focus();
                }
                rt.error(
                    &rt.capture
                        .lock()
                        .ok()
                        .and_then(|g| g.as_ref().map(|s| s.meeting.clone()))
                        .unwrap_or_default(),
                    "仍在录制，请先在主窗口结束会议，再退出应用",
                );
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
