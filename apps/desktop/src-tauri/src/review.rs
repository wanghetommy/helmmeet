//! Resumable, bounded map/reduce for exhaustive questions. All checkpoints are
//! tied to exact sources, conversation epoch, consent, model and question.
use crate::{
    analysis, assistant,
    config::ModelConfig,
    db::{err, now, Runtime},
};
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashSet;

pub struct Outcome {
    pub notes: Value,
    pub completed: usize,
    pub total: usize,
    pub assembled: bool,
}
fn batches(snapshot: &Value, config: &ModelConfig) -> Result<Vec<Vec<Value>>, String> {
    let limit = (analysis::token_budget(config)? * 3 / 2).clamp(1024, 10 * 1024);
    let mut result = vec![];
    let mut batch = vec![];
    let mut size = 2;
    for row in snapshot["evidence"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|e| e["kind"] == "meeting" && e["assistant_playback_overlap"] != true)
    {
        let seg = json!({"id":row["segment_id"],"revision":row["revision"],"text":row["text"],"start_ms":row["start_ms"]});
        for fragment in analysis::split_segment(&seg, (limit / 2).max(256)) {
            let n = fragment.to_string().len() + 1;
            if size + n > limit && !batch.is_empty() {
                result.push(std::mem::take(&mut batch));
                size = 2;
            }
            size += n;
            batch.push(fragment);
        }
    }
    if !batch.is_empty() {
        result.push(batch);
    }
    Ok(result)
}
pub fn is_large(snapshot: &Value, config: &ModelConfig) -> bool {
    let rows = snapshot["evidence"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|e| e["kind"] == "meeting")
        .map(crate::assistant_context::wire)
        .collect::<Vec<_>>();
    let text = json!(rows).to_string();
    text.len() > 45 * 1024
        || meeting_core::budget::estimate(&text) > analysis::token_budget(config).unwrap_or(0) / 2
}
pub fn initialize(
    rt: &Runtime,
    turn: &str,
    snapshot: &Value,
    config: &ModelConfig,
) -> Result<(), String> {
    let total = batches(snapshot, config)?.len();
    rt.db()?.execute("INSERT OR REPLACE INTO assistant_review_progress(turn_id,total,estimated_calls,through_ms) VALUES (?,?,?,?)",params![turn,total,(total*4+2).min(24),snapshot["context"]["asr_through_ms"].as_u64().unwrap_or(0)]).map_err(err)?;
    Ok(())
}
fn progress(rt: &Runtime, turn: &str, done: usize, phase: &str) -> Result<(), String> {
    rt.db()?.execute("UPDATE assistant_review_progress SET completed=?,model_calls=?,phase=? WHERE turn_id=?",params![done,24usize.saturating_sub(assistant::remaining_models(turn)),phase,turn]).map_err(err)?;
    Ok(())
}
fn known(input: &Value) -> HashSet<(String, u64)> {
    fn walk(v: &Value, into: &mut HashSet<(String, u64)>) {
        match v {
            Value::Array(a) => {
                for v in a {
                    walk(v, into)
                }
            }
            Value::Object(o) => {
                if let (Some(s), Some(r)) = (
                    o.get("segment_id").and_then(Value::as_str),
                    o.get("revision").and_then(Value::as_u64),
                ) {
                    into.insert((s.into(), r));
                }
                for v in o.values() {
                    walk(v, into);
                }
            }
            _ => {}
        }
    }
    let mut out = HashSet::new();
    walk(input, &mut out);
    out
}
fn cached_request(
    rt: &Runtime,
    turn: &str,
    config: &ModelConfig,
    snapshot: &Value,
    question: &str,
    input: Value,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<Option<Value>, String> {
    check()?;
    let db = rt.db()?;
    let (cid, epoch): (String, u64) = db
        .query_row(
            "SELECT conversation_id,epoch FROM assistant_turns WHERE id=?",
            [turn],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(err)?;
    let key = format!(
        "{:x}",
        Sha256::digest(
            json!([
                cid,
                epoch,
                analysis::target(config),
                snapshot["context"]["meeting_id"],
                snapshot["context"]["generation"],
                snapshot["context"]["consent_version"],
                snapshot["context"]["scope"],
                question,
                input
            ])
            .to_string()
            .as_bytes()
        )
    );
    let cached:Option<String>=db.query_row("SELECT content_json FROM assistant_review_parts WHERE cache_key=? AND conversation_id=?",params![key,cid],|r|r.get(0)).optional().map_err(err)?;
    drop(db);
    let known = known(&input);
    if let Some(raw) = cached {
        let value: Value = serde_json::from_str(&raw).map_err(err)?;
        analysis::validate(&value, &known)?;
        return Ok(Some(value));
    }
    // Two spare calls are protected for final answer/repair. Format repair of a
    // map/reduce also counts, so a request starts only with four calls left.
    if assistant::remaining_models(turn) < 4 {
        return Ok(None);
    }
    let mut selected = config.clone();
    selected.max_output_tokens = selected.max_output_tokens.min(1024);
    let value = assistant::with_model(rt, turn, check, || {
        analysis::grounded_request(rt, check, &selected, input, &known, &|| {
            assistant::spend_model(turn)
        })
    })?;
    check()?;
    let _gate = rt.model_gate.lock().map_err(|_| "模型锁异常")?;
    check()?;
    let mut db = rt.db()?;
    let tx = db.transaction().map_err(err)?;
    let mid = snapshot["context"]["meeting_id"]
        .as_str()
        .ok_or("整场审阅需要关联会议")?;
    let valid:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM meetings m JOIN assistant_conversations c ON c.meeting_id=m.id JOIN assistant_turns t ON t.conversation_id=c.id AND t.epoch=c.epoch WHERE m.id=? AND m.generation=? AND m.consent_version=? AND m.state!='deleting' AND t.id=? AND t.state='running')",params![mid,snapshot["context"]["generation"].as_u64(),snapshot["context"]["consent_version"].as_u64(),turn],|r|r.get(0)).map_err(err)?;
    if !valid {
        return Err("会议或会话已变化，审阅检查点不写回".into());
    }
    tx.execute(
        "INSERT OR IGNORE INTO assistant_review_parts VALUES (?,?,?,?,?)",
        params![key, mid, cid, value.to_string(), now()],
    )
    .map_err(err)?;
    tx.commit().map_err(err)?;
    Ok(Some(value))
}
pub fn run(
    rt: &Runtime,
    turn: &str,
    config: &ModelConfig,
    snapshot: &Value,
    question: &str,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<Outcome, String> {
    let batches = batches(snapshot, config)?;
    let total = batches.len();
    let mut parts = vec![];
    let mut fresh = 0;
    let before = assistant::remaining_models(turn);
    for batch in batches {
        progress(rt, turn, parts.len(), "reading")?;
        let input = json!({"task_type":"answer","question":question,"meeting_rules":snapshot["context"]["behavior"]["meeting_template"],"transcript":batch,"instruction":"这是整场审阅的一个原文批次，仅记录与当前问题有关的事实/决定/风险/未决事项，保留否定和改口；用第一人称简洁表达，不展示过程。无相关内容summary=[]。每批最多6项，保留各自原文引用，不猜测负责人、数字或共识；section=answer。"});
        let calls = assistant::remaining_models(turn);
        let Some(value) = cached_request(rt, turn, config, snapshot, question, input, check)?
        else {
            break;
        };
        if assistant::remaining_models(turn) < calls {
            fresh += 1;
        }
        parts.push(value);
        if fresh >= 4 {
            break;
        }
    }
    let completed = parts.len();
    let mut assembled = true;
    while parts.len() > 1 {
        let mut reduced = vec![];
        let mut changed = false;
        for pair in parts.chunks(2) {
            if pair.len() == 1 {
                reduced.push(pair[0].clone());
                continue;
            }
            progress(rt, turn, completed, "merging")?;
            let input = json!({"task_type":"answer","question":question,"meeting_rules":snapshot["context"]["behavior"]["meeting_template"],"parts":pair,"instruction":"合并整场分批审阅结果，保留开头目标、后续调整、冲突和最终状态，区分建议与决定；同一事实引用不超过32条，必要时拆分。简洁回答当前问题，不展示思考过程，不补造事实，section=answer，总计不超过1000中文字。"});
            if let Some(value) = cached_request(rt, turn, config, snapshot, question, input, check)?
            {
                reduced.push(value);
                changed = true;
            } else {
                reduced.extend_from_slice(pair);
                assembled = false;
            }
        }
        parts = reduced;
        if !changed {
            break;
        }
    }
    let mut notes = json!({"summary":[],"actions":[],"reviewed_batches":completed,"total_batches":total,"assembled":assembled,"complete":completed==total && assembled,"notice":"只有本次实际审阅批次可作依据；处理完所有原文不等于保留所有细节。未完成时明确范围，可以继续审阅。"});
    for part in parts {
        for item in part["summary"].as_array().into_iter().flatten() {
            notes["summary"].as_array_mut().unwrap().push(item.clone());
            if notes.to_string().len() > 10 * 1024 {
                notes["summary"].as_array_mut().unwrap().pop();
                notes["assembled"] = json!(false);
                notes["complete"] = json!(false);
                assembled = false;
                break;
            }
        }
    }
    let _ = before;
    progress(
        rt,
        turn,
        completed,
        if completed == total && assembled {
            "answering"
        } else {
            "partial"
        },
    )?;
    Ok(Outcome {
        notes,
        completed,
        total,
        assembled,
    })
}
