//! Precomputed, voice-matched wake responses. The wake path only reads memory.
use crate::{db::Runtime, lifecycle::Worker};
use base64::Engine;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

// A complete declarative sentence gives very short neural TTS a sentence end.
// The recipe is part of identity: never relabel/reuse old, possibly faulty PCM.
pub const TEXT: &str = "我在。";
const RECIPE: u32 = 2;
const RATE: usize = 16000;
const MAX_FILE: u64 = 240_000;
#[derive(Default)]
struct Entry {
    key: String,
    speaker: String,
    pcm: Option<Arc<[f32]>>,
    error: Option<String>,
    phase: &'static str,
    refreshing: bool,
}
#[derive(Default)]
pub struct Cache {
    epoch: AtomicU64,
    force_epoch: AtomicU64,
    entry: Mutex<Entry>,
    synthesis: Mutex<()>,
}
pub(crate) fn synthesis_guard<'a>(
    cache: &'a Cache,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<std::sync::MutexGuard<'a, ()>, String> {
    loop {
        check()?;
        match cache.synthesis.try_lock() {
            Ok(guard) => return Ok(guard),
            Err(std::sync::TryLockError::Poisoned(e)) => return Ok(e.into_inner()),
            Err(std::sync::TryLockError::WouldBlock) => {
                std::thread::sleep(Duration::from_millis(20))
            }
        }
    }
}
impl Cache {
    pub fn invalidate(&self) {
        self.reset(false);
    }
    pub fn refresh(&self) {
        self.reset(true);
    }
    fn reset(&self, force: bool) {
        let mut entry = self.entry.lock().unwrap_or_else(|e| e.into_inner());
        let epoch = self.epoch.fetch_add(1, Ordering::AcqRel).wrapping_add(1);
        self.force_epoch.store(
            if force { epoch.wrapping_add(1) } else { 0 },
            Ordering::Release,
        );
        *entry = Entry::default();
    }
    pub fn status(&self) -> Value {
        let e = self.entry.lock().unwrap_or_else(|e| e.into_inner());
        json!({"state":if e.phase.is_empty(){"preparing"}else{e.phase},"speaker":e.speaker,"available":e.pcm.is_some(),"refreshing":e.refreshing,"error":e.error})
    }
    pub fn pcm(&self, config: &Value) -> Option<Arc<[f32]>> {
        let key = fingerprint(config)?;
        let e = self.entry.lock().ok()?;
        (e.key == key).then(|| e.pcm.clone()).flatten()
    }
    fn begin(&self, epoch: u64, key: &str, speaker: &str, cached: Option<Vec<f32>>) -> bool {
        let mut e = self.entry.lock().unwrap_or_else(|e| e.into_inner());
        if self.epoch.load(Ordering::Acquire) != epoch {
            return false;
        }
        *e = Entry {
            key: key.into(),
            speaker: speaker.into(),
            pcm: cached.map(Arc::from),
            error: None,
            phase: if key.is_empty() {
                "disabled"
            } else {
                "preparing"
            },
            refreshing: !key.is_empty(),
        };
        if e.pcm.is_some() {
            e.phase = "ready";
        }
        true
    }
    fn finish(&self, epoch: u64, key: &str, result: Result<Vec<f32>, String>) -> bool {
        let mut e = self.entry.lock().unwrap_or_else(|e| e.into_inner());
        if self.epoch.load(Ordering::Acquire) != epoch || e.key != key {
            return false;
        }
        e.refreshing = false;
        match result {
            Ok(pcm) => {
                e.pcm = Some(Arc::from(pcm));
                e.phase = "ready";
                e.error = None;
            }
            Err(error) => {
                e.phase = if e.pcm.is_some() { "ready" } else { "failed" };
                e.error = Some(error);
            }
        }
        true
    }
    fn reused(&self, epoch: u64, key: &str) {
        let mut e = self.entry.lock().unwrap_or_else(|e| e.into_inner());
        if self.epoch.load(Ordering::Acquire) == epoch && e.key == key {
            e.refreshing = false;
        }
    }
}
pub(crate) fn fingerprint(config: &Value) -> Option<String> {
    if config["enabled"] != true {
        return None;
    }
    Some(format!(
        "{:x}",
        Sha256::digest(
            serde_json::to_vec(&json!([
                RECIPE,
                TEXT,
                RATE,
                "agent-plan-tts-unidirectional",
                config["model"],
                config["speaker"],
                config["profile_id"],
                config["config_version"]
            ]))
            .ok()?
        )
    ))
}
pub(crate) fn valid(pcm: &[f32]) -> Result<(), String> {
    if !(RATE / 10..=RATE * 5).contains(&pcm.len())
        || !pcm.iter().all(|v| v.is_finite() && v.abs() <= 1.)
        || !pcm.iter().any(|v| v.abs() > 0.001)
    {
        return Err("唤醒回应音频无效（需为0.1–5秒的有效声音）".into());
    }
    Ok(())
}
fn directory(root: &Path) -> PathBuf {
    root.join("cache").join("wake-ack-v1")
}
pub(crate) fn load(root: &Path, key: &str) -> Result<Vec<f32>, String> {
    let path = directory(root).join(format!("{key}.json"));
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let file = options.open(path).map_err(|_| "没有可用的唤醒回应缓存")?;
    if file.metadata().map_err(|_| "回应缓存无效")?.len() > MAX_FILE {
        return Err("回应缓存过大".into());
    }
    let mut raw = Vec::new();
    file.take(MAX_FILE + 1)
        .read_to_end(&mut raw)
        .map_err(|_| "回应缓存读取失败")?;
    if raw.len() as u64 > MAX_FILE {
        return Err("回应缓存过大".into());
    }
    let v: Value = serde_json::from_slice(&raw).map_err(|_| "回应缓存格式无效")?;
    if v["key"] != key || v["version"] != 1 || v["sample_rate"] != RATE {
        return Err("回应缓存配置不匹配".into());
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(v["pcm"].as_str().ok_or("回应缓存无音频")?)
        .map_err(|_| "回应缓存编码无效")?;
    if bytes.len() % 2 != 0 || v["sha256"] != format!("{:x}", Sha256::digest(&bytes)) {
        return Err("回应缓存校验失败".into());
    }
    let pcm: Vec<f32> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|p| i16::from_le_bytes([p[0], p[1]]) as f32 / 32768.)
        .collect();
    valid(&pcm)?;
    Ok(pcm)
}
pub(crate) fn save(root: &Path, key: &str, pcm: &[f32]) -> Result<(), String> {
    valid(pcm)?;
    for dir in [root.join("cache"), directory(root)] {
        match fs::symlink_metadata(&dir) {
            Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => {}
            Ok(_) => return Err("回应缓存目录无效".into()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir(&dir).map_err(|_| "回应缓存目录创建失败")?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))
                        .map_err(|_| "回应缓存权限设置失败")?;
                }
            }
            Err(_) => return Err("回应缓存目录不可用".into()),
        }
    }
    let bytes: Vec<u8> = pcm
        .iter()
        .flat_map(|v| ((v * 32768.).round().clamp(-32768., 32767.) as i16).to_le_bytes())
        .collect();
    let raw=serde_json::to_vec(&json!({"version":1,"key":key,"sample_rate":RATE,"sha256":format!("{:x}",Sha256::digest(&bytes)),"pcm":base64::engine::general_purpose::STANDARD.encode(bytes)})).map_err(|_| "回应缓存序列化失败")?;
    let temp = directory(root).join(format!(".{}.tmp", crate::db::id()));
    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temp).map_err(|_| "回应缓存创建失败")?;
        file.write_all(&raw)
            .and_then(|_| file.sync_all())
            .map_err(|_| "回应缓存保存失败")?;
        fs::rename(&temp, directory(root).join(format!("{key}.json")))
            .map_err(|_| "回应缓存提交失败")
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result.map_err(str::to_owned)
}
pub fn background(rt: Arc<Runtime>) -> Worker {
    background_with(rt, |rt, config, check| {
        crate::tts::synthesize_snapshot(rt, config, TEXT, check)
    })
}
fn background_with(
    rt: Arc<Runtime>,
    synthesize: impl Fn(&Runtime, &Value, &dyn Fn() -> Result<(), String>) -> Result<Vec<f32>, String>
        + Send
        + 'static,
) -> Worker {
    Worker::spawn(move |stop| {
        let mut observed = None;
        while !stop.is_stopped() && !rt.shutting_down.load(Ordering::Acquire) {
            let config = match crate::tts::saved(&rt) {
                Ok(v) => v,
                Err(_) => {
                    stop.wait(Duration::from_millis(250));
                    continue;
                }
            };
            let epoch = rt.voice.ack.epoch.load(Ordering::Acquire);
            let key = fingerprint(&config).unwrap_or_default();
            if observed.as_ref() == Some(&(key.clone(), epoch)) {
                stop.wait(Duration::from_millis(250));
                continue;
            }
            observed = Some((key.clone(), epoch));
            let cached = if key.is_empty() {
                None
            } else {
                load(&rt.root, &key).ok()
            };
            let forced = rt.voice.ack.force_epoch.load(Ordering::Acquire) == epoch.wrapping_add(1);
            let reuse = cached.is_some() && !forced;
            if !rt.voice.ack.begin(
                epoch,
                &key,
                config["speaker"].as_str().unwrap_or(""),
                cached,
            ) || key.is_empty()
            {
                continue;
            }
            let _ = rt.voice.ack.force_epoch.compare_exchange(
                epoch.wrapping_add(1),
                0,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
            if reuse {
                rt.voice.ack.reused(epoch, &key);
                continue;
            }
            let check = || {
                if stop.is_stopped()
                    || rt.shutting_down.load(Ordering::Acquire)
                    || rt.voice.ack.epoch.load(Ordering::Acquire) != epoch
                {
                    Err("唤醒回应准备已取消".into())
                } else {
                    Ok(())
                }
            };
            // Cache miss or explicit regeneration only. A restart/voice switch
            // with valid stored audio must not incur another API request.
            let Ok(_synthesis) = synthesis_guard(&rt.voice.ack, &check) else {
                continue;
            };
            // An explicit preview may have filled this cache while we waited.
            let result = if !forced {
                load(&rt.root, &key).ok()
            } else {
                None
            }
            .map_or_else(|| synthesize(&rt, &config, &check), Ok)
            .and_then(|pcm| {
                check()?;
                valid(&pcm)?;
                Ok(pcm)
            });
            if check().is_ok() {
                if let Ok(pcm) = &result {
                    // A disk error does not discard valid memory audio or block listening.
                    if let Err(error) = save(&rt.root, &key, pcm) {
                        if rt.voice.ack.finish(epoch, &key, Ok(pcm.clone())) {
                            let mut e =
                                rt.voice.ack.entry.lock().unwrap_or_else(|e| e.into_inner());
                            if e.key == key && rt.voice.ack.epoch.load(Ordering::Acquire) == epoch {
                                e.error = Some(error);
                            }
                        }
                        continue;
                    }
                }
                rt.voice.ack.finish(epoch, &key, result);
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{atomic::AtomicUsize, mpsc};
    use std::time::Instant;
    fn legacy_key(config: &Value) -> String {
        format!(
            "{:x}",
            Sha256::digest(
                serde_json::to_vec(&json!([
                    1,
                    "我在",
                    RATE,
                    "agent-plan-tts-unidirectional",
                    config["model"],
                    config["speaker"],
                    config["profile_id"],
                    config["config_version"]
                ]))
                .unwrap()
            )
        )
    }
    #[test]
    #[ignore = "explicit local model and saved fixed wake caches only; read-only, no network/playback/capture"]
    fn inspect_saved_short_voice_content_offline() {
        let root = PathBuf::from(
            std::env::var_os("HELMMEET_WAKE_CHECK_ROOT").expect("explicit data root"),
        );
        let model = std::env::var("HELMMEET_WAKE_CHECK_MODEL").expect("explicit local model");
        let db = rusqlite::Connection::open_with_flags(
            root.join("app.sqlite"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let raw: String = db
            .query_row(
                "SELECT value FROM app_preferences WHERE key='tts_settings'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let mut cfg: Value = serde_json::from_str(&raw).unwrap();
        let ctx =
            whisper_rs::WhisperContext::new_with_params(&model, crate::asr::context_parameters())
                .unwrap();
        let mut verified = Vec::new();
        for speaker in [
            "zh_female_xiaohe_uranus_bigtts",
            "zh_female_kefunvsheng_uranus_bigtts",
        ] {
            cfg["speaker"] = json!(speaker);
            let pcm = load(&root, &legacy_key(&cfg)).unwrap();
            let text = crate::asr::decode(&ctx, &pcm, "zh").unwrap();
            println!(
                "Fixed cached {speaker}, {:.3}s: {text:?}",
                pcm.len() as f64 / RATE as f64
            );
            let mut padded = vec![0.; RATE / 2];
            padded.extend_from_slice(&pcm);
            padded.resize(padded.len() + RATE / 2, 0.);
            println!(
                "Padded cache {speaker}: {:?}",
                crate::asr::decode(&ctx, &padded, "zh").unwrap()
            );
            if let Some(folder) = std::env::var_os("HELMMEET_WAKE_AUDIT_OUTPUT") {
                let bytes =
                    std::fs::read(PathBuf::from(folder).join(format!("{speaker}.pcm"))).unwrap();
                let pcm: Vec<f32> = bytes
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|p| i16::from_le_bytes(*p) as f32 / 32768.)
                    .collect();
                let result = crate::asr::decode(&ctx, &pcm, "zh").unwrap();
                println!(
                    "Punctuated voice {speaker}, {:.3}s: {:?}",
                    pcm.len() as f64 / RATE as f64,
                    result
                );
                let spoken: String = result
                    .iter()
                    .flat_map(|(_, _, text)| text.chars())
                    .filter(|c| c.is_alphanumeric())
                    .collect();
                assert_eq!(
                    spoken, "我在",
                    "The new fixed response must contain both characters and no added words"
                );
                verified.push((fingerprint(&cfg).unwrap(), pcm));
            }
        }
        if std::env::var("HELMMEET_WAKE_INSTALL_VALIDATED").as_deref() == Ok("1") {
            assert_eq!(
                verified.len(),
                2,
                "Both audited fixed voices must pass before caching"
            );
            for (key, pcm) in verified {
                assert!(
                    !directory(&root).join(format!("{key}.json")).exists(),
                    "Never overwrite an existing voice in this diagnostic"
                );
                save(&root, &key, &pcm).unwrap();
                assert_eq!(load(&root, &key).unwrap(), pcm);
            }
            println!("Installed two verified fixed wake responses under the new recipe; no settings/meeting writes");
        }
    }
    fn config(speaker: &str) -> Value {
        json!({"enabled":true,"model":"seed-tts-2.0-standard","speaker":speaker,"profile_id":"synthetic-profile","config_version":1,"fallback_local":true})
    }
    fn wait(check: impl Fn() -> bool) {
        let end = Instant::now() + Duration::from_secs(3);
        while !check() {
            assert!(Instant::now() < end, "wake cache worker timed out");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    #[test]
    fn new_sentence_recipe_rebuilds_once_without_reusing_or_deleting_legacy_audio() {
        let root = tempfile::tempdir().unwrap();
        let rt = Runtime::open_with_vault(
            root.path().into(),
            Arc::new(crate::models::tests::MemoryVault::default()),
        )
        .unwrap();
        let cfg = config("synthetic-voice");
        rt.db()
            .unwrap()
            .execute(
                "UPDATE app_preferences SET value=? WHERE key='tts_settings'",
                [cfg.to_string()],
            )
            .unwrap();
        let old = legacy_key(&cfg);
        let next = fingerprint(&cfg).unwrap();
        assert_ne!(old, next);
        save(root.path(), &old, &vec![0.75; RATE]).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = calls.clone();
        let worker = background_with(rt.clone(), move |_, _, _| {
            observed.fetch_add(1, Ordering::AcqRel);
            Ok(vec![0.25; RATE])
        });
        wait(|| rt.voice.ack.pcm(&cfg).is_some());
        assert_eq!(calls.load(Ordering::Acquire), 1);
        assert_eq!(rt.voice.ack.pcm(&cfg).unwrap()[0], 0.25);
        assert_eq!(load(root.path(), &old).unwrap()[0], 0.75);
        assert_eq!(load(root.path(), &next).unwrap()[0], 0.25);
        drop(worker);
        rt.voice.ack.invalidate();
        let worker = background_with(rt.clone(), |_, _, _| {
            panic!("A valid new recipe must be reused on restart")
        });
        wait(|| rt.voice.ack.pcm(&cfg).is_some());
        assert_eq!(rt.voice.ack.pcm(&cfg).unwrap()[0], 0.25);
        drop(worker);
    }
    #[test]
    fn cache_identity_codec_and_invalid_audio_are_checked_without_playback() {
        let root = tempfile::tempdir().unwrap();
        let a = config("a");
        let key = fingerprint(&a).unwrap();
        assert_ne!(fingerprint(&config("b")).unwrap(), key);
        let mut other = a.clone();
        other["config_version"] = json!(2);
        assert_ne!(fingerprint(&other).unwrap(), key);
        other = a.clone();
        other["model"] = json!("other-model");
        assert_ne!(fingerprint(&other).unwrap(), key);
        other = a.clone();
        other["fallback_local"] = json!(false);
        assert_eq!(fingerprint(&other).unwrap(), key);
        other["enabled"] = json!(false);
        assert!(fingerprint(&other).is_none());
        let pcm = vec![0.25; RATE];
        save(root.path(), &key, &pcm).unwrap();
        assert_eq!(load(root.path(), &key).unwrap(), pcm);
        assert!(load(root.path(), &fingerprint(&config("b")).unwrap()).is_err());
        let path = directory(root.path()).join(format!("{key}.json"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        let mut value: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        value["sha256"] = json!("broken");
        fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(load(root.path(), &key).is_err());
        for pcm in [
            vec![],
            vec![0.; RATE],
            vec![f32::NAN; RATE],
            vec![2.; RATE],
            vec![0.25; RATE * 6],
        ] {
            assert!(valid(&pcm).is_err());
        }
    }
    #[test]
    fn mismatched_or_late_voice_can_never_be_selected() {
        let cache = Cache::default();
        let a = config("a");
        let b = config("b");
        let ka = fingerprint(&a).unwrap();
        let kb = fingerprint(&b).unwrap();
        assert!(cache.begin(0, &ka, "a", None));
        cache.invalidate();
        assert!(!cache.finish(0, &ka, Ok(vec![0.5; RATE])));
        assert!(cache.pcm(&a).is_none());
        assert!(cache.begin(1, &kb, "b", None));
        assert!(cache.finish(1, &kb, Ok(vec![0.25; RATE])));
        assert!(cache.pcm(&a).is_none());
        assert_eq!(cache.pcm(&b).unwrap()[0], 0.25);
        assert!(!cache.finish(0, &ka, Ok(vec![0.75; RATE])));
        assert_eq!(cache.pcm(&b).unwrap()[0], 0.25);
    }
    #[test]
    fn worker_supersedes_inflight_voice_reuses_cache_and_joins_on_shutdown() {
        let root = tempfile::tempdir().unwrap();
        let rt = Runtime::open_with_vault(
            root.path().into(),
            Arc::new(crate::models::tests::MemoryVault::default()),
        )
        .unwrap();
        let update = |v: &Value| {
            rt.db()
                .unwrap()
                .execute(
                    "UPDATE app_preferences SET value=? WHERE key='tts_settings'",
                    [v.to_string()],
                )
                .unwrap()
        };
        update(&config("old"));
        let count = Arc::new(AtomicUsize::new(0));
        let calls = count.clone();
        let (tx, rx) = mpsc::sync_channel(1);
        let worker = background_with(rt.clone(), move |_, v, check| {
            calls.fetch_add(1, Ordering::AcqRel);
            if v["speaker"] == "old" {
                tx.send(()).unwrap();
                while check().is_ok() {
                    std::thread::sleep(Duration::from_millis(5));
                }
                // An adapter may still return late: the worker must discard it.
                return Ok(vec![0.75; RATE]);
            }
            Ok(vec![0.25; RATE])
        });
        rx.recv_timeout(Duration::from_secs(2)).unwrap();
        update(&config("new"));
        rt.voice.ack.invalidate();
        wait(|| rt.voice.ack.pcm(&config("new")).is_some());
        assert!(rt.voice.ack.pcm(&config("old")).is_none());
        wait(|| load(root.path(), &fingerprint(&config("new")).unwrap()).is_ok());
        assert!(load(root.path(), &fingerprint(&config("old")).unwrap()).is_err());
        assert_eq!(count.load(Ordering::Acquire), 2);
        std::thread::sleep(Duration::from_millis(550));
        assert_eq!(
            count.load(Ordering::Acquire),
            2,
            "unchanged settings must not issue another synthesis"
        );
        drop(worker);
        let (tx, rx) = mpsc::sync_channel(1);
        let worker = background_with(rt.clone(), move |_, _, check| {
            tx.send(()).unwrap();
            while check().is_ok() {
                std::thread::sleep(Duration::from_millis(5));
            }
            Err("cancelled".into())
        });
        assert!(
            rx.recv_timeout(Duration::from_millis(600)).is_err(),
            "restart must reuse persistent audio without synthesis"
        );
        assert!(rt.voice.ack.status()["available"] == true);
        assert!(rt.voice.ack.status()["refreshing"] == false);
        rt.voice.ack.refresh();
        rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(rt.voice.ack.status()["available"] == true);
        assert!(rt.voice.ack.status()["refreshing"] == true);
        assert_eq!(rt.voice.ack.pcm(&config("new")).unwrap()[0], 0.25);
        let began = Instant::now();
        drop(worker);
        assert!(began.elapsed() < Duration::from_secs(1));
    }
    #[test]
    fn failed_refresh_keeps_only_matching_cache_and_never_enables_a_wrong_voice() {
        let cache = Cache::default();
        let config = config("current");
        let key = fingerprint(&config).unwrap();
        cache.begin(0, &key, "current", Some(vec![0.25; RATE]));
        cache.finish(0, &key, Err("synthetic network failure".into()));
        assert!(cache.pcm(&config).is_some());
        assert!(cache.status()["error"].is_string());
        cache.invalidate();
        cache.begin(1, &key, "current", None);
        cache.finish(1, &key, Err("synthetic failure".into()));
        assert!(cache.pcm(&config).is_none());
        assert_eq!(cache.status()["state"], "failed");
    }
    #[test]
    fn refresh_action_does_not_require_or_activate_a_meeting_microphone() {
        let root = tempfile::tempdir().unwrap();
        let rt = Runtime::open_with_vault(
            root.path().into(),
            Arc::new(crate::models::tests::MemoryVault::default()),
        )
        .unwrap();
        assert!(crate::voice::command(&rt, "refresh_ack").is_err());
        rt.db()
            .unwrap()
            .execute(
                "UPDATE app_preferences SET value=? WHERE key='tts_settings'",
                [config("a").to_string()],
            )
            .unwrap();
        let capture = rt.voice.epoch.load(Ordering::Acquire);
        let speech = rt.voice.speech_epoch.load(Ordering::Acquire);
        assert_eq!(
            crate::voice::command(&rt, "refresh_ack").unwrap()["accepted"],
            true
        );
        assert_eq!(rt.voice.epoch.load(Ordering::Acquire), capture);
        assert_eq!(rt.voice.speech_epoch.load(Ordering::Acquire), speech);
        assert_eq!(crate::voice::status(&rt).unwrap()["state"], "off");
    }
    #[test]
    fn switching_between_saved_voices_never_calls_the_service() {
        let root = tempfile::tempdir().unwrap();
        let rt = Runtime::open_with_vault(
            root.path().into(),
            Arc::new(crate::models::tests::MemoryVault::default()),
        )
        .unwrap();
        for (speaker, volume) in [("a", 0.25), ("b", 0.5)] {
            save(
                root.path(),
                &fingerprint(&config(speaker)).unwrap(),
                &vec![volume; RATE],
            )
            .unwrap();
        }
        let update = |speaker: &str| {
            rt.db()
                .unwrap()
                .execute(
                    "UPDATE app_preferences SET value=? WHERE key='tts_settings'",
                    [config(speaker).to_string()],
                )
                .unwrap();
            rt.voice.ack.invalidate();
        };
        update("a");
        let count = Arc::new(AtomicUsize::new(0));
        let calls = count.clone();
        let worker = background_with(rt.clone(), move |_, _, _| {
            calls.fetch_add(1, Ordering::AcqRel);
            Err("must use cache".into())
        });
        wait(|| rt.voice.ack.pcm(&config("a")).is_some());
        update("b");
        wait(|| rt.voice.ack.pcm(&config("b")).is_some());
        assert_eq!(rt.voice.ack.pcm(&config("b")).unwrap()[0], 0.5);
        update("a");
        wait(|| rt.voice.ack.pcm(&config("a")).is_some());
        assert_eq!(rt.voice.ack.pcm(&config("a")).unwrap()[0], 0.25);
        assert_eq!(count.load(Ordering::Acquire), 0);
        drop(worker);
    }
    #[cfg(unix)]
    #[test]
    fn cache_rejects_symlink_paths_without_touching_external_files() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        symlink(outside.path(), root.path().join("cache")).unwrap();
        assert!(save(root.path(), "synthetic", &vec![0.25; RATE]).is_err());
        assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 0);
        fs::remove_file(root.path().join("cache")).unwrap();
        save(root.path(), "synthetic", &vec![0.25; RATE]).unwrap();
        let path = directory(root.path()).join("synthetic.json");
        let external = outside.path().join("audio.json");
        fs::write(&external, b"unchanged").unwrap();
        fs::remove_file(&path).unwrap();
        symlink(&external, &path).unwrap();
        assert!(load(root.path(), "synthetic").is_err());
        assert_eq!(fs::read(external).unwrap(), b"unchanged");
    }
}
