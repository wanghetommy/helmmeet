use crate::db::{err, Runtime};
use meeting_core::summary::{Revisions, Window};
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{collections::HashMap, ffi::CString, sync::Arc, time::Instant};

pub fn seed_templates(db: &Connection) -> Result<(), String> {
    for (id,name,instructions) in [
        ("weekly","周会","按进展、目标偏差、阻塞、下期安排整理。核对依赖与行动，不把进度汇报当成承诺。"),
        ("review","方案评审","按目标与约束、方案与替代项、权衡依据、风险、验证、通过条件整理。未验证假设和待确认结论单列。"),
        ("decision","决策会议","按待决事项、选项、判断标准、支持与反对依据、已决/未决、执行条件整理。未明确共识不能写成决定。"),
        ("retro","复盘","按预期与实际、事实时间线、原因与假设、经验、改进实验整理。区分证据与归因猜测。"),
        ("brainstorm","头脑风暴","按问题边界、想法分组、探索方向、待验证假设、后续实验整理。想法不代表采纳。"),
    ] {
        db.execute("INSERT OR IGNORE INTO assistant_templates VALUES (?,1,?,?)", params![id,name,instructions]).map_err(err)?;
    }
    Ok(())
}

pub fn transcript(
    rt: &Runtime,
    meeting: &str,
    source: &str,
    timestamps: bool,
) -> Result<Value, String> {
    rt.check_id(meeting)?;
    if !matches!(source, "current" | "original") {
        return Err("原稿来源无效".into());
    }
    let db = rt.db()?;
    let (title, duration): (String, u64) = db
        .query_row(
            "SELECT title,recording_duration_ms FROM meetings WHERE id=?",
            [meeting],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(err)?;
    let sql = if source == "original" {
        "SELECT s.id,s.start_ms,r.revision,r.text,s.end_ms FROM transcript_segments s JOIN transcript_revisions r ON r.segment_id=s.id AND r.revision=(SELECT min(revision) FROM transcript_revisions WHERE segment_id=s.id AND origin='asr') WHERE s.meeting_id=? ORDER BY s.start_ms,s.id"
    } else {
        "SELECT s.id,s.start_ms,r.revision,r.text,s.end_ms FROM transcript_segments s JOIN transcript_revisions r ON r.segment_id=s.id AND r.revision=s.current_revision WHERE s.meeting_id=? ORDER BY s.start_ms,s.id"
    };
    let mut stmt = db.prepare(sql).map_err(err)?;
    let rows=stmt.query_map([meeting],|r|Ok(json!({"segment_id":r.get::<_,String>(0)?,"start_ms":r.get::<_,u64>(1)?,"revision":r.get::<_,u64>(2)?,"text":r.get::<_,String>(3)?,"end_ms":r.get::<_,u64>(4)?}))).map_err(err)?.collect::<Result<Vec<_>,_>>().map_err(err)?;
    let active: bool = db
        .query_row(
            "SELECT state IN('recording','finalizing') FROM meetings WHERE id=?",
            [meeting],
            |r| r.get(0),
        )
        .map_err(err)?;
    let paragraphs = crate::paragraphs::project(&db, meeting, &rows, active)?;
    let paragraphs = if source == "current" {
        crate::calibration::decorate(&db, meeting, paragraphs)?
    } else {
        paragraphs
    };
    let paragraphs = crate::paragraphs::readable(paragraphs);
    let text = format_transcript(&paragraphs, timestamps);
    let hash = format!(
        "{:x}",
        Sha256::digest(
            serde_json::to_vec(&json!([meeting, source, timestamps, rows, text])).map_err(err)?
        )
    );
    Ok(
        json!({"meeting_id":meeting,"title":title,"source":source,"timestamps":timestamps,"through_ms":duration,"segments":rows,"paragraphs":paragraphs,"text":text,"hash":hash}),
    )
}
pub fn format_transcript(rows: &[Value], timestamps: bool) -> String {
    rows.iter()
        .filter(|v| !v["text"].as_str().unwrap_or("").trim().is_empty())
        .map(|v| {
            let t = v["start_ms"].as_u64().unwrap_or(0) / 1000;
            format!(
                "{}{}",
                if timestamps {
                    format!("[{:02}:{:02}] ", t / 60, t % 60)
                } else {
                    String::new()
                },
                meeting_core::reading::blocks(v["text"].as_str().unwrap_or("")).join("\n\n")
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}
extern "C" {
    fn hm_clipboard_write(text: *const std::ffi::c_char) -> i32;
}
pub fn copy(
    rt: &Runtime,
    meeting: &str,
    source: &str,
    timestamps: bool,
    expected_hash: &str,
) -> Result<Value, String> {
    let snapshot = transcript(rt, meeting, source, timestamps)?;
    if snapshot["hash"] != expected_hash {
        return Err("原稿已更新，请再次点击复制以确认最新快照".into());
    }
    let text = snapshot["text"].as_str().ok_or("原稿无效")?;
    write_clipboard(text)?;
    Ok(snapshot)
}
pub fn write_clipboard(text: &str) -> Result<(), String> {
    if text.trim().is_empty() {
        return Err("没有可复制的文字".into());
    }
    let text = CString::new(text).map_err(|_| "文字含无效字符")?;
    // Swift consumes the UTF-8 string synchronously and only writes the pasteboard.
    if unsafe { hm_clipboard_write(text.as_ptr()) } != 0 {
        return Err("剪贴板写入失败，请重试或导出 TXT".into());
    }
    Ok(())
}
pub fn pending(rt: &Runtime, mid: &str) -> Result<Revisions, String> {
    let db = rt.db()?;
    let mut q=db.prepare("SELECT s.id,s.current_revision,r.text,c.revision FROM transcript_segments s JOIN transcript_revisions r ON r.segment_id=s.id AND r.revision=s.current_revision LEFT JOIN summary_coverage c ON c.meeting_id=s.meeting_id AND c.segment_id=s.id WHERE s.meeting_id=? AND s.state IN('final','edited') AND (c.revision IS NULL OR c.revision!=s.current_revision)").map_err(err)?;
    let rows = q
        .query_map([mid], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, u64>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<u64>>(3)?,
            ))
        })
        .map_err(err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(err)?;
    Ok(rows
        .into_iter()
        .filter_map(|(id, rev, text, old)| {
            let n = text.chars().filter(|c| c.is_alphanumeric()).count();
            (n > 0 || old.is_some()).then_some((id, (rev, if old.is_some() { 0 } else { n })))
        })
        .collect())
}
pub fn set_schedule(
    rt: &Runtime,
    mid: &str,
    version: u64,
    enabled: bool,
    mode: &str,
) -> Result<Value, String> {
    rt.check_id(mid)?;
    if enabled && crate::product::globally_paused(rt)? {
        return Err("全局分析已暂停，请先在设置中恢复".into());
    }
    let mut db = rt.db()?;
    let tx = db.transaction().map_err(err)?;
    let current_mode: String = tx
        .query_row(
            "SELECT mode FROM summary_schedules WHERE meeting_id=?",
            [mid],
            |r| r.get(0),
        )
        .map_err(err)?;
    if !matches!(mode, "realtime" | "three_minutes" | "economy")
        && !(matches!(mode, "legacy_5" | "legacy_10") && current_mode == mode)
    {
        return Err("请选择实时、3 分钟或节省调用".into());
    }
    if enabled {
        let permitted:bool=tx.query_row("SELECT analysis_mode!='transcript_only' AND context_consent=1 FROM meetings WHERE id=?",[mid],|r|r.get(0)).map_err(err)?;
        if !permitted {
            return Err("请先在本场分析设置中授权总结上下文范围".into());
        }
    }
    if tx.execute("UPDATE summary_schedules SET enabled=?,mode=?,version=version+1,failures=0,retry_after_utc=0,paused_reason=NULL WHERE meeting_id=? AND version=?",params![enabled,mode,mid,version]).map_err(err)?!=1 {return Err("总结设置已变化，请刷新重试".into());}
    tx.execute("UPDATE analysis_jobs SET state='cancelled' WHERE meeting_id=? AND state IN('pending','running') AND id IN(SELECT job_id FROM analysis_origins WHERE automatic=1)",[mid]).map_err(err)?;
    tx.commit().map_err(err)?;
    Ok(json!({"saved":true}))
}
struct Clock {
    version: u64,
    elapsed: u64,
    boundary_ms: u64,
    completion_seq: u64,
    window: Window,
    job: Option<String>,
}
pub fn background(rt: Arc<Runtime>) -> crate::lifecycle::Worker {
    crate::lifecycle::Worker::spawn(move |stop| {
        let mut clocks: HashMap<String, Clock> = HashMap::new();
        let mut ticks = 0u64;
        let mut last = Instant::now();
        while stop.wait(std::time::Duration::from_millis(250)) {
            let delta = last.elapsed().as_millis() as u64;
            last = Instant::now();
            if crate::product::globally_paused(&rt).unwrap_or(true) {
                for clock in clocks.values_mut() {
                    clock.window.clear_boundary();
                }
                continue;
            }
            ticks += 1;
            if ticks.is_multiple_of(40) {
                let _ = crate::commands::cleanup(&rt);
            }
            let meetings = (|| -> Result<Value, String> {
                let db = rt.db()?;
                let mut stmt=db.prepare("SELECT m.id FROM meetings m JOIN summary_schedules s ON s.meeting_id=m.id WHERE m.state IN('recording','paused','finalizing') OR (s.tail_done=0 AND m.state IN('completed','needs_attention'))").map_err(err)?;
                let rows = stmt
                    .query_map([], |r| Ok(json!({"id":r.get::<_,String>(0)?})))
                    .map_err(err)?
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(err)?;
                Ok(json!(rows))
            })();
            let Ok(meetings) = meetings else { continue };
            for m in meetings.as_array().into_iter().flatten() {
                let Some(mid) = m["id"].as_str() else {
                    continue;
                };
                let d = (|| -> Result<Value, String> {
                    let db = rt.db()?;
                    let mut v=db.query_row("SELECT m.state,m.analysis_mode,m.context_consent,s.enabled,s.mode,s.version,s.paused_reason,s.completed_asr_ms,s.failures,(SELECT coalesce(max(a.rowid),0) FROM artifacts a JOIN meeting_memories mm ON mm.artifact_id=a.id WHERE a.meeting_id=m.id) FROM meetings m JOIN summary_schedules s ON s.meeting_id=m.id WHERE m.id=?",[mid],|r|Ok(json!({"state":r.get::<_,String>(0)?,"analysis_mode":r.get::<_,String>(1)?,"context_consent":r.get::<_,bool>(2)?,"schedule":{"enabled":r.get::<_,bool>(3)?,"mode":r.get::<_,String>(4)?,"version":r.get::<_,u64>(5)?,"paused_reason":r.get::<_,Option<String>>(6)?,"completed_asr_ms":r.get::<_,u64>(7)?,"failures":r.get::<_,u64>(8)?,"completion_seq":r.get::<_,u64>(9)?}}))).map_err(err)?;
                    let mut stmt=db.prepare("SELECT id,state FROM analysis_jobs WHERE meeting_id=? AND type='stage' AND state IN('pending','running')").map_err(err)?;
                    v["jobs"]=json!(stmt.query_map([mid],|r|Ok(json!({"id":r.get::<_,String>(0)?,"type":"stage","state":r.get::<_,String>(1)?}))).map_err(err)?.collect::<Result<Vec<_>,_>>().map_err(err)?);
                    Ok(v)
                })();
                let Ok(d) = d else { continue };
                let schedule = &d["schedule"];
                let version = schedule["version"].as_u64().unwrap_or(0);
                let fresh = !clocks.contains_key(mid);
                let c = clocks.entry(mid.into()).or_insert_with(|| Clock {
                    version,
                    elapsed: 0,
                    boundary_ms: 0,
                    completion_seq: 0,
                    window: Window::default(),
                    job: None,
                });
                if c.version != version {
                    *c = Clock {
                        version,
                        elapsed: 0,
                        boundary_ms: 0,
                        completion_seq: 0,
                        window: Window::default(),
                        job: None,
                    };
                }
                let Ok(pending) = pending(&rt, mid) else {
                    continue;
                };
                let mode = schedule["mode"].as_str().unwrap_or("three_minutes");
                let (asr_ms,first,revised,sources) = match rt.db().and_then(|db| {
                    let asr=db.query_row("SELECT coalesce(sum(end_ms-start_ms),0) FROM asr_chunks WHERE meeting_id=?",[mid],|r|r.get::<_,u64>(0)).map_err(err)?;
                    let first=!db.query_row("SELECT EXISTS(SELECT 1 FROM artifacts WHERE meeting_id=? AND type='stage' AND status!='stale' AND json_extract(content_json,'$.manual_text') IS NULL)",[mid],|r|r.get::<_,bool>(0)).map_err(err)?;
                    let revised=db.query_row("SELECT EXISTS(SELECT 1 FROM summary_coverage c JOIN transcript_segments s ON s.id=c.segment_id AND s.meeting_id=c.meeting_id WHERE c.meeting_id=? AND c.revision!=s.current_revision)",[mid],|r|r.get::<_,bool>(0)).map_err(err)?;
                    let mut q=db.prepare("SELECT CASE source WHEN 'microphone' THEN 0 ELSE 1 END,state FROM recording_sources WHERE meeting_id=? AND state!='disabled'").map_err(err)?;
                    let sources=q.query_map([mid],|r|Ok((r.get::<_,i32>(0)?,r.get::<_,String>(1)?))).map_err(err)?.collect::<Result<Vec<_>,_>>().map_err(err)?;
                    Ok((asr,first,revised,sources))
                }) { Ok(v)=>v,Err(_)=>continue };
                let seq = schedule["completion_seq"].as_u64().unwrap_or(0);
                if c.completion_seq != seq {
                    c.completion_seq = seq;
                    c.window.completed(
                        schedule["completed_asr_ms"].as_u64().unwrap_or(asr_ms),
                        &pending,
                        false,
                        0,
                    );
                    c.job = None;
                }
                c.elapsed = asr_ms;
                if d["state"] == "recording" {
                    c.boundary_ms += delta;
                } else {
                    c.window.clear_boundary();
                }
                let seconds = match mode {
                    "economy" => 600,
                    "three_minutes" => 180,
                    "legacy_5" => 300,
                    "legacy_10" => 600,
                    _ => 30,
                };
                if let Some(job) = &c.job {
                    let status = rt.db().and_then(|db| {
                        db.query_row("SELECT state FROM analysis_jobs WHERE id=?", [job], |r| {
                            r.get::<_, String>(0)
                        })
                        .map_err(err)
                    });
                    if let Ok(state) = status {
                        if !matches!(state.as_str(), "pending" | "running") {
                            c.window
                                .completed(c.elapsed, &pending, state == "failed", 0);
                            c.job = None;
                        }
                    } else {
                        c.window.completed(c.elapsed, &pending, true, 0);
                        c.job = None;
                    }
                }
                if fresh {
                    c.window.completed(
                        schedule["completed_asr_ms"].as_u64().unwrap_or(0),
                        &pending,
                        false,
                        seconds,
                    );
                }
                c.window.observe(c.elapsed, &pending);
                if d["state"] == "recording" {
                    if let Ok(db) = rt.db() {
                        let _ = db.execute(
                            "UPDATE summary_schedules SET tail_done=0 WHERE meeting_id=?",
                            [mid],
                        );
                    }
                }
                let retry_wait = rt
                    .db()
                    .ok()
                    .and_then(|db| {
                        db.query_row(
                            "SELECT retry_after_utc>? FROM summary_schedules WHERE meeting_id=?",
                            params![chrono::Utc::now().timestamp(), mid],
                            |r| r.get::<_, bool>(0),
                        )
                        .ok()
                    })
                    .unwrap_or(false);
                if (d["state"] == "completed" || d["state"] == "needs_attention")
                    && (!schedule["enabled"].as_bool().unwrap_or(false)
                        || !schedule["paused_reason"].is_null()
                        || d["context_consent"] != true
                        || (pending.is_empty() && c.job.is_none()))
                {
                    if let Ok(db) = rt.db() {
                        let _ = db.execute(
                            "UPDATE summary_schedules SET tail_done=1 WHERE meeting_id=?",
                            [mid],
                        );
                    }
                }
                let model_wait=crate::models::selected(&rt,mid).ok().is_some_and(|config|rt.db().ok().is_some_and(|db|db.query_row("SELECT EXISTS(SELECT 1 FROM summary_model_backoff WHERE model_key=? AND retry_after_utc>?)",params![crate::analysis::target(&config),chrono::Utc::now().timestamp()],|r|r.get::<_,bool>(0)).unwrap_or(false)));
                if model_wait
                    || retry_wait
                    || !schedule["enabled"].as_bool().unwrap_or(false)
                    || !schedule["paused_reason"].is_null()
                    || d["analysis_mode"] == "transcript_only"
                    || d["context_consent"] != true
                {
                    continue;
                }
                let terminal = d["state"] == "completed" || d["state"] == "needs_attention";
                let tail = terminal
                    && rt
                        .db()
                        .ok()
                        .and_then(|db| {
                            db.query_row(
                                "SELECT tail_done=0 FROM summary_schedules WHERE meeting_id=?",
                                [mid],
                                |r| r.get::<_, bool>(0),
                            )
                            .ok()
                        })
                        .unwrap_or(false);
                if c.job.is_none() {
                    let active = d["jobs"].as_array().into_iter().flatten().find(|j| {
                        j["type"] == "stage" && (j["state"] == "pending" || j["state"] == "running")
                    });
                    if let Some(j) = active {
                        c.job = j["id"].as_str().map(str::to_string);
                        c.window.dispatched();
                        continue;
                    }
                    let quiet = crate::activity::quiet(&rt, mid, &sources);
                    let catchup=rt.db().ok().and_then(|db|db.query_row("SELECT EXISTS(SELECT 1 FROM summary_fragments f JOIN transcript_segments s ON s.id=f.segment_id AND s.meeting_id=f.meeting_id LEFT JOIN summary_coverage c ON c.segment_id=s.id AND c.meeting_id=s.meeting_id WHERE f.meeting_id=? AND f.revision=s.current_revision AND (c.revision IS NULL OR c.revision!=s.current_revision)) OR coalesce((SELECT sum(length(CAST(r.text AS BLOB))) FROM transcript_segments s JOIN transcript_revisions r ON r.segment_id=s.id AND r.revision=s.current_revision LEFT JOIN summary_coverage c ON c.meeting_id=s.meeting_id AND c.segment_id=s.id WHERE s.meeting_id=? AND s.state IN('final','edited') AND (c.revision IS NULL OR c.revision!=s.current_revision)),0)>10240",params![mid,mid],|r|r.get::<_,bool>(0)).ok()).unwrap_or(false);
                    let retrying =
                        schedule["failures"].as_u64().unwrap_or(0) > 0 && !pending.is_empty();
                    let ready = (catchup || retrying) && d["state"] == "recording"
                        || d["state"] == "recording"
                            && c.window.at_boundary_on(
                                c.elapsed,
                                c.boundary_ms,
                                mode,
                                first,
                                revised,
                                quiet,
                            );
                    if let Ok(mut state) = rt.transient.lock() {
                        state[mid]["summary_state"] = json!(if pending.is_empty() {
                            "已同步会议记忆"
                        } else if c.job.is_some() {
                            "正在更新总结与记忆"
                        } else if c.window.waiting() {
                            "等待发言停顿"
                        } else {
                            "累计新增原文"
                        });
                        state[mid]["summary_pending_chars"] =
                            json!(pending.values().map(|(_, n)| n).sum::<usize>());
                    }
                    if ready || (tail && !pending.is_empty() && c.window.can_dispatch(c.elapsed)) {
                        if tail {
                            if let Ok(db) = rt.db() {
                                let _ = db.execute(
                                    "UPDATE summary_schedules SET tail_done=1 WHERE meeting_id=?",
                                    [mid],
                                );
                            }
                        }
                        match crate::analysis::submit_auto(rt.clone(), mid, version) {
                            Ok(v) => {
                                c.job = v["job_id"].as_str().map(str::to_string);
                                c.window.dispatched();
                            }
                            Err(_) => c.window.completed(c.elapsed, &pending, true, 0),
                        }
                    }
                }
            }
        }
        // HTTP cancellation observes shutting_down; join all model threads before allowing native exit.
        if let Ok(mut handles) = rt.analysis_threads.lock() {
            for handle in handles.drain(..) {
                let _ = handle.join();
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        analysis,
        commands::{self, Request},
        db::{id, now},
        models::{self, tests::MemoryVault, ProfileInput},
    };
    use std::{
        io::{Read, Write},
        net::{TcpListener, TcpStream},
        time::Duration,
    };
    fn runtime() -> (std::path::PathBuf, Arc<Runtime>) {
        let root = std::env::temp_dir().join(format!("helmmeet-workspace-{}", id()));
        let rt = Runtime::open_with_vault(root.clone(), Arc::new(MemoryVault::default())).unwrap();
        (root, rt)
    }
    fn meeting(rt: &Runtime, template: &str) -> String {
        rt.create("合成测试会议", template).unwrap()["id"]
            .as_str()
            .unwrap()
            .into()
    }
    fn seed(rt: &Runtime, m: &str, text: &str, at: u64) -> String {
        rt.db()
            .unwrap()
            .execute(
                "UPDATE meetings SET state='finalizing' WHERE id=? AND state='ready'",
                [m],
            )
            .unwrap();
        let s = id();
        let db = rt.db().unwrap();
        db.execute(
            "INSERT INTO transcript_segments VALUES (?,?,?,?, 'mixed',1,'final')",
            params![s, m, at, at + 1000],
        )
        .unwrap();
        db.execute(
            "INSERT INTO transcript_revisions VALUES (?,1,?,?,'asr',?)",
            params![s, m, text, now()],
        )
        .unwrap();
        s
    }
    fn enable(rt: &Runtime, m: &str) {
        let v = rt.detail(m).unwrap()["version"].as_u64().unwrap();
        models::authorize(rt, m, v, true, 0, None).unwrap();
        rt.db()
            .unwrap()
            .execute("UPDATE meetings SET context_consent=1 WHERE id=?", [m])
            .unwrap();
        set_schedule(rt, m, 1, true, "realtime").unwrap();
    }
    fn model(rt: &Runtime, url: &str) {
        models::save(
            rt,
            ProfileInput {
                profile_id: None,
                expected_version: None,
                display_name: "本地假服务".into(),
                base_url: url.into(),
                model: "fixture".into(),
                context_tokens: 32768,
                max_output_tokens: 4096,
                timeout_seconds: 10,
                key_action: "clear".into(),
                api_key: String::new(),
            },
        )
        .unwrap();
    }
    fn receive(stream: &mut TcpStream) -> Value {
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut data = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            let n = stream.read(&mut buf).unwrap();
            assert!(n > 0);
            data.extend_from_slice(&buf[..n]);
            if let Some(pos) = data.windows(4).position(|w| w == b"\r\n\r\n") {
                let header = String::from_utf8_lossy(&data[..pos]);
                let size = header
                    .lines()
                    .find_map(|l| {
                        l.to_lowercase()
                            .strip_prefix("content-length:")
                            .and_then(|v| v.trim().parse::<usize>().ok())
                    })
                    .unwrap();
                if data.len() >= pos + 4 + size {
                    let body: Value =
                        serde_json::from_slice(&data[pos + 4..pos + 4 + size]).unwrap();
                    return serde_json::from_str(body["messages"][1]["content"].as_str().unwrap())
                        .unwrap();
                }
            }
        }
    }
    fn respond(s: &mut TcpStream, input: &Value, code: u16) {
        let r = &input["transcript"][0];
        let section = match input["task_type"].as_str() {
            Some("stage") => "topic",
            Some("minutes") => "minutes",
            _ => "answer",
        };
        let output = json!({"summary":[{"text":"合成讨论主题","section":section,"kind":"discussion","evidence":[{"segment_id":r["segment_id"],"revision":r["revision"]}]}],"actions":[]});
        respond_output(s, &output, code);
    }
    fn respond_output(s: &mut TcpStream, output: &Value, code: u16) {
        let body = json!({"choices":[{"message":{"content":output.to_string()}}]}).to_string();
        write!(s,"HTTP/1.1 {code} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
    }
    fn wait(rt: &Runtime, m: &str, job: &Value) {
        let until = Instant::now() + Duration::from_secs(7);
        loop {
            let d = rt.detail(m).unwrap();
            let j = d["jobs"]
                .as_array()
                .unwrap()
                .iter()
                .find(|j| j["id"] == job["job_id"])
                .unwrap();
            if j["state"] == "succeeded" {
                break;
            }
            assert!(j["state"] != "failed", "{j}");
            assert!(Instant::now() < until, "job timed out: {j}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    fn join(rt: &Runtime) {
        let mut handles = rt.analysis_threads.lock().unwrap();
        for h in handles.drain(..) {
            h.join().unwrap();
        }
    }
    #[test]
    fn transcript_snapshot_keeps_original_and_current_separate_and_stable() {
        let (root, rt) = runtime();
        let m = meeting(&rt, "general");
        let other = meeting(&rt, "weekly");
        let s = seed(&rt, &m, "原始识别", 0);
        seed(&rt, &other, "另一场不得混入", 0);
        commands::run(
            rt.clone(),
            Request::Revise {
                meeting_id: m.clone(),
                segment_id: s.clone(),
                expected_revision: 1,
                text: "人工修订".into(),
            },
        )
        .unwrap();
        let a = transcript(&rt, &m, "original", true).unwrap();
        let b = transcript(&rt, &m, "current", false).unwrap();
        assert_eq!(a["text"], "[00:00] 原始识别");
        assert_eq!(b["text"], "人工修订");
        assert_ne!(a["hash"], b["hash"]);
        assert_eq!(a["segments"][0]["revision"], 1);
        assert_eq!(b["segments"][0]["revision"], 2);
        assert!(copy(&rt, &m, "current", false, "outdated").is_err()); // Must fail before touching pasteboard.
        assert!(transcript(&rt, &m, "unknown", true).is_err());
        drop(rt);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn template_edits_do_not_rebind_history_and_new_meetings_use_new_version() {
        let (root, rt) = runtime();
        let m = meeting(&rt, "review");
        let old = rt.detail(&m).unwrap()["template"].clone();
        commands::run(
            rt.clone(),
            Request::SaveTemplate {
                id: Some("review".into()),
                name: "新评审".into(),
                instructions: "新模板".into(),
                policy: None,
                expected_version: Some(1),
            },
        )
        .unwrap();
        assert_eq!(rt.detail(&m).unwrap()["template"], old);
        assert_eq!(
            rt.detail(&meeting(&rt, "review")).unwrap()["template"]["version"],
            2
        );
        assert!(rt
            .db()
            .unwrap()
            .execute("UPDATE template_versions SET name='overwrite'", [])
            .is_err());
        assert_eq!(rt.detail(&m).unwrap()["schedule"]["enabled"], false);
        drop(rt);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn long_segment_backlog_commits_bounded_fragments_and_resumes_after_restart() {
        let (root, mut rt) = runtime();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        model(&rt, &format!("http://{}", listener.local_addr().unwrap()));
        let mid = meeting(&rt, "weekly");
        let text = "合成讨论目标需要核对。".repeat(2200);
        let sid = seed(&rt, &mid, &text, 0);
        enable(&rt, &mid);
        let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop = done.clone();
        let server = std::thread::spawn(move || {
            let mut calls = 0;
            while !stop.load(std::sync::atomic::Ordering::Acquire) {
                let (mut stream, _) = match listener.accept() {
                    Ok(v) => v,
                    Err(_) => {
                        std::thread::sleep(Duration::from_millis(10));
                        continue;
                    }
                };
                let input = receive(&mut stream);
                let source = &input["allowed_evidence"][0];
                if let Some(rows) = input["transcript"].as_array() {
                    assert!(
                        rows.iter()
                            .map(|r| r["text"].as_str().unwrap().len())
                            .sum::<usize>()
                            <= 10 * 1024
                    );
                }
                let output = json!({"summary":[{"text":"合成讨论目标待核对","kind":"discussion","section":"topic","evidence":[source]}],"actions":[]});
                let body=json!({"choices":[{"message":{"content":output.to_string()},"finish_reason":"stop"}]}).to_string();
                write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
                calls += 1;
            }
            calls
        });
        let mut rounds = 0;
        while !pending(&rt, &mid).unwrap().is_empty() && rounds < 12 {
            let job = analysis::submit_auto(rt.clone(), &mid, 2).unwrap();
            wait(&rt, &mid, &job);
            let state: String = rt
                .db()
                .unwrap()
                .query_row(
                    "SELECT state FROM analysis_jobs WHERE id=?",
                    [job["job_id"].as_str().unwrap()],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(state, "succeeded", "{}", rt.detail(&mid).unwrap());
            rounds += 1;
            if rounds == 1 {
                assert!(!pending(&rt, &mid).unwrap().is_empty());
                assert!(
                    rt.db()
                        .unwrap()
                        .query_row(
                            "SELECT count(*) FROM summary_fragments WHERE segment_id=?",
                            [&sid],
                            |r| r.get::<_, usize>(0)
                        )
                        .unwrap()
                        > 0
                );
                for h in rt.analysis_threads.lock().unwrap().drain(..) {
                    h.join().unwrap();
                }
                drop(rt);
                rt = Runtime::open_with_vault(root.clone(), Arc::new(MemoryVault::default()))
                    .unwrap();
            }
        }
        done.store(true, std::sync::atomic::Ordering::Release);
        let calls = server.join().unwrap();
        assert!(rounds > 1 && rounds < 12);
        assert!(calls >= rounds);
        assert!(pending(&rt, &mid).unwrap().is_empty());
        let d = rt.detail(&mid).unwrap();
        let latest = d["artifacts"][0]["id"].as_str().unwrap();
        assert_eq!(
            rt.db()
                .unwrap()
                .query_row(
                    "SELECT artifact_id FROM meeting_memories WHERE artifact_id=?",
                    [latest],
                    |r| r.get::<_, String>(0)
                )
                .unwrap(),
            latest
        );
        let memory = crate::memory::context(&rt, &mid, "all").unwrap();
        assert!(!memory["facts"].as_array().unwrap().is_empty());
        assert!(crate::memory::context(&rt, &mid, "recent")
            .unwrap()
            .is_null());
        rt.db()
            .unwrap()
            .execute("UPDATE meetings SET context_consent=0 WHERE id=?", [&mid])
            .unwrap();
        assert!(crate::memory::context(&rt, &mid, "all").unwrap()["facts"]
            .as_array()
            .unwrap()
            .is_empty());
        drop(rt);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn completion_clock_excludes_asr_progress_during_model_work() {
        let (root, rt) = runtime();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        model(&rt, &format!("http://{}", listener.local_addr().unwrap()));
        let mid = meeting(&rt, "weekly");
        seed(&rt, &mid, "合成原文目标", 0);
        enable(&rt, &mid);
        let owner = rt.clone();
        let meeting = mid.clone();
        let server = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let input = receive(&mut s);
            owner
                .db()
                .unwrap()
                .execute("INSERT INTO asr_chunks VALUES (?,0,180000)", [&meeting])
                .unwrap();
            respond(&mut s, &input, 200);
        });
        let job = analysis::submit(rt.clone(), &mid, "stage", "", "all").unwrap();
        wait(&rt, &mid, &job);
        server.join().unwrap();
        assert_eq!(
            rt.db()
                .unwrap()
                .query_row(
                    "SELECT completed_asr_ms FROM summary_schedules WHERE meeting_id=?",
                    [&mid],
                    |r| r.get::<_, u64>(0)
                )
                .unwrap(),
            180000
        );
        assert_eq!(
            rt.db()
                .unwrap()
                .query_row(
                    "SELECT count(*) FROM meeting_memories WHERE meeting_id=?",
                    [&mid],
                    |r| r.get::<_, u64>(0)
                )
                .unwrap(),
            1
        );
        drop(rt);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn priority_dispatch_freezes_latest_text_and_sends_incremental_evidence() {
        let (root, rt) = runtime();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        model(&rt, &format!("http://{}", listener.local_addr().unwrap()));
        let a = meeting(&rt, "weekly");
        let b = meeting(&rt, "decision");
        seed(&rt, &a, "第一段", 0);
        seed(&rt, &b, "问答依据", 0);
        enable(&rt, &a);
        enable(&rt, &b);
        let server = std::thread::spawn(move || {
            let mut seen = vec![];
            for _ in 0..3 {
                let (mut s, _) = listener.accept().unwrap();
                let v = receive(&mut s);
                respond(&mut s, &v, 200);
                seen.push(v);
            }
            seen
        });
        let gate = rt.llm_gate.lock().unwrap();
        let auto = analysis::submit_auto(rt.clone(), &a, 2).unwrap();
        let second = seed(&rt, &a, "排队后新增", 1000);
        let qa = analysis::submit(rt.clone(), &b, "answer", "目标是什么", "all").unwrap();
        drop(gate);
        wait(&rt, &b, &qa);
        wait(&rt, &a, &auto);
        assert!(pending(&rt, &a).unwrap().is_empty());
        let third = seed(&rt, &a, "下一轮新增", 2000);
        let auto2 = analysis::submit_auto(rt.clone(), &a, 2).unwrap();
        wait(&rt, &a, &auto2);
        let seen = server.join().unwrap();
        assert_eq!(seen[0]["task_type"], "answer");
        assert_eq!(seen[1]["task_type"], "stage");
        assert_eq!(seen[1]["transcript"].as_array().unwrap().len(), 2);
        let texts = seen[2]["transcript"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|s| s["text"].as_str())
            .collect::<Vec<_>>();
        assert!(texts.contains(&"下一轮新增"));
        assert!(!texts.contains(&"排队后新增"));
        assert!(seen[2]["previous_summary"].is_object());
        let raw: String = rt
            .db()
            .unwrap()
            .query_row(
                "SELECT input_snapshot FROM analysis_jobs WHERE id=?",
                [auto2["job_id"].as_str().unwrap()],
                |r| r.get(0),
            )
            .unwrap();
        assert!(!raw.contains("下一轮新增"));
        assert!(raw.contains(&third));
        assert!(!raw.contains(&second));
        assert!(raw.contains("new_manifest"));
        join(&rt);
        drop(rt);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn dialogue_history_is_authorized_and_clear_cancels_only_chat() {
        let (root, rt) = runtime();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        model(&rt, &format!("http://{}", listener.local_addr().unwrap()));
        let m = meeting(&rt, "review");
        seed(&rt, &m, "讨论依据", 0);
        enable(&rt, &m);
        let server = std::thread::spawn(move || {
            let mut seen = vec![];
            for _ in 0..2 {
                let (mut s, _) = listener.accept().unwrap();
                let v = receive(&mut s);
                respond(&mut s, &v, 200);
                seen.push(v);
            }
            seen
        });
        let a = analysis::submit(rt.clone(), &m, "answer", "有哪些假设？", "all").unwrap();
        wait(&rt, &m, &a);
        let b = analysis::submit(rt.clone(), &m, "answer", "其中哪个需要验证？", "all").unwrap();
        wait(&rt, &m, &b);
        let seen = server.join().unwrap();
        assert!(seen[0]["chat_history"].as_array().unwrap().is_empty());
        assert_eq!(seen[1]["chat_history"][0]["question"], "有哪些假设？");
        let gate = rt.llm_gate.lock().unwrap();
        let qa = analysis::submit(rt.clone(), &m, "answer", "排队提问", "all").unwrap();
        let auto = analysis::submit_auto(rt.clone(), &m, 2).unwrap();
        set_schedule(&rt, &m, 2, false, "realtime").unwrap();
        let state = |job: &Value| {
            rt.db()
                .unwrap()
                .query_row(
                    "SELECT state FROM analysis_jobs WHERE id=?",
                    [job["job_id"].as_str().unwrap()],
                    |r| r.get::<_, String>(0),
                )
                .unwrap()
        };
        assert_eq!(state(&auto), "cancelled");
        assert_eq!(state(&qa), "pending");
        commands::run(
            rt.clone(),
            Request::ClearConversation {
                meeting_id: m.clone(),
            },
        )
        .unwrap();
        assert_eq!(state(&qa), "cancelled");
        drop(gate);
        join(&rt);
        assert!(rt.detail(&m).unwrap()["conversation"]
            .as_array()
            .unwrap()
            .is_empty());
        assert_eq!(
            rt.detail(&m).unwrap()["artifacts"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        drop(rt);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn transient_service_failures_keep_coverage_and_backoff_the_shared_model() {
        let (root, rt) = runtime();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        model(&rt, &format!("http://{}", listener.local_addr().unwrap()));
        let mid = meeting(&rt, "weekly");
        seed(&rt, &mid, "合成原文保持不变", 0);
        enable(&rt, &mid);
        let server = std::thread::spawn(move || {
            for _ in 0..3 {
                let (mut s, _) = listener.accept().unwrap();
                let input = receive(&mut s);
                respond(&mut s, &input, 503);
            }
        });
        let job = analysis::submit_auto(rt.clone(), &mid, 2).unwrap();
        join(&rt);
        assert_eq!(
            rt.db()
                .unwrap()
                .query_row(
                    "SELECT state FROM analysis_jobs WHERE id=?",
                    [job["job_id"].as_str().unwrap()],
                    |r| r.get::<_, String>(0)
                )
                .unwrap(),
            "failed"
        );
        server.join().unwrap();
        let d = rt.detail(&mid).unwrap();
        assert_eq!(d["schedule"]["failures"], 1);
        assert!(d["schedule"]["paused_reason"].is_null());
        assert_eq!(pending(&rt, &mid).unwrap().len(), 1);
        let until: i64 = rt
            .db()
            .unwrap()
            .query_row(
                "SELECT retry_after_utc FROM summary_model_backoff",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(until >= chrono::Utc::now().timestamp() + 295);
        assert!(d["artifacts"].as_array().unwrap().is_empty());
        drop(rt);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn invalid_output_retries_with_backoff_then_pauses_without_advancing_and_can_resume() {
        let (root, rt) = runtime();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        model(&rt, &format!("http://{}", listener.local_addr().unwrap()));
        let m = meeting(&rt, "general");
        seed(&rt, &m, "合成依据保持不变", 0);
        enable(&rt, &m);
        let server = std::thread::spawn(move || {
            // Three failed jobs, each including the single format repair.
            for _ in 0..6 {
                let (mut s, _) = listener.accept().unwrap();
                let input = receive(&mut s);
                let r = &input["transcript"][0];
                let item = json!({"text":"合成内容","section":"topic","kind":"discussion","evidence":[{"segment_id":r["segment_id"],"revision":r["revision"]}]});
                let mut invalid = item.clone();
                invalid["kind"] = json!("progress");
                respond_output(
                    &mut s,
                    &json!({"summary":[item.clone(),item.clone(),item,invalid],"actions":[]}),
                    200,
                );
            }
            let (mut s, _) = listener.accept().unwrap();
            let input = receive(&mut s);
            respond(&mut s, &input, 200);
        });
        for attempt in 1..=3 {
            let before = chrono::Utc::now().timestamp();
            analysis::submit_auto(rt.clone(), &m, 2).unwrap();
            join(&rt);
            let d = rt.detail(&m).unwrap();
            assert_eq!(d["schedule"]["failures"], attempt);
            assert_eq!(d["schedule"]["paused_reason"].is_string(), attempt == 3);
            assert_eq!(pending(&rt, &m).unwrap().len(), 1);
            assert!(d["artifacts"].as_array().unwrap().is_empty());
            let (retry, covered, message, shared): (i64, u64, String, u64) = rt.db().unwrap().query_row(
                "SELECT retry_after_utc,completed_asr_ms,(SELECT error_code FROM analysis_jobs ORDER BY rowid DESC LIMIT 1),(SELECT count(*) FROM summary_model_backoff) FROM summary_schedules WHERE meeting_id=?",
                [&m], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).unwrap();
            assert!(
                retry
                    >= before
                        + match attempt {
                            1 => 30,
                            2 => 120,
                            _ => 300,
                        }
            );
            assert_eq!(covered, 0);
            assert_eq!(
                shared, 0,
                "output errors must not block other meetings' model"
            );
            assert!(message.starts_with("模型输出校验失败：summary[3].kind"));
        }
        set_schedule(&rt, &m, 2, true, "realtime").unwrap();
        let job = analysis::submit_auto(rt.clone(), &m, 3).unwrap();
        wait(&rt, &m, &job);
        join(&rt);
        server.join().unwrap();
        let d = rt.detail(&m).unwrap();
        assert_eq!(d["schedule"]["failures"], 0);
        assert!(d["schedule"]["paused_reason"].is_null());
        assert!(pending(&rt, &m).unwrap().is_empty());
        drop(rt);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn nonretryable_error_pauses_auto_and_does_not_advance_coverage() {
        let (root, rt) = runtime();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        model(&rt, &format!("http://{}", listener.local_addr().unwrap()));
        let m = meeting(&rt, "general");
        seed(&rt, &m, "测试依据", 0);
        enable(&rt, &m);
        let server = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let v = receive(&mut s);
            respond(&mut s, &v, 401);
        });
        analysis::submit_auto(rt.clone(), &m, 2).unwrap();
        join(&rt);
        server.join().unwrap();
        let d = rt.detail(&m).unwrap();
        assert!(d["schedule"]["paused_reason"].is_string());
        assert_eq!(d["schedule"]["failures"], 1);
        assert_eq!(pending(&rt, &m).unwrap().len(), 1);
        drop(rt);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn paused_queued_auto_waits_for_resume_and_restart_keeps_coverage_not_tail() {
        let (root, rt) = runtime();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        model(&rt, &format!("http://{}", listener.local_addr().unwrap()));
        let m = meeting(&rt, "general");
        seed(&rt, &m, "暂停时的确认文字", 0);
        enable(&rt, &m);
        rt.db()
            .unwrap()
            .execute("UPDATE meetings SET state='paused' WHERE id=?", [&m])
            .unwrap();
        let job = analysis::submit_auto(rt.clone(), &m, 2).unwrap();
        listener.set_nonblocking(true).unwrap();
        std::thread::sleep(Duration::from_millis(200));
        assert!(listener.accept().is_err());
        assert_eq!(rt.detail(&m).unwrap()["jobs"][0]["state"], "pending");
        listener.set_nonblocking(false).unwrap();
        let server = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let v = receive(&mut s);
            respond(&mut s, &v, 200);
        });
        rt.db()
            .unwrap()
            .execute("UPDATE meetings SET state='recording' WHERE id=?", [&m])
            .unwrap();
        wait(&rt, &m, &job);
        join(&rt);
        server.join().unwrap();
        rt.db()
            .unwrap()
            .execute(
                "UPDATE summary_schedules SET tail_done=0 WHERE meeting_id=?",
                [&m],
            )
            .unwrap();
        drop(rt);
        let reopened =
            Runtime::open_with_vault(root.clone(), Arc::new(MemoryVault::default())).unwrap();
        assert_eq!(reopened.detail(&m).unwrap()["state"], "interrupted");
        assert!(pending(&reopened, &m).unwrap().is_empty());
        assert!(reopened
            .db()
            .unwrap()
            .query_row(
                "SELECT tail_done=1 FROM summary_schedules WHERE meeting_id=?",
                [&m],
                |r| r.get::<_, bool>(0)
            )
            .unwrap());
        drop(reopened);
        std::fs::remove_dir_all(root).unwrap();
    }
}
