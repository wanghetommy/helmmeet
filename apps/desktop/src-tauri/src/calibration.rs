//! Optional transcript calibration. Raw ASR revisions remain authoritative;
//! calibration is a separately stored, reviewable paragraph projection.
use crate::{
    analysis,
    db::{err, id, now, Runtime},
    knowledge, models, paragraphs,
};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::{sync::Arc, time::Duration};

const RULES: &str = r#"你是会议语音识别文本校准器。只校准已确认的语音识别文字，不做会议总结。
必须遵守：不增删事实，不改变原意，不补写未听到的内容；专有名词只能从提供的词典中选择；数字、日期、金额、单位、网址、代码和人名没有明确依据时不得改写。
标点要求：在不改变原意的前提下，根据句子结构、语义关系和已确认停顿，适当增加、减少或替换中文标点，使段落更易阅读。可以补充完整句末标点、调整并列标点、删除重复或明显错误的标点；不确定时少改。不得通过标点改变陈述、疑问、否定或推测语气，不得用重新断句制造事实。
上一段和 rolling_context 只读用于理解上下文，当前 targets 才是允许修改的对象。必须严格返回唯一 JSON 对象：{"results":[{"paragraph_id":"目标ID","corrected_text":"校准文本","changes":[]}],"rolling_context":{"decisions":[],"open_questions":[],"terms":[]}}。results 必须逐一覆盖所有 targets，不能返回 summary/actions，也不能输出 Markdown。rolling_context 只保留本批及此前已提供原文中有依据的简短会议上下文，不得补造事实；没有内容时返回空数组。"#;
type CalibrationBatch = (Vec<Value>, Option<Value>, Value, Value);

pub fn set_enabled(
    rt: &Runtime,
    meeting: &str,
    expected_version: u64,
    enabled: bool,
    mode: &str,
) -> Result<Value, String> {
    rt.check_id(meeting)?;
    if !matches!(mode, "low_latency" | "standard" | "high_accuracy") {
        return Err("校准频率无效".into());
    }
    if enabled {
        let permitted: bool = rt.db()?.query_row(
            "SELECT analysis_mode!='transcript_only' AND context_consent=1 FROM meetings WHERE id=?",
            [meeting],
            |r| r.get(0),
        ).map_err(err)?;
        if !permitted {
            return Err("请先在本场分析设置中授权文本发送，才能启用语音校准".into());
        }
    }
    let db = rt.db()?;
    if db.execute(
        "UPDATE meetings SET calibration_enabled=?,calibration_mode=?,version=version+1 WHERE id=? AND version=?",
        params![enabled, mode, meeting, expected_version],
    ).map_err(err)? != 1 {
        return Err("会议设置已变化，请刷新后重试".into());
    }
    if enabled {
        db.execute(
            "UPDATE transcript_calibrations SET status='stale',error=NULL,updated_at=? WHERE meeting_id=? AND status='failed'",
            params![now(), meeting],
        )
        .map_err(err)?;
    }
    Ok(json!({"saved":true,"enabled":enabled,"mode":mode}))
}

