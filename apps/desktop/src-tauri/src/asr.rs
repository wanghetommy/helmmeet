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
use std::{sync::Arc, time::Duration};
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

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
    decode_with_partial(context, pcm, language, None, None, "")
}
fn decode_with_partial(
    context: &WhisperContext,
    pcm: &[f32],
    language: &str,
    partial: Option<Box<dyn FnMut(whisper_rs::SegmentCallbackData)>>,
    stop: Option<StopToken>,
    initial_prompt: &str,
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
    if let Some(token) = stop.as_ref() {
        unsafe extern "C" fn abort(data: *mut std::ffi::c_void) -> bool {
            // The borrowed token stays alive and unmoved through state.full().
            // Native inference invokes this only synchronously within full().
            unsafe { (&*data.cast::<StopToken>()).is_stopped() }
        }
        unsafe {
            p.set_abort_callback(Some(abort));
            p.set_abort_callback_user_data(
                (token as *const StopToken)
                    .cast_mut()
                    .cast::<std::ffi::c_void>(),
            );
        }
    }
    p.set_segment_callback_safe_lossy::<_, Box<dyn FnMut(whisper_rs::SegmentCallbackData)>>(
        partial,
    );
    p.set_language(if language == "auto" {
        None
    } else {
        Some(language)
    });
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
        'jobs: while stop.wait(Duration::from_millis(700)) {
            let job = match work(&rt) {
                Ok(Some(v)) => v,
                _ => continue,
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
                rt.error(&meeting, "未选择转写模型，录音保留，可配置后重试");
                if let Ok(db) = rt.db() {
                    let _=db.execute("UPDATE meetings SET state='needs_attention' WHERE id=? AND state='finalizing'",[&meeting]);
                }
                continue;
            }
            if loaded != config.model_path {
                if let Ok(mut t) = rt.transient.lock() {
                    t[&meeting]["asr_state"] = json!("loading");
                }
                if let Some(expected) = &config.model_sha256 {
                    let check = (|| -> Result<String, String> {
                        let mut f = std::fs::File::open(&config.model_path).map_err(err)?;
                        let mut hash = Sha256::new();
                        let mut bytes = [0u8; 1024 * 1024];
                        loop {
                            if stop.is_stopped() {
                                return Err("转写已取消".into());
                            }
                            let n = f.read(&mut bytes).map_err(err)?;
                            if n == 0 {
                                break;
                            }
                            hash.update(&bytes[..n]);
                        }
                        Ok(format!("{:x}", hash.finalize()))
                    })();
                    if check.as_deref() != Ok(expected.as_str()) {
                        if stop.is_stopped() {
                            break 'jobs;
                        }
                        rt.error(&meeting, "模型 SHA-256 不匹配或文件不可读");
                        stop.wait(Duration::from_secs(5));
                        continue;
                    }
                }
                let mut params = WhisperContextParameters::default();
                params.use_gpu(true);
                match WhisperContext::new_with_params(&config.model_path, params) {
                    Ok(c) => {
                        context = Some(c);
                        loaded = config.model_path.clone();
                    }
                    Err(_) => {
                        if stop.is_stopped() {
                            break;
                        }
                        rt.error(&meeting, "模型加载失败，请检查模型文件");
                        stop.wait(Duration::from_secs(5));
                        continue;
                    }
                }
            }
            if stop.is_stopped() {
                break;
            }
            let began = std::time::Instant::now();
            let lo = start.saturating_sub(750);
            if let Ok(mut t) = rt.transient.lock() {
                t[&meeting]["asr_state"] = json!("transcribing");
            }
            let partial_rt = rt.clone();
            let partial_mid = meeting.clone();
            let callback = Box::new(move |segment: whisper_rs::SegmentCallbackData| {
                if let Ok(mut t) = partial_rt.transient.lock() {
                    t[&partial_mid]["partial"] = json!(segment.text);
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
