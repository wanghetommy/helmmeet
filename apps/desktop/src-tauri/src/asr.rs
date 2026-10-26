use crate::{
    audio,
    db::{err, id, now, safe_relative, Runtime},
    lifecycle::{StopToken, Worker},
};
use meeting_core::coverage::{missing_intervals, Interval};
use rusqlite::params;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::{process::Command, sync::Arc, time::Duration};
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

// The ASR worker alone owns the cached Whisper context. Preparation sends a
// bounded request to that worker; it never loads a second context on an IPC thread.
#[derive(Default)]
pub struct Preparation {
    pending:
        std::sync::Mutex<std::collections::VecDeque<std::sync::mpsc::Sender<Result<bool, String>>>>,
}
pub fn prepare(rt: &Runtime) -> Result<serde_json::Value, String> {
    rt.ensure_running()?;
    if rt
        .config
        .lock()
        .map_err(|_| "配置锁异常")?
        .asr
        .model_path
        .is_empty()
    {
        return Err("请先在全局设置选择本地转写模型".into());
    }
    let (tx, rx) = std::sync::mpsc::channel();
    {
        let mut pending = rt
            .asr_preparation
            .pending
            .lock()
            .map_err(|_| "转写准备锁异常")?;
        if pending.len() >= 4 {
            return Err("正在准备转写模型，请稍后重试".into());
        }
        pending.push_back(tx);
    }
    let until = std::time::Instant::now() + Duration::from_secs(180);
    loop {
        rt.ensure_running()?;
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(result) => {
                let loaded = result?;
                return Ok(json!({"ready":true,"cached":!loaded}));
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                return Err("转写服务已停止，请重启应用".into())
            }
            Err(_) if std::time::Instant::now() >= until => {
                return Err("转写模型准备超时，请检查模型后重试".into())
            }
            Err(_) => {}
        }
    }
}
fn load_context(
    config: &crate::config::AsrConfig,
    stop: &StopToken,
    loaded: &mut String,
    context: &mut Option<WhisperContext>,
    backend: &mut Option<bool>,
) -> Result<bool, String> {
    if config.model_path.is_empty() {
        return Err("请先选择本地转写模型".into());
    }
    // Include the checksum in the cache identity; changing the selected model
    // during preparation must not silently reuse an incompatible context.
    let key = format!(
        "{}:{}",
        config.model_path,
        config.model_sha256.as_deref().unwrap_or("")
    );
    if *loaded == key && context.is_some() {
        return Ok(false);
    }
    if *loaded != key {
        *backend = None;
    }
    if let Some(expected) = &config.model_sha256 {
        let mut file = std::fs::File::open(&config.model_path).map_err(|_| "模型文件不可读")?;
        let mut hash = Sha256::new();
        let mut bytes = [0u8; 1024 * 1024];
        loop {
            if stop.is_stopped() {
                return Err("转写已取消".into());
            }
            let n = file.read(&mut bytes).map_err(|_| "模型文件不可读")?;
            if n == 0 {
                break;
            }
            hash.update(&bytes[..n]);
        }
        if format!("{:x}", hash.finalize()) != *expected {
            return Err("模型 SHA-256 不匹配".into());
        }
    }
    if stop.is_stopped() {
        return Err("转写已取消".into());
    }
    let use_gpu = *backend.get_or_insert_with(|| probe_metal(&config.model_path));
    let next = match WhisperContext::new_with_params(
        &config.model_path,
        context_parameters_for_gpu(use_gpu),
    ) {
        Ok(context) => context,
        Err(_) if use_gpu => {
            // A successful child probe can still race with a driver/resource
            // change. Retry once on CPU so a recoverable Metal load error does
            // not make ASR unavailable.
            *backend = Some(false);
            WhisperContext::new_with_params(&config.model_path, context_parameters_for_gpu(false))
                .map_err(|_| "模型加载失败，请检查模型文件")?
        }
        Err(_) => return Err("模型加载失败，请检查模型文件".into()),
    };
    if stop.is_stopped() {
        return Err("转写已取消".into());
    }
    *context = Some(next);
    *loaded = key;
    Ok(true)
}

pub fn context_parameters() -> WhisperContextParameters<'static> {
    context_parameters_for_gpu(false)
}

