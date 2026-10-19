//! Explicit, bounded real-device diagnostic. No ASR, HTTP, personal config or Keychain.
use crate::{
    audio,
    config::Config,
    db::{err, Runtime},
    enhancement,
    models::CredentialStore,
};
use serde_json::{json, Value};
use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

struct NoCredentials;
impl CredentialStore for NoCredentials {
    fn get(&self, _: &str) -> Result<Option<String>, String> {
        Ok(None)
    }
    fn put(&self, _: &str, _: &str) -> Result<(), String> {
        Err("Acoustic probe has no credentials".into())
    }
    fn remove(&self, _: &str) -> Result<(), String> {
        Ok(())
    }
}
extern "C" {
    fn hm_audio_pump_events();
}

pub fn run() -> Result<Value, String> {
    if !enhancement::AVAILABLE {
        return Err("Build with audio-processing".into());
    }
    let root = PathBuf::from(std::env::var_os("HELMMEET_ACOUSTIC_ROOT").ok_or(
        "Explicit HELMMEET_ACOUSTIC_ROOT required; real microphone and system audio will be saved",
    )?);
    let seconds: u64 = std::env::var("HELMMEET_ACOUSTIC_SECONDS")
        .unwrap_or_else(|_| "65".into())
        .parse()
        .map_err(|_| "Invalid duration")?;
    if !(5..=90).contains(&seconds) {
        return Err("Duration must be 5..90 seconds".into());
    }
    // Refuse a user's existing data directory, even if it has no meetings yet.
    std::fs::create_dir(&root).map_err(err)?;
    Config::default().save(&root)?;
    if let Some(source) = std::env::var_os("HELMMEET_ACOUSTIC_REPLAY") {
        return replay(root, PathBuf::from(source));
    }
    let worker = std::thread::spawn(move || capture(root, seconds));
    while !worker.is_finished() {
        // SAFETY: the explicit CLI owns the main thread; keep native notifications alive.
        unsafe { hm_audio_pump_events() };
    }
    worker
        .join()
        .map_err(|_| "Acoustic diagnostic thread failed")?
}

fn capture(root: PathBuf, seconds: u64) -> Result<Value, String> {
    let rt = Runtime::open_with_vault(root.clone(), Arc::new(NoCredentials))?;
    let route = audio::output_route_status();
    if route["aec_supported"] != true {
        return Err("Default output is unavailable or unsupported; capture not started".into());
    }
    let mid = rt.create("本地外放音质诊断（真实音频）", "general")?["id"]
        .as_str()
        .ok_or("Missing ID")?
        .to_string();
    enhancement::configure_options(&rt, &mid, "off", Some(true))?;
    let mut processor = enhancement::background(rt.clone());
    audio::start(rt.clone(), &mid, 3, 0, false)?;
    // This scope guarantees stop/join on every ordinary failure after native capture starts.
    let result: Result<(), String> = (|| {
        std::fs::write(
            root.join("ready.json"),
            serde_json::to_vec(
                &json!({"meeting_id":mid,"seconds":seconds,"route":route,"capture_started":true}),
            )
            .map_err(err)?,
        )
        .map_err(err)?;
        let began = Instant::now();
        while began.elapsed() < Duration::from_secs(seconds) && !root.join("stop").exists() {
            let state: String = rt
                .db()?
                .query_row("SELECT state FROM meetings WHERE id=?", [&mid], |r| {
                    r.get(0)
                })
                .map_err(err)?;
            if state != "recording" {
                return Err("Capture interrupted; raw files retained".into());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        Ok(())
    })();
    let stopped = audio::control(&rt, &mid, "stop");
    result?;
    stopped?;
    finish(&rt, &mid, &mut processor, route)
}

fn replay(root: PathBuf, source: PathBuf) -> Result<Value, String> {
    // Only snapshots produced by this explicit diagnostic are accepted. Copy raw
    // files and SQLite into a fresh root; never alter the captured originals.
    let report: Value =
        serde_json::from_slice(&std::fs::read(source.join("capture.json")).map_err(err)?)
            .map_err(err)?;
    if report["asr_started"] != false || report["network_models_started"] != false {
        return Err("Not an acoustic diagnostic snapshot".into());
    }
    let mid = report["meeting_id"]
        .as_str()
        .ok_or("Missing diagnostic meeting")?;
    let conn = rusqlite::Connection::open_with_flags(
        source.join("app.sqlite"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .map_err(err)?;
    let mut copy = rusqlite::Connection::open(root.join("app.sqlite")).map_err(err)?;
    rusqlite::backup::Backup::new(&conn, &mut copy)
        .map_err(err)?
        .run_to_completion(64, Duration::from_millis(10), None)
        .map_err(err)?;
    copy.execute_batch("PRAGMA foreign_keys=ON; DELETE FROM enhanced_chunks; UPDATE audio_enhancement SET generation=generation+1,state='queued',error=NULL;").map_err(err)?;
    let mut q = conn
        .prepare("SELECT relative_path FROM audio_chunks WHERE meeting_id=? AND status='sealed'")
        .map_err(err)?;
    for path in q.query_map([mid], |r| r.get::<_, String>(0)).map_err(err)? {
        let path = path.map_err(err)?;
        let input = crate::db::safe_relative(&source, &path)?;
        let output = root.join(&path);
        // safe_relative rejects absolute paths and parent components.
        std::fs::create_dir_all(output.parent().ok_or("Missing parent")?).map_err(err)?;
        std::fs::copy(input, output).map_err(err)?;
    }
    drop(copy);
    let rt = Runtime::open_with_vault(root, Arc::new(NoCredentials))?;
    let mut processor = enhancement::background(rt.clone());
    finish(&rt, mid, &mut processor, report["route"].clone())
}

fn finish(
    rt: &Runtime,
    mid: &str,
    processor: &mut crate::lifecycle::Worker,
    route: Value,
) -> Result<Value, String> {
    let root = &rt.root;
    let until = Instant::now() + Duration::from_secs(30);
    loop {
        let status = enhancement::detail(rt, mid)?;
        if status["state"] == "complete" {
            break;
        }
        if status["state"] == "failed" || Instant::now() >= until {
            return Err(format!("Enhancement did not complete: {status}"));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    processor.join().map_err(str::to_string)?;
    let duration: u64 = rt
        .db()?
        .query_row(
            "SELECT recording_duration_ms FROM meetings WHERE id=?",
            [mid],
            |r| r.get(0),
        )
        .map_err(err)?;
    for (name, source) in [
        ("microphone-raw.wav", Some("microphone")),
        ("system-reference.wav", Some("system")),
        ("microphone-aec.wav", None),
    ] {
        let mut wav = audio::WavFile::create(&root.join(name))?;
        for start in (0..duration).step_by(30000) {
            let end = (start + 30000).min(duration);
            let pcm = match source {
                Some(source) => crate::asr::mix_source(rt, mid, start, end, Some(source))?,
                None => enhancement::mix(rt, mid, start, end, true)?,
            };
            wav.write(&pcm)?;
        }
        wav.sync()?;
    }
    let report = json!({"meeting_id":mid,"duration_ms":duration,"enhancement":enhancement::detail(rt,mid)?,"route":route,"asr_started":false,"network_models_started":false,"noise_suppression":"off","human_double_talk_verified":false});
    std::fs::write(
        root.join("capture.json"),
        serde_json::to_vec_pretty(&report).map_err(err)?,
    )
    .map_err(err)?;
    Ok(report)
}
