//! Opt-in local regression using synthetic PCM, never microphone or model HTTP.
use crate::{
    audio::WavFile,
    db::{err, id, Runtime},
};
use rusqlite::params;
use std::{
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

extern "C" {
    fn hm_application_request_quit();
}

pub fn prepare(rt: &Runtime, model: &Path) -> Result<String, String> {
    if !model.is_file() {
        return Err("退出检查需要显式指定可读的本地模型".into());
    }
    {
        let mut config = rt.config.lock().map_err(|_| "配置锁异常")?;
        config.asr.model_path = model.to_string_lossy().into_owned();
        config.asr.model_sha256 = None;
        config.asr.language = "en".into();
    }
    let meeting = rt.create("合成 PCM 退出回归（无真实录音）", "general")?;
    let mid = meeting["id"].as_str().ok_or("缺少测试会议 ID")?.to_string();
    let dir = rt.meeting_dir(&mid).join("audio");
    std::fs::create_dir_all(&dir).map_err(err)?;
    let pcm: Vec<f32> = (0..128000)
        .map(|i| (i as f32 * 440.0 * std::f32::consts::TAU / 16000.0).sin() * 0.08)
        .collect();
    let mut wav = WavFile::create(&dir.join("synthetic.wav"))?;
    wav.write(&pcm)?;
    wav.sync()?;
    drop(wav);
    let db = rt.db()?;
    db.execute(
        "INSERT INTO audio_chunks VALUES (?,?,'microphone',?,0,8000,128000,'sealed')",
        params![id(), mid, format!("meetings/{mid}/audio/synthetic.wav")],
    )
    .map_err(err)?;
    db.execute(
        "UPDATE meetings SET state='finalizing',recording_duration_ms=8000 WHERE id=?",
        [&mid],
    )
    .map_err(err)?;
    Ok(mid)
}

pub fn reached(rt: &Runtime, meeting: &str, phase: &str) -> bool {
    let Ok(t) = rt.transient.lock() else {
        return false;
    };
    match phase {
        "loading" => t[meeting]["asr_state"] == "loading",
        "busy" => t[meeting]["asr_state"] == "transcribing",
        _ => t[meeting]["asr_through_ms"].as_u64().unwrap_or(0) >= 8000,
    }
}

pub fn request_native_quit(
    app: tauri::AppHandle,
    rt: Arc<Runtime>,
    meeting: String,
    phase: String,
) {
    std::thread::spawn(move || {
        let began = Instant::now();
        loop {
            if reached(&rt, &meeting, &phase) {
                println!("ASR exit probe: {phase}; requesting AppKit Quit (synthetic PCM only)");
                let _ = app.run_on_main_thread(|| unsafe {
                    hm_application_request_quit();
                    hm_application_request_quit(); // repeated Quit must not bypass the drain
                });
                break;
            }
            if began.elapsed() > Duration::from_secs(90) {
                eprintln!("ASR exit probe timed out before target phase");
                app.exit(1);
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{asr, lifecycle::stop_workers, models::tests::MemoryVault};

    #[test]
    fn shutdown_blocks_queued_capture_preview_commands_and_analysis() {
        let root = std::env::temp_dir().join(format!("helmmeet-exit-fence-{}", id()));
        let rt = Runtime::open_with_vault(root.clone(), Arc::new(MemoryVault::default())).unwrap();
        let mid = rt.create("合成退出边界", "general").unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        rt.shutting_down
            .store(true, std::sync::atomic::Ordering::Release);
        assert!(crate::audio::preview(&rt, 1, 0)
            .unwrap_err()
            .contains("正在退出"));
        assert!(crate::audio::start(rt.clone(), &mid, 1, 0, false)
            .unwrap_err()
            .contains("正在退出"));
        assert!(
            crate::commands::run(rt.clone(), crate::commands::Request::List)
                .unwrap_err()
                .contains("正在退出")
        );
        assert!(
            crate::analysis::submit(rt.clone(), &mid, "stage", "", "all")
                .unwrap_err()
                .contains("正在退出")
        );
        drop(rt);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    #[ignore = "explicit HELMMEET_TEST_MODEL needed; real local Metal, synthetic PCM only"]
    fn real_metal_worker_releases_cached_context_and_cancelled_work() {
        whisper_rs::install_logging_hooks();
        let model =
            std::env::var_os("HELMMEET_TEST_MODEL").expect("set HELMMEET_TEST_MODEL explicitly");
        for phase in ["idle", "busy", "loading"] {
            let root = std::env::temp_dir().join(format!("helmmeet-metal-exit-{}", id()));
            let rt =
                Runtime::open_with_vault(root.clone(), Arc::new(MemoryVault::default())).unwrap();
            let mid = prepare(&rt, Path::new(&model)).unwrap();
            let worker = asr::spawn(rt.clone());
            let until = Instant::now() + Duration::from_secs(90);
            while !reached(&rt, &mid, phase) {
                assert!(Instant::now() < until, "ASR did not reach {phase}");
                std::thread::sleep(Duration::from_millis(2));
            }
            rt.shutting_down
                .store(true, std::sync::atomic::Ordering::Release);
            let began = Instant::now();
            stop_workers(vec![worker]).unwrap();
            let db = rt.db().unwrap();
            let covered: u64 = db
                .query_row(
                    "SELECT count(*) FROM asr_chunks WHERE meeting_id=?",
                    [&mid],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(covered, u64::from(phase == "idle"));
            let chunks: u64 = db
                .query_row(
                    "SELECT count(*) FROM audio_chunks WHERE meeting_id=? AND status='sealed'",
                    [&mid],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(chunks, 1, "shutdown must preserve recoverable audio");
            println!(
                "Metal worker {phase} released in {}ms; checkpoint verified",
                began.elapsed().as_millis()
            );
            drop(db);
            drop(rt);
            std::fs::remove_dir_all(root).unwrap();
        }
    }
}