fn context_parameters_for_gpu(use_gpu: bool) -> WhisperContextParameters<'static> {
    let mut params = WhisperContextParameters::default();
    params.use_gpu(use_gpu);
    params
}

pub(crate) fn context_parameters_for_probe() -> WhisperContextParameters<'static> {
    context_parameters_for_gpu(true)
}

/// Probe Metal in a child process. whisper.cpp's Metal allocator can raise a
/// native SIGSEGV on this machine, which Rust cannot catch with `Result` or
/// `catch_unwind`. Isolating the probe keeps the main application alive and
/// lets it select CPU only when Metal is actually unavailable.
fn probe_metal(model_path: &str) -> bool {
    let Ok(executable) = std::env::current_exe() else {
        return false;
    };
    Command::new(executable)
        .arg("--probe-whisper-metal")
        .arg(model_path)
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(test)]
mod preparation_tests {
    use super::*;
    fn fixture() -> (tempfile::TempDir, Arc<Runtime>) {
        let dir = tempfile::tempdir().unwrap();
        let rt = Runtime::open_with_vault(
            dir.path().into(),
            Arc::new(crate::models::tests::MemoryVault::default()),
        )
        .unwrap();
        rt.config.lock().unwrap().asr.model_path.clear();
        (dir, rt)
    }
    #[test]
    fn missing_or_invalid_model_never_creates_a_meeting_or_starts_capture() {
        let (_dir, rt) = fixture();
        assert!(prepare(&rt).unwrap_err().contains("选择"));
        let mut worker = spawn(rt.clone());
        rt.config.lock().unwrap().asr.model_path = "/nonexistent/helmmeet-test.bin".into();
        rt.config.lock().unwrap().asr.model_sha256 = Some("invalid".into());
        assert!(prepare(&rt).unwrap_err().contains("不可读"));
        assert!(rt.capture.lock().unwrap().is_none());
        assert_eq!(
            rt.db()
                .unwrap()
                .query_row("SELECT count(*) FROM meetings", [], |r| r
                    .get::<_, usize>(0))
                .unwrap(),
            0
        );
        worker.join().unwrap();
    }
    #[test]
    #[ignore = "explicit HELMMEET_TEST_MODEL; local model preparation only, no audio hardware or HTTP"]
    fn local_model_preparation_reuses_worker_context_without_capture() {
        let (_dir, rt) = fixture();
        rt.config.lock().unwrap().asr.model_path = std::env::var("HELMMEET_TEST_MODEL").unwrap();
        rt.config.lock().unwrap().asr.model_sha256 = None;
        let mut worker = spawn(rt.clone());
        assert_eq!(prepare(&rt).unwrap(), json!({"ready":true,"cached":false}));
        assert_eq!(prepare(&rt).unwrap(), json!({"ready":true,"cached":true}));
        assert!(rt.capture.lock().unwrap().is_none());
        assert_eq!(
            rt.db()
                .unwrap()
                .query_row("SELECT count(*) FROM audio_chunks", [], |r| r
                    .get::<_, usize>(0))
                .unwrap(),
            0
        );
        worker.join().unwrap();
    }
}