pub fn run_now(rt: &Runtime, meeting: &str, expected_version: u64) -> Result<Value, String> {
    rt.check_id(meeting)?;
    let db = rt.db()?;
    let (enabled, mode): (bool, String) = db
        .query_row(
            "SELECT calibration_enabled,calibration_mode FROM meetings WHERE id=? AND version=?",
            params![meeting, expected_version],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(|_| "会议设置已变化，请刷新后重试")?;
    if !enabled {
        return Err("请先开启实时语音校准".into());
    }
    let permitted: bool = db
        .query_row(
            "SELECT analysis_mode!='transcript_only' AND context_consent=1 FROM meetings WHERE id=?",
            [meeting],
            |r| r.get(0),
        )
        .map_err(err)?;
    if !permitted {
        return Err("请先授权本场文本分析".into());
    }
    db.execute(
        "UPDATE meetings SET version=version+1 WHERE id=? AND version=?",
        params![meeting, expected_version],
    )
    .map_err(err)?;
    db.execute(
        "UPDATE transcript_calibrations SET status='stale',error=NULL,updated_at=? WHERE meeting_id=? AND status IN ('failed','needs_review')",
        params![now(), meeting],
    )
    .map_err(err)?;
    Ok(json!({"queued":true,"mode":mode}))
}

pub fn confirm(
    rt: &Runtime,
    meeting: &str,
    paragraph: &str,
    expected_version: u64,
) -> Result<Value, String> {
    rt.check_id(meeting)?;
    if paragraph.trim().is_empty() {
        return Err("校准段落无效".into());
    }
    let mut db = rt.db()?;
    let tx = db.transaction().map_err(err)?;
    let manifest: String = tx
        .query_row(
            "SELECT source_manifest_json FROM transcript_calibrations WHERE meeting_id=? AND paragraph_id=? AND status='needs_review' ORDER BY rowid DESC LIMIT 1",
            params![meeting, paragraph],
            |r| r.get(0),
        )
        .map_err(|_| "待确认校准不存在或已处理")?;
    let members =
        serde_json::from_str::<Value>(&manifest).map_err(|_| "校准来源清单无效，请重新校准")?;
    for member in members.as_array().into_iter().flatten() {
        let segment = member["segment_id"]
            .as_str()
            .ok_or("校准来源清单缺少片段")?;
        let revision = member["revision"]
            .as_u64()
            .ok_or("校准来源清单缺少修订号")?;
        let current: u64 = tx
            .query_row(
                "SELECT current_revision FROM transcript_segments WHERE id=? AND meeting_id=?",
                params![segment, meeting],
                |r| r.get(0),
            )
            .map_err(|_| "原文已变化，请重新校准")?;
        if current != revision {
            return Err("原文已变化，请重新校准".into());
        }
    }
    if tx
        .execute(
            "UPDATE transcript_calibrations SET status='succeeded',error=NULL,updated_at=? WHERE meeting_id=? AND paragraph_id=? AND status='needs_review'",
            params![now(), meeting, paragraph],
        )
        .map_err(err)?
        != 1
    {
        return Err("校准状态已变化，请刷新后重试".into());
    }
    if tx
        .execute(
            "UPDATE meetings SET version=version+1 WHERE id=? AND version=?",
            params![meeting, expected_version],
        )
        .map_err(err)?
        != 1
    {
        return Err("会议内容已变化，请刷新后重试".into());
    }
    tx.commit().map_err(err)?;
    Ok(json!({"confirmed":true,"paragraph_id":paragraph}))
}

pub fn detail(db: &Connection, meeting: &str) -> Result<Value, String> {
    let (enabled, mode, pending, succeeded, review, failed): (bool, String, u64, u64, u64, u64) = db
        .query_row(
            "SELECT calibration_enabled,calibration_mode,
                    coalesce(sum(status='pending'),0),
                    coalesce(sum(status='succeeded'),0),
                    coalesce(sum(status='needs_review'),0),
                    coalesce(sum(status='failed'),0)
             FROM meetings LEFT JOIN transcript_calibrations ON transcript_calibrations.meeting_id=meetings.id
             WHERE meetings.id=?",
            [meeting],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
        )
        .map_err(err)?;
    let run: Option<Value> = db
        .query_row(
            "SELECT id,state,total_paragraphs,completed_paragraphs,cursor,error FROM transcript_calibration_runs WHERE meeting_id=? ORDER BY rowid DESC LIMIT 1",
            [meeting],
            |r| Ok(json!({"id":r.get::<_,String>(0)?,"state":r.get::<_,String>(1)?,"total":r.get::<_,u64>(2)?,"completed":r.get::<_,u64>(3)?,"cursor":r.get::<_,u64>(4)?,"error":r.get::<_,Option<String>>(5)?})),
        )
        .optional()
        .map_err(err)?;
    Ok(
        json!({"enabled":enabled,"mode":mode,"pending":pending,"succeeded":succeeded,"needs_review":review,"failed":failed,"run":run}),
    )
}

pub fn decorate(
    db: &Connection,
    meeting: &str,
    paragraphs: Vec<Value>,
) -> Result<Vec<Value>, String> {
    let mut out = paragraphs;
    let review_segments: std::collections::HashSet<String> = db
        .prepare("SELECT source_manifest_json FROM transcript_calibrations WHERE meeting_id=? AND status='needs_review'")
        .map_err(err)?
        .query_map([meeting], |r| r.get::<_, String>(0))
        .map_err(err)?
        .filter_map(Result::ok)
        .flat_map(|raw| serde_json::from_str::<Value>(&raw).ok().into_iter())
        .flat_map(|manifest| manifest.as_array().cloned().unwrap_or_default())
        .filter_map(|member| member["segment_id"].as_str().map(str::to_owned))
        .collect();
    for paragraph in &mut out {
        let pid = paragraph["id"].as_str().unwrap_or("");
        let raw = paragraph["text"].as_str().unwrap_or("").to_owned();
        let manifest = paragraph["members"].to_string();
        let row: Option<(String, Option<String>, String, String)> = db
            .query_row(
                "SELECT source_manifest_json,corrected_text,status,changes_json
                 FROM transcript_calibrations
                 WHERE meeting_id=? AND paragraph_id=? AND source_manifest_json=?
                 ORDER BY rowid DESC LIMIT 1",
                params![meeting, pid, manifest],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()
            .map_err(err)?;
        paragraph["raw_text"] = json!(raw);
        if let Some((_, corrected, status, changes)) = row {
            paragraph["calibration"] = json!({
                "status": status,
                "changes": serde_json::from_str::<Value>(&changes).unwrap_or_else(|_| json!([])),
            });
            if matches!(status.as_str(), "succeeded" | "needs_review") {
                if let Some(text) = corrected.filter(|t| !t.trim().is_empty()) {
                    paragraph["text"] = json!(text);
                }
            }
        } else if paragraph["members"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|member| {
                member["segment_id"]
                    .as_str()
                    .is_some_and(|id| review_segments.contains(id))
            })
        {
            paragraph["calibration"] = json!({"status":"needs_review","changes":[]});
        } else {
            paragraph["calibration"] = json!({"status":"none","changes":[]});
        }
    }
    Ok(out)
}

fn rows_for_meeting(
    db: &Connection,
    meeting: &str,
) -> Result<(String, u64, Vec<Value>, bool), String> {
    let (state, duration): (String, u64) = db
        .query_row(
            "SELECT state,recording_duration_ms FROM meetings WHERE id=?",
            [meeting],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(err)?;
    let mut q = db
        .prepare("SELECT s.id,s.start_ms,s.end_ms,s.current_revision,r.text,s.state FROM transcript_segments s JOIN transcript_revisions r ON r.segment_id=s.id AND r.revision=s.current_revision WHERE s.meeting_id=? AND s.state='final' AND r.origin='asr' ORDER BY s.start_ms,s.id")
        .map_err(err)?;
    let rows = q
        .query_map([meeting], |r| {
            Ok(json!({"id":r.get::<_,String>(0)?,"start_ms":r.get::<_,u64>(1)?,"end_ms":r.get::<_,u64>(2)?,"revision":r.get::<_,u64>(3)?,"text":r.get::<_,String>(4)?,"state":r.get::<_,String>(5)?}))
        })
        .map_err(err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(err)?;
    let active = matches!(state.as_str(), "recording" | "finalizing");
    Ok((state, duration, rows, active))
}

fn thresholds(mode: &str) -> (usize, usize, usize) {
    match mode {
        "low_latency" => (400, 2, 45),
        "high_accuracy" => (1200, 3, 120),
        _ => (800, 2, 90),
    }
}

fn digits(text: &str) -> Vec<String> {
    let mut out = vec![];
    let mut current = String::new();
    for c in text.chars() {
        if c.is_ascii_digit() {
            current.push(c);
        } else if !current.is_empty() {
            out.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

fn suspicious(raw: &str, corrected: &str, changes: &Value, dictionary: &Value) -> bool {
    if digits(raw) != digits(corrected) {
        return true;
    }
    let raw_len = raw.chars().count().max(1) as f32;
    let delta =
        (corrected.chars().count() as isize - raw.chars().count() as isize).unsigned_abs() as f32;
    if delta / raw_len > 0.25 {
        return true;
    }
    let allowed = dictionary["terms"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| v["term"]["canonical"].as_str())
        .collect::<std::collections::HashSet<_>>();
    changes
        .as_array()
        .into_iter()
        .flatten()
        .any(|c| c["type"] == "term" && c["to"].as_str().is_some_and(|to| !allowed.contains(to)))
}

pub fn background(rt: Arc<Runtime>) -> crate::lifecycle::Worker {
    crate::lifecycle::Worker::spawn(move |stop| {
        while stop.wait(Duration::from_secs(1)) {
            let _ = tick(&rt);
        }
    })
}

pub fn tick(rt: &Runtime) -> Result<(), String> {
    let meetings: Vec<(String, String)> = {
        let db = rt.db()?;
        let mut q = db.prepare("SELECT id,calibration_mode FROM meetings WHERE calibration_enabled=1 AND state IN('recording','paused','finalizing','completed','needs_attention') AND analysis_mode!='transcript_only' AND context_consent=1 ORDER BY created_at").map_err(err)?;
        let rows = q
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .map_err(err)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(err)?;
        rows
    };
    for (meeting, mode) in meetings {
        let Ok(_permit) = rt.llm_gate.try_lock() else {
            // Another analysis request currently owns the shared model slot.
            // Leave this meeting for the next tick instead of starving later
            // calibration-enabled meetings in the same scan.
            continue;
        };
        let input_budget = models::selected(rt, &meeting)
            .ok()
            .and_then(|c| analysis::input_budget(&c).ok())
            .unwrap_or(100 * 1024);
        let Some(batch) = (match candidate_batch_with_dictionary(rt, &meeting, &mode, input_budget)
        {
            Ok(batch) => batch,
            // A malformed/removed local dictionary must not prevent other
            // meetings from getting a calibration attempt.
            Err(_) => continue,
        }) else {
            continue;
        };
        match calibrate(rt, &meeting, &mode, batch) {
            Ok(true) => return Ok(()),
            Ok(false) | Err(_) => {
                // The pending rows are marked failed by calibrate when the
                // request or validation fails. Continue scanning so one bad
                // meeting cannot block all other enabled meetings.
            }
        }
    }
    Ok(())
}

fn candidate_batch_with_dictionary(
    rt: &Runtime,
    meeting: &str,
    mode: &str,
    input_budget: usize,
) -> Result<Option<CalibrationBatch>, String> {
    let db = rt.db()?;
    let (state, duration, rows, active) = rows_for_meeting(&db, meeting)?;
    let projected = paragraphs::project(&db, meeting, &rows, active)?;
    let (min_chars, min_paragraphs, max_wait_seconds) = thresholds(mode);
    let mut batch = vec![];
    let mut previous = None;
    let mut rolling_context = json!({"decisions":[],"open_questions":[],"terms":[]});
    let mut chars = 0usize;
    let mut request_bytes = RULES.len() + 2048;
    let mut forced = false;
    if let Some(raw) = db
        .query_row(
            "SELECT rolling_context_json FROM transcript_calibrations WHERE meeting_id=? AND status IN ('succeeded','needs_review') ORDER BY rowid DESC LIMIT 1",
            [meeting],
            |r| r.get::<_, String>(0),
        )
        .optional()
        .map_err(err)?
    {
        rolling_context = serde_json::from_str(&raw).unwrap_or(rolling_context);
        request_bytes += raw.len();
    }
    for source in projected {
        for mut p in split_oversized(&source, &rows, input_budget) {
            if p["open"] == true || p["text"].as_str().unwrap_or("").trim().is_empty() {
                continue;
            }
            let pid = p["id"].as_str().unwrap_or("");
            let manifest = p["members"].to_string();
            let existing: Option<(String, String, Option<String>)> = db.query_row("SELECT status,source_manifest_json,corrected_text FROM transcript_calibrations WHERE meeting_id=? AND paragraph_id=? ORDER BY rowid DESC LIMIT 1", params![meeting,pid], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional().map_err(err)?;
            if let Some((status, saved_manifest, corrected)) = existing {
                if saved_manifest == manifest {
                    if status == "stale" {
                        forced = true;
                    }
                    if matches!(status.as_str(), "pending" | "succeeded" | "needs_review") {
                        if let Some(text) = corrected.filter(|t| !t.trim().is_empty()) {
                            p["text"] = json!(text);
                        }
                        previous = Some(p);
                        continue;
                    }
                    // A failed attempt is already visible in the meeting detail.
                    // Do not retry it every second and monopolize the model gate;
                    // a new source manifest (or an explicit setting change) will
                    // make it eligible again.
                    if status == "failed" {
                        previous = Some(p);
                        continue;
                    }
                }
            }
            let added_bytes = p.to_string().len() + 512;
            if !batch.is_empty() && request_bytes.saturating_add(added_bytes) > input_budget {
                break;
            }
            chars += p["text"].as_str().unwrap_or("").chars().count();
            request_bytes = request_bytes.saturating_add(added_bytes);
            batch.push(p);
            if batch.len() >= 128
                || (!forced && chars >= min_chars && batch.len() >= min_paragraphs)
            {
                break;
            }
        }
    }
    let completed = matches!(state.as_str(), "completed" | "needs_attention");
    let waited_long_enough = batch
        .first()
        .and_then(|p| p["end_ms"].as_u64())
        .is_some_and(|end| duration >= end.saturating_add(max_wait_seconds as u64 * 1000));
    if batch.is_empty()
        || (!forced
            && !completed
            && !waited_long_enough
            && (chars < min_chars || batch.len() < min_paragraphs))
    {
        return Ok(None);
    }
    let query = batch
        .iter()
        .map(|p| p["text"].as_str().unwrap_or(""))
        .collect::<Vec<_>>()
        .join(" ");
    // `knowledge::retrieve` acquires the runtime database mutex itself. Do
    // not call it while this read connection is still held, otherwise the
    // calibration worker self-deadlocks and every detail/list IPC waits
    // forever behind the same mutex.
    drop(db);
    let dictionary = knowledge::retrieve(rt, meeting, &query)?;
    Ok(Some((batch, previous, dictionary, rolling_context)))
}

fn split_oversized(paragraph: &Value, rows: &[Value], budget: usize) -> Vec<Value> {
    if paragraph.to_string().len().saturating_add(512) <= budget {
        return vec![paragraph.clone()];
    }
    let members = paragraph["members"].as_array().cloned().unwrap_or_default();
    if members.len() <= 1 {
        return vec![paragraph.clone()];
    }
    let mut pieces = vec![];
    let mut current = vec![];
    let mut size = 0usize;
    for member in members {
        let sid = member["segment_id"].as_str().unwrap_or("");
        let row = rows.iter().find(|r| r["id"].as_str() == Some(sid));
        let added = row.map(|r| r.to_string().len() + 256).unwrap_or(512);
        if !current.is_empty() && size.saturating_add(added) > budget {
            pieces.push(current);
            current = vec![];
            size = 0;
        }
        current.push((member, row.cloned()));
        size = size.saturating_add(added);
    }
    if !current.is_empty() {
        pieces.push(current);
    }
    if pieces.len() <= 1 {
        return vec![paragraph.clone()];
    }
    pieces
        .into_iter()
        .enumerate()
        .map(|(index, members)| {
            let first = members.first().and_then(|(_, r)| r.as_ref()).unwrap_or(paragraph);
            let last = members.last().and_then(|(_, r)| r.as_ref()).unwrap_or(first);
            let text = paragraphs::join(
                members
                    .iter()
                    .filter_map(|(_, r)| r.as_ref().and_then(|v| v["text"].as_str()).map(str::to_owned)),
            );
            let manifests = members.iter().map(|(m, _)| m.clone()).collect::<Vec<_>>();
            json!({
                "id": format!("{}-part-{}", paragraph["id"].as_str().unwrap_or("p"), index),
                "start_ms": first["start_ms"],
                "end_ms": last["end_ms"],
                "text": text,
                "segment_ids": manifests.iter().filter_map(|m| m["segment_id"].as_str()).collect::<Vec<_>>(),
                "members": manifests,
                "open": false,
                "reason": "budget",
                "boundary_source": "calibration_budget"
            })
        })
        .collect()
}

fn calibrate(
    rt: &Runtime,
    meeting: &str,
    mode: &str,
    input: CalibrationBatch,
) -> Result<bool, String> {
    let (batch, previous, dictionary, rolling_context) = input;
    let mut targets = vec![];
    let mut pending = vec![];
    for p in &batch {
        let manifest = p["members"].clone();
        let source_revision = p["members"][0]["revision"].as_u64().unwrap_or(1);
        let id = id();
        pending.push((
            id.clone(),
            p["id"].as_str().unwrap_or("").to_owned(),
            manifest.clone(),
            source_revision,
            p["text"].as_str().unwrap_or("").to_owned(),
        ));
        targets
            .push(json!({"paragraph_id":p["id"],"source_manifest":manifest,"raw_text":p["text"]}));
    }
    {
        let db = rt.db()?;
        for (id, pid, manifest, revision, raw) in &pending {
            db.execute("INSERT OR REPLACE INTO transcript_calibrations(id,meeting_id,paragraph_id,source_manifest_json,source_revision,raw_text,changes_json,previous_paragraph_id,dictionary_manifest_json,status,created_at,updated_at) VALUES (?,?,?,?,?,?,?, ?,?,'pending',?,?)", params![id,meeting,pid,manifest.to_string(),revision,raw,"[]",previous.as_ref().and_then(|p|p["id"].as_str()),dictionary["bindings"].to_string(),now(),now()]).map_err(err)?;
        }
    }
    let config = match models::selected(rt, meeting) {
        Ok(config) => config,
        Err(error) => {
            let db = rt.db()?;
            db.execute("UPDATE transcript_calibrations SET status='failed',error=?,updated_at=? WHERE meeting_id=? AND status='pending'", params![error, now(), meeting]).map_err(err)?;
            return Ok(false);
        }
    };
    // Keep the meeting's configured output budget. Calibration used to cap
    // every request at 8192 tokens, which wasted large-context model profiles;
    // request sizing and batching must constrain the input independently.
    let selected = config;
    let run_id = id();
    {
        let db = rt.db()?;
        db.execute("INSERT INTO transcript_calibration_runs(id,meeting_id,profile_id,config_version,mode,state,total_paragraphs,completed_paragraphs,cursor,input_budget_bytes,output_budget_tokens,rolling_context_json,created_at,updated_at) VALUES (?,?,?,?,?,'running',?,?,0,?,?,?, ?,?)", params![run_id,meeting,selected.profile_id,selected.config_version,mode,batch.len(),0,analysis::input_budget(&selected).unwrap_or(0),selected.max_output_tokens,rolling_context.to_string(),now(),now()]).map_err(err)?;
        db.execute("UPDATE transcript_calibrations SET run_id=?,batch_index=0 WHERE meeting_id=? AND status='pending'", params![run_id,meeting]).map_err(err)?;
    }
    let request = json!({
        "task_type":"calibration",
        "mode":mode,
        "instruction":RULES,
        "output_contract":{"results":"每个target一个结果","rolling_context":{"decisions":"array","open_questions":"array","terms":"array"}},
        "previous_reference":previous.map(|p|json!({"paragraph_id":p["id"],"text":p["text"]})).unwrap_or(Value::Null),
        "rolling_context":rolling_context,
        "dictionary":dictionary["terms"],
        "targets":targets
    });
    let check = || rt.ensure_running();
    let output = analysis::request(
        rt,
        &selected,
        json!([{"role":"system","content":RULES},{"role":"user","content":request.to_string()}]),
        &check,
    );
    let result = output.and_then(|text| analysis::parse(&text));
    let db = rt.db()?;
    match result {
        Ok(value) => {
            let applied = (|| -> Result<(), String> {
                let rows = value["results"]
                    .as_array()
                    .ok_or("校准结果缺少results数组")?;
                let next_context = value["rolling_context"]
                    .as_object()
                    .map(|_| value["rolling_context"].clone())
                    .ok_or("校准结果缺少rolling_context")?;
                for (id, pid, _manifest, _revision, raw) in &pending {
                    let item = rows
                        .iter()
                        .find(|v| v["paragraph_id"].as_str() == Some(pid.as_str()))
                        .ok_or("校准结果缺少段落")?;
                    let corrected = item["corrected_text"].as_str().ok_or("校准文本无效")?;
                    let changes = item["changes"].clone();
                    let status = if suspicious(raw, corrected, &changes, &dictionary) {
                        "needs_review"
                    } else {
                        "succeeded"
                    };
                    db.execute("UPDATE transcript_calibrations SET corrected_text=?,changes_json=?,status=?,model_json=?,rolling_context_json=?,updated_at=? WHERE id=?",params![corrected,changes.to_string(),status,serde_json::to_string(&selected).map_err(err)?,next_context.to_string(),now(),id]).map_err(err)?;
                }
                Ok(())
            })();
            if let Err(error) = applied {
                db.execute("UPDATE transcript_calibrations SET status='failed',error=?,updated_at=? WHERE meeting_id=? AND status='pending'",params![error,now(),meeting]).map_err(err)?;
                db.execute("UPDATE transcript_calibration_runs SET state='failed',error=?,updated_at=? WHERE id=?",params![error,now(),run_id]).map_err(err)?;
                return Ok(false);
            }
            let context = value["rolling_context"].clone();
            db.execute("UPDATE transcript_calibration_runs SET state='succeeded',completed_paragraphs=total_paragraphs,cursor=total_paragraphs,rolling_context_json=?,updated_at=? WHERE id=?",params![context.to_string(),now(),run_id]).map_err(err)?;
            Ok(true)
        }
        Err(error) => {
            db.execute("UPDATE transcript_calibrations SET status='failed',error=?,updated_at=? WHERE meeting_id=? AND status='pending'",params![error,now(),meeting]).map_err(err)?;
            db.execute("UPDATE transcript_calibration_runs SET state='failed',error=?,updated_at=? WHERE id=?",params![error,now(),run_id]).map_err(err)?;
            Ok(false)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calibration_thresholds_avoid_short_repeated_requests() {
        assert_eq!(thresholds("low_latency"), (400, 2, 45));
        assert_eq!(thresholds("standard"), (800, 2, 90));
        assert_eq!(thresholds("high_accuracy"), (1200, 3, 120));
    }

    #[test]
    fn calibration_guard_preserves_numbers_and_dictionary_terms() {
        let dictionary = json!({"terms":[{"term":{"canonical":"HelmMeet"}}]});
        assert!(!suspicious(
            "今年项目计划将在2026年完成并进入验收阶段",
            "今年项目计划将在 2026 年完成并进入验收阶段。",
            &json!([]),
            &dictionary
        ));
        assert!(suspicious(
            "预算为100元",
            "预算为200元。",
            &json!([]),
            &dictionary
        ));
        assert!(suspicious(
            "使用海姆会议",
            "使用HelmMeet。",
            &json!([{"type":"term","to":"未知产品"}]),
            &dictionary
        ));
    }

    #[test]
    fn confirming_reviewed_calibration_requires_current_source_revision() {
        let dir = tempfile::tempdir().unwrap();
        let rt = Runtime::open_with_vault(
            dir.path().into(),
            Arc::new(crate::models::tests::MemoryVault::default()),
        )
        .unwrap();
        let meeting = rt.create("校准确认", "general").unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned();
        let segment = id();
        let version: u64;
        {
            let db = rt.db().unwrap();
            version = db
                .query_row("SELECT version FROM meetings WHERE id=?", [&meeting], |r| {
                    r.get(0)
                })
                .unwrap();
            db.execute(
                "INSERT INTO transcript_segments VALUES (?,?,0,1000,'mixed',1,'final')",
                params![segment, meeting],
            )
            .unwrap();
            db.execute(
                "INSERT INTO transcript_revisions VALUES (?,1,?,'原始文字','asr',?)",
                params![segment, meeting, now()],
            )
            .unwrap();
            let manifest = json!([{"segment_id":segment,"revision":1}]);
            db.execute(
                "INSERT INTO transcript_calibrations(id,meeting_id,paragraph_id,source_manifest_json,source_revision,raw_text,corrected_text,changes_json,status,created_at,updated_at) VALUES (?,?,?,?,?,?,?,'[]','needs_review',?,?)",
                params![id(), meeting, "p-test", manifest.to_string(), 1, "原始文字", "校准文字", now(), now()],
            )
            .unwrap();
        }
        assert!(confirm(&rt, &meeting, "p-test", version).unwrap()["confirmed"] == true);
        assert_eq!(
            rt.db()
                .unwrap()
                .query_row(
                    "SELECT status FROM transcript_calibrations WHERE meeting_id=? AND paragraph_id=?",
                    params![meeting, "p-test"],
                    |r| r.get::<_, String>(0),
                )
                .unwrap(),
            "succeeded"
        );
    }

    #[test]
    fn candidate_batch_releases_db_before_dictionary_retrieval() {
        let dir = tempfile::tempdir().unwrap();
        let rt = Runtime::open_with_vault(
            dir.path().into(),
            Arc::new(crate::models::tests::MemoryVault::default()),
        )
        .unwrap();
        let meeting = rt.create("校准锁回归", "general").unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned();
        let segment = id();
        {
            let db = rt.db().unwrap();
            db.execute(
                "UPDATE meetings SET state='completed',analysis_mode='local',context_consent=1,calibration_enabled=1,recording_duration_ms=1000 WHERE id=?",
                [&meeting],
            )
            .unwrap();
            db.execute(
                "INSERT INTO transcript_segments VALUES (?,?,0,1000,'mixed',1,'final')",
                rusqlite::params![segment, meeting],
            )
            .unwrap();
            db.execute(
                "INSERT INTO transcript_revisions VALUES (?,1,?,'这是用于校准锁回归的已确认文字。','asr',?)",
                rusqlite::params![segment, meeting, now()],
            )
            .unwrap();
            db.execute("INSERT INTO asr_chunks VALUES (?,0,1000)", [&meeting])
                .unwrap();
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let worker_rt = rt.clone();
        std::thread::spawn(move || {
            let result =
                candidate_batch_with_dictionary(&worker_rt, &meeting, "standard", 100 * 1024);
            tx.send(result.is_ok()).unwrap();
        });
        assert!(rx.recv_timeout(Duration::from_secs(2)).unwrap());
    }

    #[test]
    fn failed_meeting_does_not_starve_later_calibration() {
        let dir = tempfile::tempdir().unwrap();
        let rt = Runtime::open_with_vault(
            dir.path().into(),
            Arc::new(crate::models::tests::MemoryVault::default()),
        )
        .unwrap();
        let meetings = [
            rt.create("第一个校准失败", "general").unwrap()["id"]
                .as_str()
                .unwrap()
                .to_owned(),
            rt.create("第二个校准继续", "general").unwrap()["id"]
                .as_str()
                .unwrap()
                .to_owned(),
        ];
        for meeting in &meetings {
            let first = id();
            let second = id();
            let db = rt.db().unwrap();
            db.execute("UPDATE meetings SET state='completed',analysis_mode='remote',context_consent=1,calibration_enabled=1,calibration_mode='low_latency',recording_duration_ms=11000 WHERE id=?", [meeting]).unwrap();
            for (segment, start, end, text) in [
                (&first, 0_u64, 1000_u64, "第一段已确认文字"),
                (&second, 10000_u64, 11000_u64, "第二段已确认文字"),
            ] {
                db.execute(
                    "INSERT INTO transcript_segments VALUES (?,?,?,?,'mixed',1,'final')",
                    params![segment, meeting, start, end],
                )
                .unwrap();
                db.execute(
                    "INSERT INTO transcript_revisions VALUES (?,1,?,?,'asr',?)",
                    params![segment, meeting, text, now()],
                )
                .unwrap();
            }
            db.execute("INSERT INTO asr_chunks VALUES (?,0,11000)", [meeting])
                .unwrap();
        }

        tick(&rt).unwrap();
        let db = rt.db().unwrap();
        for meeting in &meetings {
            let (count, status): (u64, String) = db
                .query_row(
                    "SELECT count(*),coalesce(max(status),'') FROM transcript_calibrations WHERE meeting_id=?",
                    [meeting],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .unwrap();
            assert!(count > 0, "meeting {meeting} was never attempted");
            assert_eq!(status, "failed");
        }
    }
}
