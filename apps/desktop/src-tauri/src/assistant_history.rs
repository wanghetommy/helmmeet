//! Full chat history index and rolling continuity checkpoints.
//! The index is derived; assistant_turns remains the source of truth.
use crate::{
    assistant::{begin_budget, run_pi, PiInput},
    config::ModelConfig,
    db::{err, id, now, Runtime},
};
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};
use std::sync::Arc;

type Checkpoint = (String, Option<String>, u64, String, Option<String>);

fn answer_text(raw: &str) -> String {
    serde_json::from_str::<Value>(raw)
        .ok()
        .and_then(|v| {
            v["blocks"].as_array().map(|blocks| {
                blocks
                    .iter()
                    .filter_map(|b| b["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("\n")
            })
        })
        .unwrap_or_else(|| raw.to_owned())
}

pub fn sync_turn(rt: &Runtime, turn_id: &str) -> Result<(), String> {
    let db = rt.db()?;
    let (cid, epoch, question, response, created): (String, u64, String, String, String) = db
        .query_row(
            "SELECT conversation_id,epoch,question,coalesce(response_json,''),created_at
             FROM assistant_turns WHERE id=? AND state='succeeded'",
            [turn_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .map_err(err)?;
    let answer = answer_text(&response);
    db.execute(
        "INSERT INTO assistant_history_index(conversation_id,epoch,turn_id,question,answer,search_text,created_at)
         VALUES (?,?,?,?,?,?,?)
         ON CONFLICT(turn_id) DO UPDATE SET epoch=excluded.epoch,question=excluded.question,
         answer=excluded.answer,search_text=excluded.search_text,created_at=excluded.created_at",
        params![
            cid,
            epoch,
            turn_id,
            question,
            answer,
            format!("{} {}", question, answer),
            created
        ],
    )
    .map_err(err)?;
    Ok(())
}

fn indexed_rows(
    rt: &Runtime,
    cid: &str,
    epoch: u64,
) -> Result<Vec<(String, String, String, String)>, String> {
    let db = rt.db()?;
    let mut q = db
        .prepare(
            "SELECT turn_id,question,answer,search_text FROM assistant_history_index
             WHERE conversation_id=? AND epoch=? ORDER BY created_at,turn_id",
        )
        .map_err(err)?;
    let result = q
        .query_map(params![cid, epoch], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })
        .map_err(err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(err);
    result
}

pub fn search(
    rt: &Runtime,
    cid: &str,
    epoch: u64,
    query: &str,
    limit: usize,
) -> Result<Vec<Value>, String> {
    let rows = indexed_rows(rt, cid, epoch)?;
    let docs = rows.iter().map(|r| r.3.as_str()).collect::<Vec<_>>();
    let ranked = meeting_core::retrieval::rank(query, &docs);
    let mut result = vec![];
    for index in ranked.into_iter().take(limit.max(1)) {
        let (turn_id, question, answer, _) = &rows[index];
        let text = format!("用户：{}\n小舵：{}", question, answer);
        result.push(json!({
            "source_id": format!("chat:{turn_id}"),
            "kind": "chat_history",
            "turn_id": turn_id,
            "conversation_id": cid,
            "epoch": epoch,
            "text": text.chars().take(4000).collect::<String>(),
            "question": question,
            "answer": answer,
        }));
    }
    Ok(result)
}

fn rows_since(
    rt: &Runtime,
    cid: &str,
    epoch: u64,
    through: Option<&str>,
) -> Result<Vec<Value>, String> {
    let db = rt.db()?;
    let mut q = db
        .prepare(
            "SELECT id,question,response_json FROM assistant_turns
             WHERE conversation_id=? AND epoch=? AND state='succeeded'
             AND rowid>coalesce((SELECT rowid FROM assistant_turns WHERE id=?),0)
             ORDER BY rowid",
        )
        .map_err(err)?;
    let mut rows = q.query(params![cid, epoch, through]).map_err(err)?;
    let mut result = vec![];
    while let Some(row) = rows.next().map_err(err)? {
        let turn_id: String = row.get(0).map_err(err)?;
        let question: String = row.get(1).map_err(err)?;
        let response: String = row.get(2).map_err(err)?;
        result.push(json!({"turn_id":turn_id,"role":"user","text":question}));
        result.push(json!({"turn_id":turn_id,"role":"assistant","text":answer_text(&response)}));
    }
    Ok(result)
}

pub fn context(
    rt: &Runtime,
    cid: &str,
    epoch: u64,
    model_key: &str,
    query: &str,
) -> Result<Value, String> {
    let checkpoint: Option<Checkpoint> = {
        let db = rt.db()?;
        db.query_row(
            "SELECT summary,through_turn,pending_turns,status,error FROM assistant_history_checkpoints
             WHERE conversation_id=? AND epoch=? AND model_key=? ORDER BY updated_at DESC LIMIT 1",
            params![cid, epoch, model_key],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()
        .map_err(err)?
    };
    let rows = search(rt, cid, epoch, query, 8)?;
    Ok(json!({
        "summary": checkpoint.as_ref().map(|v| v.0.clone()).unwrap_or_default(),
        "through_turn": checkpoint.as_ref().and_then(|v| v.1.clone()),
        "pending_turns": checkpoint.as_ref().map(|v| v.2).unwrap_or(0),
        "status": checkpoint.as_ref().map(|v| v.3.clone()).unwrap_or("pending".into()),
        "error": checkpoint.as_ref().and_then(|v| v.4.clone()),
        "rows": rows,
        "partial": checkpoint.is_none() || checkpoint.as_ref().is_some_and(|v| v.2 > 0),
    }))
}

fn pending_count(
    rt: &Runtime,
    cid: &str,
    epoch: u64,
    through: Option<&str>,
) -> Result<(usize, usize), String> {
    let db = rt.db()?;
    db.query_row(
        "SELECT count(*),coalesce(sum(length(question)+length(answer)),0)
         FROM assistant_history_index i WHERE i.conversation_id=? AND i.epoch=?
         AND i.rowid>coalesce((SELECT rowid FROM assistant_history_index WHERE turn_id=?),0)",
        params![cid, epoch, through],
        |r| Ok((r.get::<_, usize>(0)?, r.get::<_, usize>(1)?)),
    )
    .map_err(err)
}

pub fn schedule(
    rt: Arc<Runtime>,
    cid: &str,
    epoch: u64,
    config: &ModelConfig,
    snapshot: &Value,
) -> Result<(), String> {
    // Unit tests use bounded fake HTTP servers that intentionally model only
    // the foreground answer. Production schedules the checkpoint worker below;
    // tests cover indexing and persistence without launching a second agent.
    if cfg!(test) {
        return Ok(());
    }
    let (through, status): (Option<String>, String) = rt
        .db()?
        .query_row(
            "SELECT through_turn,status FROM assistant_history_checkpoints WHERE conversation_id=? AND epoch=? AND model_key=?",
            params![cid, epoch, crate::analysis::target(config)],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map(|v| v.unwrap_or((None, "pending".into())))
        .map_err(err)?;
    if status == "running" || (status == "pending" && through.is_some()) {
        return Ok(());
    }
    let (count, bytes) = pending_count(&rt, cid, epoch, through.as_deref())?;
    if count < 4 && bytes < 6000 {
        return Ok(());
    }
    let model_key = crate::analysis::target(config);
    let mut db = rt.db()?;
    let tx = db.transaction().map_err(err)?;
    tx.execute(
        "INSERT INTO assistant_history_checkpoints(conversation_id,epoch,model_key,through_turn,summary,pending_turns,status,error,updated_at)
         VALUES (?,?,?,NULL,'',?,'running',NULL,?)
         ON CONFLICT(conversation_id,epoch,model_key) DO UPDATE SET pending_turns=excluded.pending_turns,status='running',error=NULL,updated_at=excluded.updated_at",
        params![cid, epoch, model_key, count, now()],
    )
    .map_err(err)?;
    tx.commit().map_err(err)?;
    let cid = cid.to_owned();
    let config = config.clone();
    let snapshot = snapshot.clone();
    let worker_rt = rt.clone();
    let task = id();
    let handle = std::thread::spawn(move || {
        begin_budget(&task, 2);
        let result = (|| -> Result<(), String> {
            let epoch: u64 = worker_rt
                .db()?
                .query_row(
                    "SELECT epoch FROM assistant_conversations WHERE id=?",
                    [&cid],
                    |r| r.get(0),
                )
                .map_err(err)?;
            let checkpoint: Option<(String, Option<String>)> = worker_rt
                .db()?
                .query_row(
                    "SELECT summary,through_turn FROM assistant_history_checkpoints WHERE conversation_id=? AND epoch=? AND model_key=?",
                    params![cid, epoch, crate::analysis::target(&config)],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()
                .map_err(err)?;
            let through = checkpoint.as_ref().and_then(|v| v.1.as_deref());
            let history = rows_since(&worker_rt, &cid, epoch, through)?;
            if history.is_empty() {
                worker_rt.db()?.execute(
                    "UPDATE assistant_history_checkpoints SET status='succeeded',pending_turns=0,error=NULL,updated_at=? WHERE conversation_id=? AND epoch=? AND model_key=?",
                    params![now(), cid, epoch, crate::analysis::target(&config)],
                ).map_err(err)?;
                return Ok(());
            }
            let check = || {
                worker_rt.ensure_running()?;
                crate::models::load(&worker_rt, &config.profile_id, config.config_version)
                    .map(|_| ())
            };
            let compact = run_pi(
                &worker_rt,
                &task,
                &config,
                PiInput {
                    question: "",
                    history: &history,
                    snapshot: &snapshot,
                    checkpoint: &Value::Null,
                    compact_only: true,
                },
                &check,
            )?;
            let summary = compact["summary"].as_str().ok_or("聊天摘要缺失")?;
            let through = history
                .iter()
                .rev()
                .find_map(|v| v["turn_id"].as_str())
                .ok_or("聊天摘要边界缺失")?;
            let mut db = worker_rt.db()?;
            let tx = db.transaction().map_err(err)?;
            if let Some(mid) = snapshot["context"]["meeting_id"].as_str() {
                let current: (u64, u64) = tx
                    .query_row(
                        "SELECT generation,consent_version FROM meetings WHERE id=?",
                        [mid],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .map_err(err)?;
                if snapshot["context"]["generation"].as_u64() != Some(current.0)
                    || snapshot["context"]["consent_version"].as_u64() != Some(current.1)
                {
                    return Err("会议范围已变化，聊天摘要不写回旧上下文".into());
                }
            }
            tx.execute(
                "UPDATE assistant_history_checkpoints SET through_turn=?,summary=?,pending_turns=0,status='succeeded',error=NULL,updated_at=? WHERE conversation_id=? AND epoch=? AND model_key=?",
                params![through, summary, now(), cid, epoch, crate::analysis::target(&config)],
            )
            .map_err(err)?;
            tx.commit().map_err(err)?;
            Ok(())
        })();
        if let Err(e) = result {
            let _ = worker_rt.db().and_then(|db| {
                db.execute(
                    "UPDATE assistant_history_checkpoints SET status='failed',error=?,updated_at=? WHERE conversation_id=? AND epoch=? AND model_key=?",
                    params![e, now(), cid, epoch, crate::analysis::target(&config)],
                )
                .map_err(err)
            });
        }
    });
    rt.analysis_threads
        .lock()
        .map_err(|_| "任务锁异常")?
        .push(handle);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::tests::MemoryVault;
    #[test]
    fn search_returns_source_registered_history_rows() {
        let root = std::env::temp_dir().join(format!("helmmeet-history-{}", id()));
        let rt = Runtime::open_with_vault(root.clone(), Arc::new(MemoryVault::default())).unwrap();
        let cid = crate::assistant::open(&rt, None, true).unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned();
        let tid = id();
        let config = crate::models::ProfileInput {
            profile_id: None,
            expected_version: None,
            display_name: "模型".into(),
            base_url: "http://127.0.0.1:11434/v1".into(),
            model: "test-model".into(),
            context_tokens: 32768,
            max_output_tokens: 4096,
            timeout_seconds: 10,
            key_action: "clear".into(),
            api_key: String::new(),
        };
        let profile = crate::models::save(&rt, config).unwrap();
        let model = crate::models::load(&rt, profile["profile_id"].as_str().unwrap(), 1).unwrap();
        rt.db().unwrap().execute("INSERT INTO assistant_turns(id,conversation_id,epoch,question,source,state,response_json,model_json,snapshot_json,created_at) VALUES (?,?,0,'验收目标','text','succeeded',?,?,?,?)",params![tid,cid,json!({"blocks":[{"kind":"suggestion","text":"核对范围","sources":[]}]}).to_string(),serde_json::to_string(&model).unwrap(),json!({"context":{"kind":"general","chat_epoch":0}}).to_string(),now()]).unwrap();
        sync_turn(&rt, &tid).unwrap();
        let rows = search(&rt, &cid, 0, "验收", 4).unwrap();
        assert_eq!(rows[0]["source_id"], format!("chat:{tid}"));
        drop(rt);
        let _ = std::fs::remove_dir_all(root);
    }
}