pub fn recover(rt: &Runtime) -> Result<(), String> {
    let rows = {
        let db = rt.db()?;
        let mut s=db.prepare("SELECT id,meeting_id,relative_path,start_ms FROM audio_chunks WHERE status='writing'").map_err(err)?;
        let v = s
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, u64>(3)?,
                ))
            })
            .map_err(err)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(err)?;
        v
    };
    for (chunk, meeting, path, start) in rows {
        match safe_relative(&rt.root, &path).and_then(|p| audio::repair(&p)) {
            Ok(samples) if samples > 0 => {
                rt.db()?.execute("UPDATE audio_chunks SET status='sealed',sample_count=?,end_ms=? WHERE id=?",params![samples,start+samples/16,chunk]).map_err(err)?;
                rt.db()?.execute("UPDATE meetings SET recording_duration_ms=max(recording_duration_ms,?) WHERE id=?",params![start+samples/16,meeting]).map_err(err)?;
            }
            _ => {
                rt.db()?
                    .execute(
                        "UPDATE audio_chunks SET status='missing' WHERE id=?",
                        [chunk],
                    )
                    .map_err(err)?;
                rt.error(&meeting, "部分音频文件无法修复");
            }
        }
    }
    Ok(())
}
fn work(rt: &Runtime) -> Result<Option<(String, u64, u64, bool)>, String> {
    let db = rt.db()?;
    let mut s=db.prepare("SELECT id,recording_duration_ms,state FROM meetings WHERE state IN ('recording','paused','interrupted','finalizing','needs_attention') AND recording_duration_ms>0 ORDER BY created_at").map_err(err)?;
    let meetings = s
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, u64>(1)?,
                r.get::<_, String>(2)?,
            ))
        })
        .map_err(err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(err)?;
    for (meeting, duration, state) in meetings {
        let mut q = db
            .prepare("SELECT start_ms,end_ms FROM asr_chunks WHERE meeting_id=?")
            .map_err(err)?;
        let done = q
            .query_map([&meeting], |r| {
                Ok((r.get::<_, u64>(0)?, r.get::<_, u64>(1)?))
            })
            .map_err(err)?
            .filter_map(Result::ok)
            .filter_map(|(s, e)| Interval::new(s, e))
            .collect::<Vec<_>>();
        let gaps = missing_intervals(Interval::new(0, duration).unwrap(), &done);
        if let Some(gap) = gaps.first() {
            let start = gap.start_ms();
            let end = (start + 8000).min(gap.end_ms());
            let sealed:u64=db.query_row("SELECT max(coalesce((SELECT max(end_ms) FROM audio_chunks WHERE meeting_id=?1 AND status='sealed'),0),coalesce((SELECT max(end_ms) FROM coverage_intervals WHERE meeting_id=?1 AND kind LIKE 'capture_%' AND state='missing'),0))",[&meeting],|r|r.get(0)).map_err(err)?;
            if end <= sealed && (end - start >= 4000 || state != "recording") {
                return Ok(Some((meeting, start, end, state == "finalizing")));
            }
        } else if state == "finalizing" {
            db.execute(
                "UPDATE meetings SET state='completed',version=version+1 WHERE id=?",
                [&meeting],
            )
            .map_err(err)?;
        }
    }
    Ok(None)
}
pub fn mix(rt: &Runtime, meeting: &str, start: u64, end: u64) -> Result<Vec<f32>, String> {
    mix_source(rt, meeting, start, end, None)
}
pub fn mix_source(
    rt: &Runtime,
    meeting: &str,
    start: u64,
    end: u64,
    source: Option<&str>,
) -> Result<Vec<f32>, String> {
    let rows = {
        let db = rt.db()?;
        let mut s=db.prepare("SELECT c.relative_path,coalesce(t.start_sample,c.start_ms*16),CASE WHEN t.chunk_id IS NULL THEN (c.end_ms-c.start_ms)*16 ELSE c.sample_count END FROM audio_chunks c LEFT JOIN audio_chunk_timing t ON t.chunk_id=c.id WHERE c.meeting_id=? AND c.status='sealed' AND coalesce(t.start_sample,c.start_ms*16)<?*16 AND coalesce(t.start_sample,c.start_ms*16)+CASE WHEN t.chunk_id IS NULL THEN (c.end_ms-c.start_ms)*16 ELSE c.sample_count END>?*16 AND (? IS NULL OR c.source=?) ORDER BY c.start_ms").map_err(err)?;
        let rows = s
            .query_map(params![meeting, end, start, source, source], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, u64>(1)?,
                    r.get::<_, u64>(2)?,
                ))
            })
            .map_err(err)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(err)?;
        rows
    };
    // A missing packet is silence for that source, not an instruction to boost
    // every other source. Use one gain for the whole meeting, including export
    // blocks, so availability/chunk boundaries cannot produce level jumps.
    let tracks: usize = if source.is_some() {
        1
    } else {
        rt.db()?.query_row("SELECT count(*) FROM (SELECT source FROM audio_chunks WHERE meeting_id=?1 UNION SELECT source FROM recording_sources WHERE meeting_id=?1)",[meeting],|r|r.get(0)).map_err(err)?
    };
    let gain = meeting_core::audio_clock::mix_gain(tracks);
    let mut pcm = vec![0.0; ((end - start) * 16) as usize];
    for (path, from, count) in rows {
        let samples = audio::read_wav(&safe_relative(&rt.root, &path)?)?;
        let lo = (start * 16).max(from);
        let hi = (end * 16).min(from + count);
        for t in lo..hi {
            let out = (t - start * 16) as usize;
            let input = (t - from) as usize;
            if let Some(v) = samples.get(input) {
                pcm[out] += *v * gain;
            }
        }
    }
    Ok(pcm)
}
pub fn decode(
    context: &WhisperContext,
    pcm: &[f32],
    language: &str,
) -> Result<Vec<(u64, u64, String)>, String> {
    decode_with_partial(context, pcm, language, None, None, "", None)
}

fn whisper_language(language: &str) -> Option<&'static str> {
    match language {
        "auto" => None,
        // Whisper uses one Chinese language token; the selected script is
        // applied to its UTF-8 output below.
        "zh" | "zh-Hans" | "zh-Hant" => Some("zh"),
        _ => Some("en"),
    }
}

#[cfg(target_os = "macos")]
fn convert_script(text: &str, language: &str) -> String {
    use std::{ffi::c_void, ptr};
    const UTF8: u32 = 0x0800_0100;
    #[allow(non_camel_case_types)]
    type CFIndex = isize;
    #[allow(non_camel_case_types)]
    type CFStringRef = *const c_void;
    #[allow(non_camel_case_types)]
    type CFMutableStringRef = *mut c_void;
    #[repr(C)]
    struct CFRange {
        location: CFIndex,
        length: CFIndex,
    }
    #[allow(non_snake_case)]
    extern "C" {
        fn CFStringCreateWithBytes(
            alloc: *const c_void,
            bytes: *const u8,
            num_bytes: CFIndex,
            encoding: u32,
            is_external_representation: u8,
        ) -> CFStringRef;
        fn CFStringCreateMutableCopy(
            alloc: *const c_void,
            max_length: CFIndex,
            string: CFStringRef,
        ) -> CFMutableStringRef;
        fn CFStringGetLength(string: CFStringRef) -> CFIndex;
        fn CFStringGetMaximumSizeForEncoding(length: CFIndex, encoding: u32) -> CFIndex;
        fn CFStringGetCString(
            string: CFStringRef,
            buffer: *mut i8,
            buffer_size: CFIndex,
            encoding: u32,
        ) -> u8;
        fn CFStringTransform(
            string: CFMutableStringRef,
            range: *mut CFRange,
            transform: CFStringRef,
            reverse: u8,
        ) -> u8;
        fn CFRelease(value: CFStringRef);
    }
    let transform_name = match language {
        "zh-Hans" => "Traditional-Simplified",
        "zh-Hant" | "zh" => "Simplified-Traditional",
        _ => return text.to_owned(),
    };
    let bytes = text.as_bytes();
    let input = unsafe {
        CFStringCreateWithBytes(ptr::null(), bytes.as_ptr(), bytes.len() as CFIndex, UTF8, 0)
    };
    if input.is_null() {
        return text.to_owned();
    }
    let transform_bytes = transform_name.as_bytes();
    let transform = unsafe {
        CFStringCreateWithBytes(
            ptr::null(),
            transform_bytes.as_ptr(),
            transform_bytes.len() as CFIndex,
            UTF8,
            0,
        )
    };
    if transform.is_null() {
        unsafe { CFRelease(input) };
        return text.to_owned();
    }
    let mutable = unsafe { CFStringCreateMutableCopy(ptr::null(), 0, input) };
    if mutable.is_null() {
        unsafe {
            CFRelease(transform);
            CFRelease(input);
        }
        return text.to_owned();
    }
    let mut range = CFRange {
        location: 0,
        length: unsafe { CFStringGetLength(mutable) },
    };
    let transformed = unsafe { CFStringTransform(mutable, &mut range, transform, 0) } != 0;
    let max = unsafe { CFStringGetMaximumSizeForEncoding(range.length, UTF8) };
    let mut output = vec![0i8; max.max(1) as usize + 1];
    let ok = transformed
        && unsafe {
            CFStringGetCString(mutable, output.as_mut_ptr(), output.len() as CFIndex, UTF8) != 0
        };
    let result = if ok {
        let bytes =
            unsafe { std::slice::from_raw_parts(output.as_ptr().cast::<u8>(), output.len()) };
        let end = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
        String::from_utf8(bytes[..end].to_vec()).unwrap_or_else(|_| text.to_owned())
    } else {
        text.to_owned()
    };
    unsafe {
        CFRelease(transform);
        CFRelease(mutable);
        CFRelease(input);
    }
    result
}

#[cfg(not(target_os = "macos"))]
fn convert_script(text: &str, _language: &str) -> String {
    text.to_owned()
}

fn decode_with_partial(
    context: &WhisperContext,
    pcm: &[f32],
    language: &str,
    partial: Option<Box<dyn FnMut(whisper_rs::SegmentCallbackData)>>,
    stop: Option<StopToken>,
    initial_prompt: &str,
    question: Option<(Arc<Runtime>, u64)>,
) -> Result<Vec<(u64, u64, String)>, String> {
    if stop.as_ref().is_some_and(StopToken::is_stopped) {
        return Err("转写已取消，原音频保留待下次处理".into());
    }
    let energy = pcm.iter().map(|v| v * v).sum::<f32>() / pcm.len().max(1) as f32;
    if energy < 0.000004 {
        return Ok(vec![]);
    }
    let mut state = context.create_state().map_err(|_| "无法创建推理状态")?;
    let mut p = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    struct Abort {
        stop: Option<StopToken>,
        question: Option<(Arc<Runtime>, u64)>,
    }
    impl Abort {
        fn cancelled(&self) -> bool {
            self.stop.as_ref().is_some_and(StopToken::is_stopped)
                || self
                    .question
                    .as_ref()
                    .is_some_and(|(rt, epoch)| !crate::voice::request_is_current(rt, *epoch))
        }
    }
    let abort = Abort { stop, question };
    unsafe extern "C" fn should_abort(data: *mut std::ffi::c_void) -> bool {
        // `abort` is immovable and alive until synchronous state.full returns.
        unsafe { (&*data.cast::<Abort>()).cancelled() }
    }
    unsafe {
        p.set_abort_callback(Some(should_abort));
        p.set_abort_callback_user_data((&abort as *const Abort).cast_mut().cast());
    }
    p.set_segment_callback_safe_lossy::<_, Box<dyn FnMut(whisper_rs::SegmentCallbackData)>>(
        partial,
    );
    p.set_language(whisper_language(language));
    p.set_n_threads(4);
    p.set_print_realtime(false);
    p.set_print_progress(false);
    p.set_print_timestamps(false);
    p.set_print_special(false);
    p.set_no_context(true);
    if !initial_prompt.is_empty() {
        p.set_initial_prompt(initial_prompt);
    }
    p.set_suppress_blank(true);
    p.set_temperature(0.0);
    state.full(p, pcm).map_err(|_| "本地识别失败")?;
    let mut output = vec![];
    for seg in state.as_iter() {
        let text = seg
            .to_str_lossy()
            .map_err(|_| "识别文本编码异常")?
            .trim()
            .to_string();
        let text = convert_script(&text, language);
        if !text.is_empty() {
            output.push((
                (seg.start_timestamp().max(0) as u64) * 10,
                (seg.end_timestamp().max(0) as u64) * 10,
                text,
            ));
        }
    }
    Ok(output)
}
pub fn spawn(rt: Arc<Runtime>) -> Worker {
    Worker::spawn(move |stop| {
        let mut loaded = String::new();
        let mut context = None;
        let mut backend = None;
        while stop.wait(Duration::from_millis(100)) {
            let preparation = rt
                .asr_preparation
                .pending
                .lock()
                .ok()
                .and_then(|mut q| q.pop_front());
            if let Some(reply) = preparation {
                let result = rt
                    .config
                    .lock()
                    .map(|c| c.asr.clone())
                    .map_err(|_| "配置锁异常".to_string())
                    .and_then(|config| {
                        load_context(&config, &stop, &mut loaded, &mut context, &mut backend)
                    });
                let _ = reply.send(result);
                continue;
            }
            let voice_question = crate::voice::take_question(&rt);
            if voice_question
                .as_ref()
                .is_some_and(|q| !crate::voice::valid_question(&rt, q))
            {
                continue;
            }
            let job = if let Some(q) = &voice_question {
                (q.meeting.clone(), 0, 0, false)
            } else {
                match work(&rt) {
                    Ok(Some(v)) => v,
                    _ => continue,
                }
            };
            let (meeting, start, end, _) = job;
            if stop.is_stopped() {
                break;
            }
            let config = match rt.config.lock() {
                Ok(c) => c.asr.clone(),
                Err(_) => continue,
            };
            if config.model_path.is_empty() {
                if let Some(q) = &voice_question {
                    crate::voice::question_failed(&rt, q, "请先配置本地转写模型");
                    continue;
                }
                rt.error(&meeting, "未选择转写模型，录音保留，可配置后重试");
                if let Ok(db) = rt.db() {
                    let _=db.execute("UPDATE meetings SET state='needs_attention' WHERE id=? AND state='finalizing'",[&meeting]);
                }
                continue;
            }
            let key = format!(
                "{}:{}",
                config.model_path,
                config.model_sha256.as_deref().unwrap_or("")
            );
            if loaded != key {
                if let Ok(mut t) = rt.transient.lock() {
                    t[&meeting]["asr_state"] = json!("loading");
                }
                if let Err(e) =
                    load_context(&config, &stop, &mut loaded, &mut context, &mut backend)
                {
                    if stop.is_stopped() {
                        break;
                    }
                    if let Some(q) = &voice_question {
                        crate::voice::question_failed(&rt, q, &e);
                    } else {
                        rt.error(&meeting, &e);
                        stop.wait(Duration::from_secs(5));
                    }
                    continue;
                }
            }
            if stop.is_stopped() {
                break;
            }
            if let Some(question) = voice_question {
                if !crate::voice::valid_question(&rt, &question) {
                    continue;
                }
                let lexicon =
                    crate::knowledge::asr_context(&rt, &meeting).unwrap_or(json!({"prompt":""}));
                let voice_prompt = format!(
                    "{} {}",
                    lexicon["prompt"].as_str().unwrap_or(""),
                    question.wake
                );
                let result = decode_with_partial(
                    context.as_ref().unwrap(),
                    &question.pcm,
                    &config.language,
                    None,
                    Some(stop.clone()),
                    &voice_prompt,
                    Some((rt.clone(), question.request_epoch)),
                );
                match result {
                    Ok(parts) if !stop.is_stopped() => crate::voice::recognized(
                        rt.clone(),
                        question,
                        parts
                            .into_iter()
                            .map(|(_, _, t)| t)
                            .collect::<Vec<_>>()
                            .join(""),
                    ),
                    Err(_) => crate::voice::question_failed(
                        &rt,
                        &question,
                        "问题识别失败，请重新提问或输入文字",
                    ),
                    _ => (),
                }
                continue;
            }
            let began = std::time::Instant::now();
            let lo = start.saturating_sub(750);
            if let Ok(mut t) = rt.transient.lock() {
                t[&meeting]["asr_state"] = json!("transcribing");
            }
            let partial_rt = rt.clone();
            let partial_mid = meeting.clone();
            let partial_language = config.language.clone();
            let callback = Box::new(move |segment: whisper_rs::SegmentCallbackData| {
                if let Ok(mut t) = partial_rt.transient.lock() {
                    t[&partial_mid]["partial"] =
                        json!(convert_script(&segment.text, &partial_language));
                }
            });
            let lexicon = match crate::knowledge::asr_context(&rt, &meeting) {
                Ok(v) => v,
                Err(_) => {
                    rt.error(&meeting, "本场词库暂不可用，本轮使用原始转写");
                    json!({"prompt":"","bindings":[],"unavailable":true})
                }
            };
            let result = mix(&rt, &meeting, lo, end).and_then(|pcm| {
                decode_with_partial(
                    context.as_ref().unwrap(),
                    &pcm,
                    &config.language,
                    Some(callback),
                    Some(stop.clone()),
                    lexicon["prompt"].as_str().unwrap_or(""),
                    None,
                )
            });
            // Cancellation cannot acknowledge coverage or commit partial text.
            if stop.is_stopped() {
                break;
            }
            match result {
                Ok(segments) => {
                    let result = (|| -> Result<(), String> {
                        let mut db = rt.db()?;
                        let tx = db.transaction().map_err(err)?;
                        let state: String = tx
                            .query_row("SELECT state FROM meetings WHERE id=?", [&meeting], |r| {
                                r.get(0)
                            })
                            .map_err(err)?;
                        if state == "deleting" {
                            return Ok(());
                        }
                        for (a, b, text) in segments {
                            let a = (lo + a).max(start);
                            let b = (lo + b).min(end);
                            if b <= a {
                                continue;
                            }
                            let segment = id();
                            tx.execute("INSERT INTO transcript_segments VALUES (?,?,?,?,'mixed',1,'final')",params![segment,meeting,a,b]).map_err(err)?;
                            tx.execute(
                                "INSERT INTO asr_lexicon_snapshots VALUES (?,?,?)",
                                params![segment, meeting, lexicon.to_string()],
                            )
                            .map_err(err)?;
                            tx.execute(
                                "INSERT INTO transcript_revisions VALUES (?,1,?,?,'asr',?)",
                                params![segment, meeting, text, now()],
                            )
                            .map_err(err)?;
                        }
                        tx.execute(
                            "INSERT OR IGNORE INTO asr_chunks VALUES (?,?,?)",
                            params![meeting, start, end],
                        )
                        .map_err(err)?;
                        tx.execute("INSERT INTO coverage_intervals VALUES (?,?,'asr',?,?,'covered',NULL,NULL)",params![id(),meeting,start,end]).map_err(err)?;
                        tx.commit().map_err(err)?;
                        Ok(())
                    })();
                    if let Err(e) = result {
                        rt.error(&meeting, &e);
                    }
                    if let Ok(mut t) = rt.transient.lock() {
                        t[&meeting]["asr_state"] = json!("ready");
                        t[&meeting]["partial"] = json!("");
                        t[&meeting]["last_inference_ms"] =
                            json!(began.elapsed().as_millis() as u64);
                        t[&meeting]["asr_through_ms"] = json!(end);
                    }
                }
                Err(e) => {
                    rt.error(&meeting, &e);
                    stop.wait(Duration::from_secs(3));
                }
            }
        }
        // Drop cached GPU buffers on this worker before its owner can join and
        // permit AppKit/libc's global Metal device destructors to run.
        drop(context);
    })
}

#[cfg(test)]
mod mix_regression_tests {
    use super::*;
    #[test]
    fn missing_system_packets_do_not_double_microphone_gain() {
        let root = std::env::temp_dir().join(format!("helmmeet-mix-{}", id()));
        let rt = Runtime::open_with_vault(
            root.clone(),
            Arc::new(crate::models::tests::MemoryVault::default()),
        )
        .unwrap();
        let m = rt.create("合成混音测试", "general").unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        std::fs::create_dir_all(rt.meeting_dir(&m).join("audio")).unwrap();
        let microphone = (0..48000)
            .map(|i| {
                0.6 * (std::f32::consts::TAU * 440.0 * i as f32 / 16000.0
                    + std::f32::consts::FRAC_PI_2)
                    .sin()
            })
            .collect::<Vec<_>>();
        for (name, source, from, to, pcm) in [
            ("mic", "microphone", 0, 3000, microphone.clone()),
            ("system-a", "system", 0, 1000, vec![0.0; 16000]),
            ("system-b", "system", 2000, 3000, vec![0.0; 16000]),
        ] {
            let relative = format!("meetings/{m}/audio/{name}.wav");
            let mut wav = audio::WavFile::create(&rt.root.join(&relative)).unwrap();
            wav.write(&pcm).unwrap();
            wav.sync().unwrap();
            rt.db()
                .unwrap()
                .execute(
                    "INSERT INTO audio_chunks VALUES (?,?,?,?,?,?,?,'sealed')",
                    params![id(), m, source, relative, from, to, pcm.len()],
                )
                .unwrap();
        }
        let mixed = mix(&rt, &m, 0, 3000).unwrap();
        assert!(
            (mixed[16000] - microphone[16000] * 0.5).abs() < 0.0001,
            "missing packets changed the other source's gain: {}",
            mixed[16000]
        );
        assert!(
            (mixed[16000] - mixed[15999]).abs() < 0.02,
            "artificial click at packet availability boundary"
        );
        let track = mix_source(&rt, &m, 0, 3000, Some("microphone")).unwrap();
        assert!((track[16000] - microphone[16000]).abs() < 0.0001);
        drop(rt);
        std::fs::remove_dir_all(root).unwrap();
    }
}
