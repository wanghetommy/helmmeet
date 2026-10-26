//! One conversation engine for typed and transcribed questions. Pi never owns credentials.
use crate::{
    analysis,
    config::ModelConfig,
    db::{err, id, now, Runtime},
    models,
};
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    io::{BufRead, BufReader, Read, Write},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{mpsc, Arc},
    time::{Duration, Instant},
};

thread_local! { static TURN_BUDGET: std::cell::RefCell<(String,usize,usize)> = const { std::cell::RefCell::new((String::new(),0,6)) }; }
pub(crate) fn begin_budget(turn: &str, limit: usize) {
    TURN_BUDGET.with(|b| *b.borrow_mut() = (turn.into(), 0, limit));
}
pub(crate) fn remaining_models(turn: &str) -> usize {
    TURN_BUDGET.with(|b| {
        let mut b = b.borrow_mut();
        if b.0 != turn {
            *b = (turn.into(), 0, 6);
        }
        b.2.saturating_sub(b.1)
    })
}
pub(crate) fn spend_model(turn: &str) -> Result<(), String> {
    if remaining_models(turn) == 0 {
        return Err("本轮模型预算已用完，已保存的审阅进度保留".into());
    }
    TURN_BUDGET.with(|b| b.borrow_mut().1 += 1);
    Ok(())
}

fn valid_uuid(id: &str) -> Result<(), String> {
    uuid::Uuid::parse_str(id)
        .map(|_| ())
        .map_err(|_| "会话/消息ID无效".into())
}

pub const TOOLS: [&str; 5] = [
    "get_context",
    "search_evidence",
    "get_transcript",
    "get_methods",
    "propose_action",
];
pub const METHODS: &str="会议管理：先澄清目的、预期产物、议程和决策方式，区分信息同步、问题解决和决策。目标管理：区分活动与结果，核对指标、基线、期限、责任边界和验收。5Why：从可验证现象逐层提出原因假设、证据和验证动作，不机械凑五问，不把猜测写成根因。金字塔表达：结论先行、同层归类、上下对应。项目管理：核对里程碑、依赖、资源、范围变更、风险和升级条件。经营管理：目标→经营动作→结果指标→复盘，说明效率、质量、成本和收益权衡。";

pub fn settings(rt: &Runtime) -> Result<Value, String> {
    let raw: String = rt
        .db()?
        .query_row(
            "SELECT value FROM app_preferences WHERE key='assistant_settings'",
            [],
            |r| r.get(0),
        )
        .map_err(err)?;
    let mut value: Value = serde_json::from_str(&raw).map_err(err)?;
    value["tts"] = crate::tts::settings(rt)?;
    value["behavior"] = crate::assistant_policy::current(rt)?;
    Ok(value)
}
pub fn save_settings(rt: &Runtime, v: Value) -> Result<Value, String> {
    let word = v["wake_word"].as_str().ok_or("请输入唤醒词")?.trim();
    if !(2..=12).contains(&word.chars().count()) || !word.chars().all(|c| c.is_alphanumeric()) {
        return Err("唤醒词需为2–12个中文或字母数字字符".into());
    }
    crate::voice::keyword(word)?;
    let silence = v["silence_ms"].as_u64().ok_or("停顿设置无效")?;
    if !(800..=2000).contains(&silence) {
        return Err("结束停顿应为800–2000毫秒".into());
    }
    let collections = v["collections"].as_array().ok_or("请选择资料范围")?;
    if collections.len() > 20 {
        return Err("最多选20份资料集合".into());
    }
    for c in collections {
        crate::knowledge::get(
            rt,
            c["set_id"].as_str().ok_or("资料ID无效")?,
            Some(
                u32::try_from(c["version"].as_u64().ok_or("资料版本无效")?)
                    .map_err(|_| "资料版本无效")?,
            ),
        )?;
    }
    let value = json!({"voice_enabled":v["voice_enabled"].as_bool().ok_or("语音开关无效")?,"wake_word":word,"silence_ms":silence,"speak_answers":v["speak_answers"].as_bool().ok_or("播报开关无效")?,"collections":collections});
    let tts = if v["tts"].is_object() {
        Some(crate::tts::validate_settings(rt, &v["tts"])?)
    } else {
        None
    };
    let mut db = rt.db()?;
    let tx = db.transaction().map_err(err)?;
    let tts_changed = if let Some(tts) = tts {
        let old: String = tx
            .query_row(
                "SELECT value FROM app_preferences WHERE key='tts_settings'",
                [],
                |r| r.get(0),
            )
            .map_err(err)?;
        let changed = serde_json::from_str::<Value>(&old).map_err(err)? != tts;
        tx.execute(
            "UPDATE app_preferences SET value=? WHERE key='tts_settings'",
            [tts.to_string()],
        )
        .map_err(err)?;
        changed
    } else {
        false
    };
    if v["behavior"].is_object() {
        crate::assistant_policy::save(&tx, &v["behavior"])?;
    }
    let old: String = tx
        .query_row(
            "SELECT value FROM app_preferences WHERE key='assistant_settings'",
            [],
            |r| r.get(0),
        )
        .map_err(err)?;
    let old: Value = serde_json::from_str(&old).map_err(err)?;
    tx.execute(
        "UPDATE app_preferences SET value=? WHERE key='assistant_settings'",
        [value.to_string()],
    )
    .map_err(err)?;
    if old["collections"] != value["collections"] {
        tx.execute("UPDATE assistant_turns SET state='cancelled',error='通用资料范围已变化，请重新提问' WHERE state IN ('pending','running') AND conversation_id IN (SELECT id FROM assistant_conversations WHERE meeting_id IS NULL)",[]).map_err(err)?;
    }
    tx.commit().map_err(err)?;
    drop(db);
    if tts_changed {
        rt.voice.ack.invalidate();
    }
    if old["voice_enabled"] != value["voice_enabled"]
        || old["wake_word"] != value["wake_word"]
        || old["silence_ms"] != value["silence_ms"]
        || old["speak_answers"] != value["speak_answers"]
        || tts_changed
    {
        crate::voice::invalidate(rt);
    }
    let mut result = value;
    result["tts"] = crate::tts::settings(rt)?;
    result["behavior"] = crate::assistant_policy::current(rt)?;
    Ok(result)
}
pub fn open(rt: &Runtime, meeting: Option<String>, general: bool) -> Result<Value, String> {
    let meeting = if general {
        None
    } else if meeting.is_some() {
        meeting
    } else {
        rt.db()?.query_row("SELECT id FROM meetings WHERE state IN ('recording','paused','interrupted') ORDER BY created_at DESC LIMIT 1",[],|r|r.get::<_,String>(0)).optional().map_err(err)?
    };
    if let Some(mid) = &meeting {
        rt.check_id(mid)?;
    }
    let mut db = rt.db()?;
    let existing: Option<String> = if let Some(mid) = &meeting {
        db.query_row(
            "SELECT id FROM assistant_conversations WHERE meeting_id=?",
            [mid],
            |r| r.get(0),
        )
        .optional()
        .map_err(err)?
    } else {
        db.query_row("SELECT id FROM assistant_conversations WHERE meeting_id IS NULL ORDER BY rowid DESC LIMIT 1",[],|r|r.get(0)).optional().map_err(err)?
    };
    let cid = if let Some(cid) = existing {
        cid
    } else {
        let title = if let Some(mid) = &meeting {
            db.query_row("SELECT title FROM meetings WHERE id=?", [mid], |r| {
                r.get::<_, String>(0)
            })
            .map_err(err)?
        } else {
            "通用聊天".into()
        };
        let cid = id();
        let tx = db.transaction().map_err(err)?;
        tx.execute(
            "INSERT INTO assistant_conversations(id,meeting_id,title,created_at) VALUES (?,?,?,?)",
            params![cid, meeting, title, now()],
        )
        .map_err(err)?;
        tx.commit().map_err(err)?;
        cid
    };
    drop(db);
    detail(rt, &cid)
}
pub fn list(rt: &Runtime) -> Result<Value, String> {
    let db = rt.db()?;
    let mut q=db.prepare("SELECT id,meeting_id,title,created_at FROM (SELECT coalesce(c.id,m.id) id,m.id meeting_id,m.title,m.created_at FROM meetings m LEFT JOIN assistant_conversations c ON c.meeting_id=m.id WHERE m.state NOT IN ('ready','deleting') UNION ALL SELECT c.id,c.meeting_id,c.title,c.created_at FROM assistant_conversations c WHERE c.meeting_id IS NULL) ORDER BY created_at DESC LIMIT 80").map_err(err)?;
    let rows=q.query_map([],|r|Ok(json!({"id":r.get::<_,String>(0)?,"meeting_id":r.get::<_,Option<String>>(1)?,"title":r.get::<_,String>(2)?,"created_at":r.get::<_,String>(3)?}))).map_err(err)?.collect::<Result<Vec<_>,_>>().map_err(err)?;
    Ok(json!(rows))
}
pub fn detail(rt: &Runtime, cid: &str) -> Result<Value, String> {
    valid_uuid(cid)?;
    let db = rt.db()?;
    let (mid,title,epoch):(Option<String>,String,u64)=db.query_row("SELECT c.meeting_id,coalesce(m.title,c.title),c.epoch FROM assistant_conversations c LEFT JOIN meetings m ON m.id=c.meeting_id WHERE c.id=?",[cid],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).map_err(err)?;
    let mut q=db.prepare("SELECT t.id,t.question,t.source,t.state,t.response_json,t.error,t.created_at,t.model_json,a.content_json,p.completed,p.total,p.model_calls,p.estimated_calls,p.through_ms,p.phase,t.speech_interrupted,t.snapshot_json FROM assistant_turns t LEFT JOIN conversation_turns old ON old.job_id=t.legacy_job_id LEFT JOIN artifacts a ON a.id=old.artifact_id LEFT JOIN assistant_review_progress p ON p.turn_id=t.id WHERE t.conversation_id=? AND t.epoch=? ORDER BY t.rowid DESC LIMIT 120").map_err(err)?;
    let mut turns=q.query_map(params![cid,epoch],|r|{
        let response:Option<String>=r.get(4)?;let legacy:Option<String>=r.get(8)?;
        let result=response.and_then(|s|serde_json::from_str::<Value>(&s).ok()).or_else(||legacy.and_then(|s|serde_json::from_str::<Value>(&s).ok()).map(|v|json!({"legacy":true,"blocks":v["summary"].as_array().into_iter().flatten().map(|s|json!({"kind":"legacy","text":s["text"],"sources":s["evidence"]})).collect::<Vec<_>>(),"evidence":[]})));
        let model:Option<String>=r.get(7)?;let model=model.and_then(|s|serde_json::from_str::<Value>(&s).ok());
        let frozen:Value=r.get::<_,Option<String>>(16)?.and_then(|s|serde_json::from_str::<Value>(&s).ok()).unwrap_or(Value::Null);
        let mut policy=frozen["context"]["behavior"].clone();
        if let Some(mode)=result.as_ref().and_then(|v|v["mode"].as_str()) {if let Some((label,_))=meeting_core::dialogue::answer_mode(mode){policy["actual_mode"]=json!(mode);policy["mode_label"]=json!(label);}}
        Ok(json!({"policy":policy,"id":r.get::<_,String>(0)?,"question":r.get::<_,String>(1)?,"source":r.get::<_,String>(2)?,"state":r.get::<_,String>(3)?,"speech_interrupted":r.get::<_,bool>(15)?,"answer":result,"error":r.get::<_,Option<String>>(5)?,"created_at":r.get::<_,String>(6)?,"model_name":model.as_ref().map(|m|&m["display_name"]),"review":if let Some(done)=r.get::<_,Option<u64>>(9)? {json!({"completed":done,"total":r.get::<_,u64>(10)?,"model_calls":r.get::<_,u64>(11)?,"estimated_calls":r.get::<_,u64>(12)?,"through_ms":r.get::<_,u64>(13)?,"phase":r.get::<_,String>(14)?})} else {Value::Null}}))
    }).map_err(err)?.collect::<Result<Vec<_>,_>>().map_err(err)?;
    turns.reverse();
    if let Some(mid) = mid.as_deref() {
        for turn in &mut turns {
            let answer = &mut turn["answer"];
            if answer["legacy"] == true {
                let mut refs = HashMap::new();
                for block in answer["blocks"].as_array_mut().into_iter().flatten() {
                    let mut keys = vec![];
                    for r in block["sources"].as_array().into_iter().flatten() {
                        if let (Some(sid), Some(rev)) =
                            (r["segment_id"].as_str(), r["revision"].as_u64())
                        {
                            if let Ok((text,start,current))=db.query_row("SELECT r.text,s.start_ms,s.current_revision FROM transcript_revisions r JOIN transcript_segments s ON s.id=r.segment_id AND s.meeting_id=r.meeting_id WHERE r.meeting_id=? AND r.segment_id=? AND r.revision=?",params![mid,sid,rev],|r|Ok((r.get::<_,String>(0)?,r.get::<_,u64>(1)?,r.get::<_,u64>(2)?))){
                                let key=format!("meeting:{sid}:{rev}");keys.push(key.clone());refs.insert(key.clone(),json!({"source_id":key,"kind":"meeting","meeting_id":mid,"segment_id":sid,"revision":rev,"text":text,"start_ms":start,"stale":rev!=current}));
                            }
                        }
                    }
                    block["sources"] = json!(keys);
                }
                answer["evidence"] = json!(refs.into_values().collect::<Vec<_>>());
            }
            for source in answer
                .get_mut("evidence")
                .and_then(Value::as_array_mut)
                .into_iter()
                .flatten()
            {
                if source["kind"] == "meeting" {
                    let current=db.query_row("SELECT current_revision FROM transcript_segments WHERE id=? AND meeting_id=?",params![source["segment_id"].as_str(),mid],|r|r.get::<_,u64>(0)).ok();
                    source["stale"] = json!(current != source["revision"].as_u64());
                }
            }
        }
    }

    let mut q=db.prepare("SELECT p.id,p.turn_id,p.payload_json,p.state,p.result_json FROM assistant_proposals p JOIN assistant_turns t ON t.id=p.turn_id WHERE t.conversation_id=? AND t.epoch=? ORDER BY p.rowid").map_err(err)?;
    let proposals=q.query_map(params![cid,epoch],|r|Ok(json!({"id":r.get::<_,String>(0)?,"turn_id":r.get::<_,String>(1)?,"payload":serde_json::from_str::<Value>(&r.get::<_,String>(2)?).unwrap_or(Value::Null),"state":r.get::<_,String>(3)?,"result":r.get::<_,Option<String>>(4)?.and_then(|s|serde_json::from_str::<Value>(&s).ok())}))).map_err(err)?.collect::<Result<Vec<_>,_>>().map_err(err)?;
    let mut template = if let Some(mid) = mid.as_deref() {
        crate::templates::bound(&db, mid)?
    } else {
        Value::Null
    };
    if let Some(key) = template["id"].as_str() {
        let latest: u64 = db
            .query_row(
                "SELECT version FROM assistant_templates WHERE id=?",
                [key],
                |r| r.get(0),
            )
            .map_err(err)?;
        template["latest_version"] = json!(latest);
    }
    let meeting_version = if let Some(mid) = mid.as_deref() {
        db.query_row("SELECT version FROM meetings WHERE id=?", [mid], |r| {
            r.get::<_, u64>(0)
        })
        .map_err(err)?
    } else {
        0
    };
    Ok(
        json!({"id":cid,"meeting_id":mid,"title":title,"turns":turns,"proposals":proposals,"template":template,"meeting_version":meeting_version}),
    )
}
pub fn clear(rt: &Runtime, cid: &str) -> Result<Value, String> {
    valid_uuid(cid)?;
    let mut db = rt.db()?;
    let tx = db.transaction().map_err(err)?;
    tx.execute("UPDATE assistant_turns SET state='cancelled',error='已开始新对话' WHERE conversation_id=? AND state IN ('pending','running')",[cid]).map_err(err)?;
    tx.execute(
        "UPDATE assistant_conversations SET epoch=epoch+1 WHERE id=?",
        [cid],
    )
    .map_err(err)?;
    tx.commit().map_err(err)?;
    Ok(json!({"cleared":true}))
}
pub fn cancel(rt: &Runtime, turn: &str) -> Result<Value, String> {
    valid_uuid(turn)?;
    rt.db()?.execute("UPDATE assistant_turns SET state='cancelled',error='用户取消回答' WHERE id=? AND state IN ('pending','running')",[turn]).map_err(err)?;
    Ok(json!({"cancelled":true}))
}

pub fn accept(rt: &Runtime, pid: &str) -> Result<Value, String> {
    valid_uuid(pid)?;
    rt.ensure_running()?;
    let _gate = rt.import_gate.lock().map_err(|_| "操作锁异常")?;
    let (raw,state,result):(String,String,Option<String>)=rt.db()?.query_row("SELECT p.payload_json,p.state,p.result_json FROM assistant_proposals p JOIN assistant_turns t ON t.id=p.turn_id JOIN assistant_conversations c ON c.id=t.conversation_id AND c.epoch=t.epoch WHERE p.id=? AND t.state='succeeded'",[pid],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).map_err(|_|"草稿尚未完成、会话已变化或已删除")?;
    if state == "accepted" {
        return result
            .and_then(|s| serde_json::from_str(&s).ok())
            .ok_or("操作收据不可读".into());
    }
    if state != "pending" {
        return Err("草稿已关闭".into());
    }
    let payload: Value = serde_json::from_str(&raw).map_err(err)?;
    let result = match payload["action"].as_str() {
        Some("prepare_meeting") => {
            let title = payload["title"].as_str().unwrap_or("新会议");
            let kind = payload["meeting_type"].as_str().unwrap_or("general");
            rt.create_with_id(pid, title, kind)?;
            let context = json!({"agenda":payload["text"],"goal":"","participants":"","terms":""});
            // A retry after a crash can fill only the untouched initial draft.
            rt.db()?.execute("UPDATE meetings SET context_json=? WHERE id=? AND state='ready' AND context_json='{}'",params![context.to_string(),pid]).map_err(err)?;
            json!({"meeting_id":pid,"state":"ready"})
        }
        Some("note") => {
            let mid = payload["meeting_id"].as_str().ok_or("笔记会议无效")?;
            rt.check_id(mid)?;
            let d = rt.detail(mid)?;
            if payload["meeting_generation"] != d["generation"] {
                return Err("会议已修订，请重新生成笔记草稿".into());
            }
            rt.db()?
                .execute(
                    "INSERT OR IGNORE INTO user_notes VALUES (?,?,?,?)",
                    params![
                        pid,
                        mid,
                        d["duration_ms"].as_u64().unwrap_or(0),
                        payload["text"].as_str().unwrap_or("")
                    ],
                )
                .map_err(err)?;
            json!({"saved":true})
        }
        Some("todo") => {
            let mid = payload["meeting_id"].as_str().ok_or("待办会议无效")?;
            let task = payload["text"]
                .as_str()
                .filter(|s| !s.trim().is_empty() && s.len() <= 6000)
                .ok_or("待办内容无效或过长")?;
            let mut db = rt.db()?;
            let tx = db.transaction().map_err(err)?;
            let generation: u64 = tx
                .query_row(
                    "SELECT generation FROM meetings WHERE id=? AND state!='deleting'",
                    [mid],
                    |r| r.get(0),
                )
                .map_err(|_| "会议已删除")?;
            if payload["meeting_generation"].as_u64() != Some(generation) {
                return Err("会议已修订，请重新生成待办草稿".into());
            }
            let changes = json!({"task":task,"owner_text":null,"due_text":null,"confirmation_state":"confirmed","execution_state":"todo","assistant_proposal_id":pid});
            tx.execute("INSERT OR IGNORE INTO action_items(id,meeting_id,task,confirmation_state,locked_fields_json) VALUES (?,?,?,'confirmed','[\"task\",\"owner_text\",\"due_text\"]')",params![pid,mid,task]).map_err(err)?;
            tx.execute(
                "INSERT OR IGNORE INTO action_revisions VALUES (?,1,'human',?,'[]',?)",
                params![pid, changes.to_string(), now()],
            )
            .map_err(err)?;
            tx.commit().map_err(err)?;
            json!({"saved":true,"action_id":pid})
        }
        _ => return Err("操作类型无效".into()),
    };
    rt.db()?
        .execute(
            "UPDATE assistant_proposals SET state='accepted',result_json=? WHERE id=?",
            params![result.to_string(), pid],
        )
        .map_err(err)?;
    Ok(result)
}

fn configuration(rt: &Runtime, mid: Option<&str>) -> Result<ModelConfig, String> {
    if let Some(mid) = mid {
        let c = models::selected(rt, mid)?;
        let db = rt.db()?;
        let (mode, target): (String, Option<String>) = db
            .query_row(
                "SELECT analysis_mode,consent_target FROM meetings WHERE id=?",
                [mid],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(err)?;
        let (_, local) = c.endpoint()?;
        if mode == "transcript_only"
            || (mode == "local" && !local)
            || target.as_deref() != Some(&analysis::target(&c))
        {
            return Err("本场分析未启用或模型授权已变化，请在会议设置核对".into());
        }
        Ok(c)
    } else {
        let dir = models::list(rt)?;
        let p = dir["profiles"]
            .as_array()
            .and_then(|a| {
                a.iter().find(|p| {
                    p["profile_id"] == dir["default_profile_id"]
                        && !p["archived"].as_bool().unwrap_or(true)
                })
            })
            .ok_or("请先在全局设置配置默认文本模型")?;
        models::load(
            rt,
            p["profile_id"].as_str().unwrap_or(""),
            p["config_version"].as_u64().unwrap_or(0) as u32,
        )
    }
}
fn user_records(detail: &Value) -> Value {
    let mut result = json!({"notes":[],"actions":[],"partial":false});
    let mut budget = 12000usize;
    for (key, limit) in [("notes", 20), ("actions", 30)] {
        let rows = detail[key].as_array().map(Vec::as_slice).unwrap_or(&[]);
        for row in rows.iter().rev().take(limit) {
            let text = row[if key == "notes" { "text" } else { "task" }]
                .as_str()
                .unwrap_or("");
            let version = row["version"].as_u64().unwrap_or(1);
            let record = json!({"source_id":format!("record:{}:{}:{version}",key,row["id"].as_str().unwrap_or("")),"kind":"record","name":if key=="notes" {"用户笔记"} else {"本场待办记录"},"meeting_id":detail["id"],"text":text,"version":version,"owner_text":row["owner_text"],"due_text":row["due_text"],"execution_state":row["execution_state"],"confirmation_state":row["confirmation_state"]});
            let size = record.to_string().len();
            if size > budget {
                result["partial"] = json!(true);
                continue;
            }
            budget -= size;
            result[key].as_array_mut().unwrap().push(record);
        }
        if rows.len() > limit {
            result["partial"] = json!(true);
        }
    }
    result
}
fn snapshot(rt: &Runtime, mid: Option<&str>, scope: &str) -> Result<Value, String> {
    let mut evidence = vec![];
    let context = if let Some(mid) = mid {
        let d = rt.detail(mid)?;
        let paragraph_ids = d["paragraphs"]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|p| {
                p["segment_ids"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|id| Some((id.as_str()?.to_owned(), p["id"].as_str()?.to_owned())))
            })
            .collect::<HashMap<_, _>>();
        if d["state"] == "ready" {
            return Err("会议尚未开始，请在通用聊天中准备会议".into());
        }
        let cutoff = d["duration_ms"]
            .as_u64()
            .unwrap_or(0)
            .saturating_sub(600000);
        let selected = scope.strip_prefix("selection:");
        let selected_paragraph = selected
            .and_then(|id| d["paragraphs"].as_array()?.iter().find(|p| p["id"] == id))
            .and_then(|p| p["segment_ids"].as_array());
        for s in d["segments"].as_array().into_iter().flatten() {
            if (scope == "recent" && s["end_ms"].as_u64().unwrap_or(0) <= cutoff)
                || selected.is_some_and(|id| {
                    selected_paragraph
                        .map_or(s["id"].as_str() != Some(id), |ids| !ids.contains(&s["id"]))
                })
            {
                continue;
            }
            if !matches!(s["state"].as_str(), Some("final" | "edited")) {
                continue;
            }
            let overlap = s["state"] != "edited"
                && crate::voice::playback_overlap(
                    rt,
                    mid,
                    s["start_ms"].as_u64().unwrap_or(0),
                    s["end_ms"].as_u64().unwrap_or(0),
                )?;
            evidence.push(json!({"paragraph_id":paragraph_ids.get(s["id"].as_str().unwrap_or("")),"assistant_playback_overlap":overlap,"source_id":format!("meeting:{}:{}",s["id"].as_str().unwrap_or(""),s["revision"]),"kind":"meeting","text":s["text"],"meeting_id":mid,"segment_id":s["id"],"revision":s["revision"],"start_ms":s["start_ms"]}));
        }
        json!({"kind":"meeting","scope":scope,"context_allowed":d["context_consent"],"meeting_id":mid,"title":d["title"],"state":d["state"],"context":if d["context_consent"]==true { d["context"].clone() } else { Value::Null },"user_records":if d["context_consent"]==true {user_records(&d)} else {Value::Null},"meeting_memory":if d["context_consent"]==true {crate::memory::context(rt,mid,scope)?} else {Value::Null},"template":d["template"],"duration_ms":d["duration_ms"],"asr_through_ms":d["asr_processed_ms"],"recording_through_ms":d["duration_ms"],"asr_missing_ranges":d["coverage"].as_array().into_iter().flatten().filter(|v|v["state"]=="missing" || v["state"]=="failed").count(),"generation":d["generation"],"chat_epoch":d["chat_epoch"],"consent_version":d["consent_version"]})
    } else {
        json!({"kind":"general","title":"通用聊天","knowledge_scope":settings(rt)?["collections"]})
    };
    let collections = if let Some(mid) = mid {
        if context["context_allowed"] == true {
            crate::knowledge::bound(rt, mid)?
        } else {
            json!([])
        }
    } else {
        settings(rt)?["collections"].clone()
    };
    for c in collections.as_array().into_iter().flatten() {
        let sid = c["set_id"].as_str().unwrap_or("");
        let version = c["version"].as_u64().unwrap_or(0) as u32;
        let collection = crate::knowledge::get(rt, sid, Some(version))?;
        let name = collection["name"].clone();
        for (i, term) in collection["terms"]
            .as_array()
            .into_iter()
            .flatten()
            .enumerate()
        {
            evidence.push(json!({"source_id":format!("knowledge:{sid}:{version}:term-{i}"),"kind":"knowledge","text":format!("标准名称：{}；别名：{}",term["canonical"].as_str().unwrap_or(""),term["aliases"]),"set_id":sid,"version":version,"name":name,"canonical":term["canonical"],"aliases":term["aliases"]}));
        }
        let db = rt.db()?;
        let mut q=db.prepare("SELECT id,text FROM knowledge_chunks WHERE set_id=? AND version=? ORDER BY ordinal").map_err(err)?;
        let rows = q
            .query_map(params![sid, version], |r| {
                Ok((r.get::<_, u64>(0)?, r.get::<_, String>(1)?))
            })
            .map_err(err)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(err)?;
        for (chunk, text) in rows {
            evidence.push(json!({"source_id":format!("knowledge:{sid}:{version}:{chunk}"),"kind":"knowledge","text":text,"set_id":sid,"version":version,"name":name,"chunk_id":chunk}));
        }
    }
    for key in ["notes", "actions"] {
        evidence.extend(
            context["user_records"][key]
                .as_array()
                .into_iter()
                .flatten()
                .cloned(),
        );
    }
    evidence.push(json!({"source_id":"method:duozhou-v1","kind":"method","name":"小舵会议方法 v1","text":METHODS}));
    if evidence.len() > 30000
        || evidence
            .iter()
            .map(|e| e["text"].as_str().unwrap_or("").len())
            .sum::<usize>()
            > 20_000_000
    {
        return Err("资料范围过大，请减少选用集合".into());
    }
    Ok(json!({"context":context,"evidence":evidence}))
}
#[cfg(test)]
pub fn submit(
    rt: Arc<Runtime>,
    cid: &str,
    question: &str,
    source: &str,
    utterance: Option<String>,
) -> Result<Value, String> {
    submit_scoped(rt, cid, question, source, utterance, "all")
}
#[cfg(test)]
pub fn submit_scoped(
    rt: Arc<Runtime>,
    cid: &str,
    question: &str,
    source: &str,
    utterance: Option<String>,
    scope: &str,
) -> Result<Value, String> {
    submit_with_review(rt, cid, question, source, utterance, scope, false)
}
#[cfg(test)]
pub fn submit_with_review(
    rt: Arc<Runtime>,
    cid: &str,
    question: &str,
    source: &str,
    utterance: Option<String>,
    scope: &str,
    review_requested: bool,
) -> Result<Value, String> {
    submit_with_mode(
        rt,
        cid,
        question,
        source,
        utterance,
        scope,
        review_requested,
        "auto",
    )
}
#[allow(clippy::too_many_arguments)]
pub fn submit_with_mode(
    rt: Arc<Runtime>,
    cid: &str,
    question: &str,
    source: &str,
    utterance: Option<String>,
    scope: &str,
    review_requested: bool,
    mode: &str,
) -> Result<Value, String> {
    meeting_core::dialogue::answer_mode(mode).ok_or("问答方式无效")?;
    rt.ensure_running()?;
    valid_uuid(cid)?;
    if !matches!(scope, "all" | "recent")
        && !scope
            .strip_prefix("selection:")
            .is_some_and(|s| valid_uuid(s.strip_prefix("p-").unwrap_or(s)).is_ok())
    {
        return Err("提问范围无效".into());
    }
    if question.trim().is_empty() || question.len() > 12000 || !matches!(source, "text" | "voice") {
        return Err("问题为空或超过长度限制".into());
    }
    if crate::product::globally_paused(&rt)? {
        return Err("全局分析已暂停".into());
    }
    let _gate = rt.model_gate.lock().map_err(|_| "模型锁异常")?;
    let (mid, epoch): (Option<String>, u64) = rt
        .db()?
        .query_row(
            "SELECT meeting_id,epoch FROM assistant_conversations WHERE id=?",
            [cid],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(err)?;
    if source == "voice" {
        crate::voice::ensure_meeting(&rt, mid.as_deref().ok_or("语音只支持正在录制的会议")?)?;
    }
    if let Some(ref u) = utterance {
        valid_uuid(u)?;
        if let Some(existing) = rt
            .db()?
            .query_row(
                "SELECT id FROM assistant_turns WHERE utterance_id=? AND conversation_id=?",
                params![u, cid],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .map_err(err)?
        {
            return Ok(json!({"turn_id":existing}));
        }
    }
    let config = configuration(&rt, mid.as_deref())?;
    let mut snapshot = snapshot(&rt, mid.as_deref(), scope)?;
    snapshot["context"]["conversation_id"] = json!(cid);
    snapshot["context"]["behavior"] =
        crate::assistant_policy::freeze_with_mode(&rt, &snapshot["context"], mode)?;
    if review_requested && (mid.is_none() || scope != "all") {
        return Err("整场审阅需要选择本场全部原文".into());
    }
    let reviewing = mid.is_some()
        && scope == "all"
        && (review_requested || meeting_core::retrieval::whole_review(question))
        && crate::review::is_large(&snapshot, &config);
    let turn = id();
    rt.db()?.execute("INSERT INTO assistant_turns(id,conversation_id,epoch,question,source,utterance_id,state,model_json,snapshot_json,created_at) VALUES (?,?,?,?,?,?,'pending',?,?,?)",params![turn,cid,epoch,question.trim(),source,utterance,serde_json::to_string(&config).map_err(err)?,json!({"context":snapshot["context"]}).to_string(),now()]).map_err(|_|"本会话已有回答进行中，请等待或取消")?;
    drop(_gate);
    let task = turn.clone();
    let cid = cid.to_string();
    let question = question.trim().to_string();
    let worker_rt = rt.clone();
    let mut handles = rt.analysis_threads.lock().map_err(|_| "任务锁异常")?;
    rt.ensure_running()?;
    handles.push(std::thread::spawn(move||{
        let began=Instant::now();
        begin_budget(&task,if reviewing {24}else{6});
        let check=||{if began.elapsed()>Duration::from_secs(if reviewing {600}else{180}){return Err("本轮问答及压缩超过总时限，请重试；已成功保存的记忆保留".into());}check_turn(&worker_rt,&task,&config,&snapshot)};
        let result:Result<(),String>=(||{
            check()?;
            worker_rt.db()?.execute("UPDATE assistant_turns SET state='running' WHERE id=? AND state='pending'",[&task]).map_err(err)?;
            if reviewing {crate::review::initialize(&worker_rt,&task,&snapshot,&config)?;}
            let (history,checkpoint)=if snapshot["context"]["context_allowed"]==false { (vec![],Value::Null) } else { remembered_history(&worker_rt,&cid,&task,&config,&question,&snapshot,&check)? };
            let outcome=if reviewing {Some(crate::review::run(&worker_rt,&task,&config,&snapshot,&question,&check)?)}else{None};
            let mut answer_snapshot=snapshot.clone();
            if let Some(outcome)=&outcome {answer_snapshot["review_notes"]=outcome.notes.clone();}
            let mut answer=run_pi(&worker_rt,&task,&config,PiInput{question:&question,history:&history,snapshot:&answer_snapshot,checkpoint:&checkpoint,compact_only:false},&check)?;
            if let Some(outcome)=&outcome {
                answer["coverage"]["basis"]=json!(if outcome.completed==outcome.total && outcome.assembled {"review_complete"}else{"review_partial"});
                answer["coverage"]["review_completed"]=json!(outcome.completed);answer["coverage"]["review_total"]=json!(outcome.total);
            }
            let _gate=worker_rt.model_gate.lock().map_err(|_|"模型锁异常")?;check()?;
            let mut db=worker_rt.db()?;let tx=db.transaction().map_err(err)?;
            if let Some(mid)=snapshot["context"]["meeting_id"].as_str(){let generation:u64=tx.query_row("SELECT generation FROM meetings WHERE id=?",[mid],|r|r.get(0)).map_err(err)?;if Some(generation)!=snapshot["context"]["generation"].as_u64(){return Err("会议内容已变化，请重新提问".into());}}
            let changed=tx.execute("UPDATE assistant_turns SET state='succeeded',response_json=? WHERE id=? AND state='running' AND epoch=(SELECT epoch FROM assistant_conversations WHERE id=conversation_id)",params![answer.to_string(),task]).map_err(err)?;
            if changed!=1{return Err("本次回答已取消".into());}
            if let Some(outcome)=&outcome {tx.execute("UPDATE assistant_review_progress SET model_calls=?,phase=? WHERE turn_id=?",params![24usize.saturating_sub(remaining_models(&task)),if outcome.completed==outcome.total && outcome.assembled {"complete"}else{"partial"},task]).map_err(err)?;}
            tx.commit().map_err(err)?;
            drop(db);
            let _ = crate::assistant_history::sync_turn(&worker_rt, &task);
            let _ = crate::assistant_history::schedule(
                worker_rt.clone(),
                &cid,
                epoch,
                &config,
                &snapshot,
            );
            Ok(())
        })();
        if let Err(e)=result{let _=worker_rt.db().and_then(|db|db.execute("UPDATE assistant_turns SET state='failed',error=? WHERE id=? AND state IN ('pending','running')",params![e,task]).map_err(err));}
    }));
    Ok(json!({"turn_id":turn}))
}
fn check_turn(
    rt: &Runtime,
    turn: &str,
    config: &ModelConfig,
    snapshot: &Value,
) -> Result<(), String> {
    rt.ensure_running()?;
    if crate::product::globally_paused(rt)? {
        return Err("全局分析已暂停".into());
    }
    let live:bool=rt.db()?.query_row("SELECT EXISTS(SELECT 1 FROM assistant_turns t JOIN assistant_conversations c ON c.id=t.conversation_id AND c.epoch=t.epoch WHERE t.id=? AND t.state IN ('pending','running'))",[turn],|r|r.get(0)).map_err(err)?;
    if !live {
        return Err("本次回答已取消".into());
    }
    models::load(rt, &config.profile_id, config.config_version)?;
    if snapshot["context"]["kind"] == "general"
        && settings(rt)?["collections"] != snapshot["context"]["knowledge_scope"]
    {
        return Err("通用资料范围已变化".into());
    }
    if let Some(mid) = snapshot["context"]["meeting_id"].as_str() {
        if analysis::target(&configuration(rt, Some(mid))?) != analysis::target(config) {
            return Err("会议模型已变化".into());
        }
        let generation: u64 = rt
            .db()?
            .query_row("SELECT generation FROM meetings WHERE id=?", [mid], |r| {
                r.get(0)
            })
            .map_err(err)?;
        if snapshot["context"]["generation"].as_u64() != Some(generation) {
            return Err("会议或资料已修订，请重新提问".into());
        }
    }
    Ok(())
}
fn context_key(context: &Value) -> String {
    use sha2::{Digest, Sha256};
    format!(
        "{:x}",
        Sha256::digest(
            json!([
                context["kind"],
                context["meeting_id"],
                context["scope"],
                context["generation"],
                context["chat_epoch"],
                context["consent_version"],
                context["knowledge_scope"]
            ])
            .to_string()
        )
    )
}
#[cfg(test)]
fn history(rt: &Runtime, cid: &str, turn: &str, context: &Value) -> Result<Vec<Value>, String> {
    Ok(history_page(rt, cid, turn, context, None)?.0)
}
fn history_page(
    rt: &Runtime,
    cid: &str,
    turn: &str,
    context: &Value,
    through: Option<&str>,
) -> Result<(Vec<Value>, bool), String> {
    let db = rt.db()?;
    let mut q=db.prepare("SELECT t.id,t.question,t.response_json,t.snapshot_json,a.content_json FROM assistant_turns t JOIN assistant_conversations c ON c.id=t.conversation_id LEFT JOIN conversation_turns old ON old.job_id=t.legacy_job_id LEFT JOIN artifacts a ON a.id=old.artifact_id LEFT JOIN assistant_review_progress p ON p.turn_id=t.id WHERE t.conversation_id=? AND t.epoch=c.epoch AND t.id!=? AND t.state='succeeded' AND t.model_json IS (SELECT model_json FROM assistant_turns WHERE id=?) AND t.rowid>coalesce((SELECT rowid FROM assistant_turns WHERE id=?),0) ORDER BY t.rowid").map_err(err)?;
    let mut rows = q.query(params![cid, turn, turn, through]).map_err(err)?;
    let mut messages = vec![];
    let mut bytes = 0;
    while let Some(r) = rows.next().map_err(err)? {
        let raw: Option<String> = r.get(3).map_err(err)?;
        let old = raw
            .and_then(|s| serde_json::from_str::<Value>(&s).ok())
            .unwrap_or(Value::Null);
        if context_key(&old["context"]) != context_key(context) {
            continue;
        }
        let id: String = r.get(0).map_err(err)?;
        let question: String = r.get(1).map_err(err)?;
        let response: Option<String> = r.get(2).map_err(err)?;
        let legacy: Option<String> = r.get(4).map_err(err)?;
        let answer = response.or(legacy).unwrap_or_default();
        let n = question.len() + answer.len() + 256;
        if bytes + n > 128 * 1024 && !messages.is_empty() {
            return Ok((messages, true));
        }
        if n > 128 * 1024 {
            return Err("单轮历史超过安全读取预算，原记录保留，请缩小会话范围".into());
        }
        bytes += n;
        messages.push(json!({"turn_id":id,"role":"user","text":question}));
        messages.push(json!({"turn_id":id,"role":"assistant","text":answer}));
    }
    Ok((messages, false))
}
fn remembered_history(
    rt: &Runtime,
    cid: &str,
    turn: &str,
    config: &ModelConfig,
    question: &str,
    snapshot: &Value,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<(Vec<Value>, Value), String> {
    let scope = context_key(&snapshot["context"]);
    let epoch: u64 = rt
        .db()?
        .query_row(
            "SELECT epoch FROM assistant_conversations WHERE id=?",
            [cid],
            |r| r.get(0),
        )
        .map_err(err)?;
    let model = analysis::target(config);
    let stored:Option<(String,String,u64)>=rt.db()?.query_row("SELECT summary,through_turn,tokens_before FROM assistant_compactions WHERE conversation_id=? AND epoch=? AND scope_key=? AND model_key=? ORDER BY rowid DESC LIMIT 1",params![cid,epoch,scope,model],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional().map_err(err)?;
    let mut checkpoint=stored.as_ref().map(|(text,through,tokens)|json!({"summary":text,"through_turn":through,"tokens_before":tokens})).unwrap_or(Value::Null);
    let budget = analysis::input_budget(config)?;
    // Reserve context/tool space, and retain full question/answer pairs.
    let history_budget = (budget.min(analysis::token_budget(config)? * 2) * 7 / 10)
        .saturating_sub(snapshot["context"].to_string().len() + question.len() + 4000)
        .clamp(512, 12_000);
    for _ in 0..3 {
        let (rows, more) = history_page(
            rt,
            cid,
            turn,
            &snapshot["context"],
            checkpoint["through_turn"].as_str(),
        )?;
        if !more
            && rows
                .iter()
                .map(|r| r["text"].as_str().unwrap_or("").len() + 128)
                .sum::<usize>()
                + checkpoint.to_string().len()
                <= history_budget
        {
            return Ok((rows, checkpoint));
        }
        let mut batch = vec![];
        let mut bytes = checkpoint.to_string().len() + 2500;
        let keep_recent_messages = if more { 0 } else { 2 };
        while rows.len().saturating_sub(batch.len()) > keep_recent_messages
            || (!more && !rows.is_empty() && batch.is_empty())
        {
            let pair = &rows[batch.len()..batch.len() + 2];
            let n = pair.iter().map(|r| r.to_string().len()).sum::<usize>();
            if bytes + n > budget.min(32_000).saturating_sub(3000) {
                break;
            }
            bytes += n;
            batch.extend_from_slice(pair);
            if bytes > budget / 2 {
                break;
            }
        }
        if batch.is_empty() {
            // A very large historical turn must not block a new question. The
            // complete chat remains persisted; this turn can still use the
            // frozen meeting ContextPack and current question.
            return Ok((vec![], checkpoint));
        }
        let result = run_pi(
            rt,
            turn,
            config,
            PiInput {
                question: "",
                history: &batch,
                snapshot,
                checkpoint: &checkpoint,
                compact_only: true,
            },
            check,
        );
        let result = match result {
            Ok(v) => v,
            Err(e) => {
                check()?;
                if !more
                    && serde_json::to_vec(&rows).map_err(err)?.len()
                        + checkpoint.to_string().len()
                        + snapshot["context"].to_string().len()
                        + question.len()
                        + 4000
                        < budget
                {
                    return Ok((rows, checkpoint));
                }
                return Err(format!("上下文压缩未完成：{e}；原始记录和已保存摘要保留"));
            }
        };
        let summary = result["summary"].as_str().ok_or("压缩结果缺失")?;
        if summary.is_empty() || summary.len() > 6144 {
            return Err("压缩摘要超过限制，完整历史已保留".into());
        }
        let through = batch.last().unwrap()["turn_id"]
            .as_str()
            .ok_or("压缩边界无效")?;
        check()?;
        let _gate = rt.model_gate.lock().map_err(|_| "模型锁异常")?;
        check()?;
        let mut db = rt.db()?;
        let tx = db.transaction().map_err(err)?;
        let valid:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM assistant_turns t JOIN assistant_conversations c ON c.id=t.conversation_id WHERE t.id=? AND t.state='running' AND t.epoch=? AND c.epoch=t.epoch)",params![turn,epoch],|r|r.get(0)).map_err(err)?;
        if !valid {
            return Err("本轮对话已取消，压缩结果未写回".into());
        }
        if let Some(mid) = snapshot["context"]["meeting_id"].as_str() {
            let valid: bool = tx
                .query_row(
                    "SELECT generation=? AND consent_version=? FROM meetings WHERE id=?",
                    params![
                        snapshot["context"]["generation"].as_u64(),
                        snapshot["context"]["consent_version"].as_u64(),
                        mid
                    ],
                    |r| r.get(0),
                )
                .map_err(err)?;
            if !valid {
                return Err("会议或授权已变化，压缩结果未写回".into());
            }
        }
        tx.execute(
            "INSERT INTO assistant_compactions VALUES (?,?,?,?,?,?,?,?,?,?)",
            params![
                id(),
                cid,
                epoch,
                scope,
                model,
                through,
                summary,
                result["tokens_before"].as_u64().unwrap_or(0),
                result["usage"].to_string(),
                now()
            ],
        )
        .map_err(err)?;
        tx.commit().map_err(err)?;
        checkpoint = json!({"summary":summary,"through_turn":through,"tokens_before":result["tokens_before"]});
    }
    let (rows, more) = history_page(
        rt,
        cid,
        turn,
        &snapshot["context"],
        checkpoint["through_turn"].as_str(),
    )?;
    if !more
        && serde_json::to_vec(&rows).map_err(err)?.len() + checkpoint.to_string().len()
            <= history_budget
    {
        return Ok((rows, checkpoint));
    }
    Err("已保存本轮压缩进度，历史积压仍超预算，请继续重试或缩小对话范围；原记录保留".into())
}
fn runtime_dir() -> PathBuf {
    let packaged = std::env::current_exe().ok().and_then(|p| {
        p.parent()?
            .parent()
            .map(|p| p.join("Resources/agent-runtime"))
    });
    if let Some(p) = packaged.filter(|p| p.join("src/main.mjs").is_file()) {
        return p;
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../.tools/agent-runtime/targets")
        .join(if cfg!(target_arch = "aarch64") {
            "aarch64-apple-darwin"
        } else {
            "x86_64-apple-darwin"
        })
}
struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
#[derive(Clone, Copy)]
pub(crate) struct PiInput<'a> {
    pub question: &'a str,
    pub history: &'a [Value],
    pub snapshot: &'a Value,
    pub checkpoint: &'a Value,
    pub compact_only: bool,
}
pub(crate) fn run_pi(
    rt: &Runtime,
    turn: &str,
    config: &ModelConfig,
    start: PiInput<'_>,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<Value, String> {
    let first = run_pi_once(rt, turn, config, start, check, 1);
    if !start.compact_only
        && first
            .as_ref()
            .is_err_and(|error| adaptive_capacity_error(error))
    {
        check()?;
        return run_pi_once(rt, turn, config, start, check, 2);
    }
    first
}
fn adaptive_capacity_error(error: &str) -> bool {
    [
        "模型上下文容量不足",
        "模型基础上下文不足",
        "模型输出被截断",
        "请求体超过当前模型输入安全预算",
        "工具与会话超过当前模型输入安全预算",
    ]
    .iter()
    .any(|needle| error.contains(needle))
}
fn run_pi_once(
    rt: &Runtime,
    turn: &str,
    config: &ModelConfig,
    start: PiInput<'_>,
    check: &dyn Fn() -> Result<(), String>,
    reduction: usize,
) -> Result<Value, String> {
    let PiInput {
        question,
        history,
        snapshot,
        checkpoint,
        compact_only,
    } = start;
    let directory = runtime_dir()
        .canonicalize()
        .map_err(|_| "小舵运行时未准备，请运行项目Agent准备脚本")?;
    let quote = serde_json::to_string(&directory.to_string_lossy()).map_err(err)?;
    let profile=format!("(version 1)(allow default)(deny network*)(deny process-fork)(deny file-write*)(deny file-read-data (require-all (subpath \"/Users\") (require-not (subpath {quote}))))");
    let mut command = Command::new("/usr/bin/sandbox-exec");
    command
        .args(["-p", &profile])
        .arg(directory.join("bin/node"))
        .arg("--permission")
        .arg(format!("--allow-fs-read={}", directory.display()))
        .arg(directory.join("src/main.mjs"))
        .current_dir(&directory)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", directory.join("empty-home"))
        .env("PI_OFFLINE", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = Process(command.spawn().map_err(|_| "小舵隔离运行时无法启动")?);
    let mut input = child.0.stdin.take().ok_or("Agent输入不可用")?;
    // Expression policy belongs in the system prompt once, not repeated in
    // scope, context_pack and get_context where it would displace evidence.
    let mut scope = snapshot["context"].clone();
    if let Some(context) = scope.as_object_mut() {
        context.remove("behavior");
    }
    let assistant_name = match snapshot["context"]["behavior"]["assistant_name"].as_str() {
        Some(name) => name.to_owned(),
        None => settings(rt)?["wake_word"]
            .as_str()
            .unwrap_or("小舵同学")
            .to_owned(),
    };
    writeln!(input,"{}",json!({"type":"start","assistant_name":assistant_name,"behavior":snapshot["context"]["behavior"],"question":question,"scope":scope,"history":history,"context_tokens":config.context_tokens,"max_tokens":config.max_output_tokens,"checkpoint":checkpoint,"compact_only":compact_only,"model_budget":remaining_models(turn)})).map_err(|_|"Agent启动消息失败")?;
    let stdout = child.0.stdout.take().ok_or("Agent输出不可用")?;
    let (tx, rx) = mpsc::sync_channel(4);
    let reader = std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        loop {
            let mut line = String::new();
            match reader.by_ref().take(2_000_001).read_line(&mut line) {
                Ok(n) if n > 0 && n <= 2_000_000 => {
                    if tx.send(line).is_err() {
                        break;
                    }
                }
                _ => break,
            }
        }
    });
    let started = Instant::now();
    let mut calls = 0;
    let mut searches = 0;
    let mut models = 0;
    let mut seen = HashMap::new();
    let mut prepared: Option<Value> = None;
    let mut validation_failed = false;
    let mut tool_cache = std::collections::HashSet::new();
    let mut no_growth = 0;
    let model_limit = remaining_models(turn);
    let mut wire_messages = json!([]);
    let mut wire_tools = json!([]);
    let result = (|| loop {
        check()?;
        if started.elapsed() > Duration::from_secs(60) {
            return Err("小舵回答超时，请缩小问题范围后重试".into());
        }
        let line = match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(l) => l,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(_) => return Err("小舵运行时中断".into()),
        };
        let mut m: Value = serde_json::from_str(&line).map_err(|_| "Agent协议无效")?;
        let result = match m["type"].as_str() {
            Some("ready") => continue,
            Some("tool_request") => {
                let key = json!([m["name"], m["args"]]).to_string();
                let repeated = !tool_cache.insert(key);
                let is_search = m["name"] == "search_evidence";
                let exhausted_limit = calls >= 8 || (is_search && searches >= 2) || no_growth >= 2;
                let previous_seen = seen.len();
                let reply = if repeated || exhausted_limit {
                    no_growth += 1;
                    json!({"rows":[],"retrieval_exhausted":exhausted_limit || no_growth>=2,"cached":repeated,"notice":"已有内容保留；停止重复检索，依据已提供背景回答。尚无依据的部分说明缺口或追问，不断言会议没有提到。"})
                } else {
                    calls += 1;
                    if is_search {
                        searches += 1;
                    }
                    let remaining =
                        analysis::agent_remaining_bytes(rt, config, &wire_messages, &wire_tools)?;
                    let reserve = ((model_limit.saturating_sub(models)) * 384
                        + (8usize.saturating_sub(calls)) * 256
                        + 512)
                        .min(4096);
                    let limit = remaining.saturating_sub(reserve).min(20 * 1024 / reduction);
                    if limit < 512 {
                        exhausted(
                            m["args"]["offset"].as_u64().unwrap_or(0) as usize,
                            m["args"]["char_offset"].as_u64().unwrap_or(0) as usize,
                            snapshot["evidence"].as_array().map_or(0, Vec::len),
                        )
                    } else {
                        match tool(rt, turn, question, snapshot, &m, &mut seen, limit) {
                            Ok(v) => v,
                            Err(e) => json!({"error":e,"executed":false}),
                        }
                    }
                };
                let returned_evidence = reply["rows"]
                    .as_array()
                    .is_some_and(|rows| !rows.is_empty())
                    || reply["user_records"].as_object().is_some_and(|records| {
                        records
                            .values()
                            .any(|rows| rows.as_array().is_some_and(|rows| !rows.is_empty()))
                    });
                if !returned_evidence
                    && previous_seen == seen.len()
                    && m["name"] != "propose_action"
                {
                    no_growth += 1;
                } else {
                    no_growth = 0;
                }
                wire_messages
                    .as_array_mut()
                    .unwrap()
                    .push(json!({"role":"tool",
                    "tool_call_id":m["tool_call_id"],"content":reply.to_string()}));
                reply
            }
            Some("model_request") => {
                models += 1;
                if models > model_limit {
                    return Err("已达到本轮模型调用上限".into());
                }
                let tools = m["tools"].as_array().ok_or("Agent工具声明无效")?;
                let names = tools
                    .iter()
                    .filter_map(|t| t["function"]["name"].as_str())
                    .collect::<std::collections::HashSet<_>>();
                let is_compaction = m["purpose"] == "compaction" && tools.is_empty();
                let is_final_answer =
                    m["purpose"] == "final_answer" && tools.is_empty() && !compact_only;
                if !is_compaction
                    && !is_final_answer
                    && (names.len() != TOOLS.len()
                        || tools.len() != TOOLS.len()
                        || tools.iter().any(|t| {
                            !TOOLS.contains(&t["function"]["name"].as_str().unwrap_or(""))
                        }))
                {
                    return Err("Agent请求了未开放工具".into());
                }
                if !is_compaction {
                    let tools_value = m["tools"].clone();
                    if prepared.is_none() {
                        let pack = crate::assistant_context::prepare(
                            rt,
                            config,
                            snapshot,
                            question,
                            &m["messages"],
                            &tools_value,
                            reduction,
                        )?;
                        remember_returned(
                            &pack,
                            snapshot["evidence"].as_array().ok_or("依据快照无效")?,
                            &mut seen,
                        );
                        for row in pack["chat_history"]["rows"]
                            .as_array()
                            .into_iter()
                            .flatten()
                        {
                            if let Some(source_id) = row["source_id"].as_str() {
                                seen.insert(source_id.to_owned(), row.clone());
                            }
                        }
                        prepared = Some(pack);
                    }
                    m["messages"] = crate::assistant_context::inject(
                        &m["messages"],
                        question,
                        prepared.as_ref().unwrap(),
                    );
                }
                let bounded = || {
                    check()?;
                    if started.elapsed() > Duration::from_secs(60) {
                        return Err("小舵回答超时".into());
                    }
                    Ok(())
                };
                let _queued = QueueEntry::new(rt, turn)?;
                let _permit = loop {
                    bounded()?;
                    let queue = rt.analysis_waiting.lock().map_err(|_| "分析队列异常")?;
                    let next = queue
                        .iter()
                        .enumerate()
                        .min_by_key(|(i, (_, p))| (*p, *i))
                        .map(|(_, (id, _))| id);
                    if next.map(String::as_str) == Some(turn) {
                        if let Ok(g) = rt.llm_gate.try_lock() {
                            break g;
                        }
                    }
                    drop(queue);
                    std::thread::sleep(Duration::from_millis(50));
                };
                let mut selected = config.clone();
                if is_compaction {
                    selected.max_output_tokens = selected.max_output_tokens.min(1536);
                }
                spend_model(turn)?;
                let response = analysis::request_agent(
                    rt,
                    &selected,
                    m["messages"].clone(),
                    &m["tools"],
                    &bounded,
                )?;
                if is_final_answer
                    && response["tool_calls"]
                        .as_array()
                        .is_some_and(|v| !v.is_empty())
                {
                    return Err("模型在最终回答阶段仍请求工具，请重试或检查模型工具能力".into());
                }
                wire_messages = m["messages"].clone();
                wire_tools = m["tools"].clone();
                let mut message = json!({"role":"assistant","content":response["content"]});
                if !response["tool_calls"].is_null() {
                    message["tool_calls"] = response["tool_calls"].clone();
                }
                wire_messages
                    .as_array_mut()
                    .ok_or("Agent消息结构无效")?
                    .push(message);
                response
            }
            Some("compacted") if compact_only => {
                let summary = m["summary"].as_str().ok_or("压缩摘要无效")?;
                if summary.len() > 6144 || summary.trim().is_empty() {
                    return Err("压缩摘要无效".into());
                }
                return Ok(
                    json!({"summary":summary,"tokens_before":m["tokens_before"],"usage":m["usage"]}),
                );
            }
            Some("result") if !compact_only => {
                let mut answer = validate_answer_scoped(
                    m["text"].as_str().ok_or("Agent没有回答正文")?,
                    &seen,
                    meeting_core::answer_policy::AdviceScope {
                        validation_failed,
                        ..advice_scope(snapshot, prepared.as_ref(), question)
                    },
                )?;
                answer["coverage"] = json!({"scope":snapshot["context"]["scope"],"through_ms":snapshot["context"]["asr_through_ms"],"transcript_returned":seen.values().filter(|e|e["kind"]=="meeting" && e["partial"]!=true).count(),"transcript_excerpts":seen.values().filter(|e|e["kind"]=="meeting" && e["partial"]==true).count(),"basis":prepared.as_ref().map(|p|p["basis"].clone()),"memory_processed":snapshot["context"]["meeting_memory"]["processed_segments"],"memory_total":snapshot["context"]["meeting_memory"]["segments_total"],"memory_pending":snapshot["context"]["meeting_memory"]["pending_segments"],"chat_history_rows":prepared.as_ref().map(|p|p["chat_history"]["rows"].as_array().map_or(0,Vec::len)),"chat_summary_pending":prepared.as_ref().map(|p|p["chat_history"]["pending_turns"].clone()),"asr_missing_ranges":snapshot["context"]["asr_missing_ranges"],"recording_through_ms":snapshot["context"]["recording_through_ms"],"playback_overlap_segments":snapshot["evidence"].as_array().into_iter().flatten().filter(|e|e["assistant_playback_overlap"]==true).count(),"transcript_total":snapshot["evidence"].as_array().into_iter().flatten().filter(|e|e["kind"]=="meeting").count()});
                return Ok(answer);
            }
            Some("validate") => match validate_answer_scoped(
                m["text"].as_str().unwrap_or(""),
                &seen,
                meeting_core::answer_policy::AdviceScope {
                    validation_failed,
                    ..advice_scope(snapshot, prepared.as_ref(), question)
                },
            ) {
                Ok(_) => json!({"ok":true}),
                Err(e) => {
                    validation_failed = true;
                    json!({"ok":false,"error":e})
                }
            },
            Some("failure") => {
                let reason = m["error"]
                    .as_str()
                    .unwrap_or("Agent运行失败，请检查模型配置和工具能力");
                return Err(format!("小舵Agent失败：{reason}"));
            }
            _ => return Err("Agent协议事件无效".into()),
        };
        check()?;
        writeln!(
            input,
            "{}",
            json!({"type":"reply","id":m["id"],"result":result})
        )
        .map_err(|_| "Agent连接已关闭")?;
    })();
    drop(input);
    drop(child);
    drop(rx);
    let _ = reader.join();
    result
}
// A model needs an unambiguous citation ID, not three copies of its UUID.
// Full provenance remains in the host snapshot and validated answer evidence.
fn model_evidence(row: &Value) -> Value {
    let mut result = json!({});
    for key in [
        "source_id",
        "paragraph_id",
        "kind",
        "text",
        "start_ms",
        "name",
        "version",
        "owner_text",
        "due_text",
        "execution_state",
        "confirmation_state",
    ] {
        if !row[key].is_null() {
            result[key] = row[key].clone();
        }
    }
    if row["assistant_playback_overlap"] == true {
        result["assistant_playback_overlap"] = json!(true);
    }
    result
}
fn tool_wire_bytes(value: &Value) -> usize {
    // Tool JSON becomes a string inside the next HTTP JSON body: count both layers.
    Value::String(value.to_string()).to_string().len()
}
fn exhausted(offset: usize, char_offset: usize, total: usize) -> Value {
    json!({"rows":[],"offset":offset,"total":total,"next_offset":offset,
        "next_char_offset":char_offset,"partial":true,"budget_exhausted":true,
        "notice":"本轮读取预算已用完，请依据已读内容回答并说明未完整覆盖；原文保留，可继续有针对性地提问。"})
}
fn evidence_page(rows: &[Value], offset: usize, char_offset: usize, limit: usize) -> Value {
    let mut out = json!({"rows":[],"offset":offset,"total":rows.len(),"next_offset":null,
        "next_char_offset":null,"partial":offset>0,"coverage":"仅返回的原文片段可作依据，未读部分不能推断不存在"});
    let mut count = 0;
    for (index, source) in rows.iter().enumerate().skip(offset) {
        let mut row = model_evidence(source);
        let start = if index == offset { char_offset } else { 0 };
        let original = source["text"].as_str().unwrap_or("");
        let chars = original.chars().collect::<Vec<_>>();
        let start = start.min(chars.len());
        row["text"] = json!(chars[start..].iter().collect::<String>());
        if start > 0 {
            row["partial"] = json!(true);
            row["char_start"] = json!(start);
        }
        out["rows"].as_array_mut().unwrap().push(row.clone());
        out["next_offset"] = (index + 1 < rows.len()).then_some(index + 1).into();
        out["next_char_offset"] = Value::Null;
        out["partial"] = json!(offset > 0 || start > 0 || index + 1 < rows.len());
        if tool_wire_bytes(&out) <= limit {
            count += 1;
            continue;
        }
        out["rows"].as_array_mut().unwrap().pop();
        if count > 0 {
            out["next_offset"] = json!(index);
            out["partial"] = json!(true);
            break;
        }
        // A single long segment is paged at Unicode boundaries without changing
        // its original segment/revision ID. Never register the unseen suffix.
        let (mut low, mut high) = (start, chars.len());
        while low < high {
            let end = low + (high - low).div_ceil(2);
            row["text"] = json!(chars[start..end].iter().collect::<String>());
            row["partial"] = json!(true);
            row["char_start"] = json!(start);
            row["char_end"] = json!(end);
            out["rows"] = json!([row]);
            out["partial"] = json!(true);
            out["next_offset"] = json!(index);
            out["next_char_offset"] = json!(end);
            if tool_wire_bytes(&out) <= limit {
                low = end;
            } else {
                high = end - 1;
            }
            row = out["rows"][0].clone();
        }
        if low == start {
            return exhausted(offset, char_offset, rows.len());
        }
        row["text"] = json!(chars[start..low].iter().collect::<String>());
        row["char_end"] = json!(low);
        out["rows"] = json!([row]);
        out["next_char_offset"] = json!(low);
        return out;
    }
    out
}
fn bounded_context(context: &Value, limit: usize) -> Value {
    let mut result = context.clone();
    if let Some(context) = result.as_object_mut() {
        context.remove("behavior");
        if let Some(template) = context.get_mut("template").and_then(Value::as_object_mut) {
            template.remove("policy");
            template.remove("instructions");
        }
    }
    // Keep the most recent user records and meeting memory facts. Omitted records
    // remain searchable; permission/scope/watermarks are never summarized away.
    for pointer in [
        "/user_records/notes",
        "/user_records/actions",
        "/meeting_memory/facts",
    ] {
        while tool_wire_bytes(&result) > limit {
            let Some(rows) = result.pointer_mut(pointer).and_then(Value::as_array_mut) else {
                break;
            };
            if rows.is_empty() {
                break;
            }
            rows.pop();
            result["partial"] = json!(true);
            if pointer.starts_with("/user_records/") {
                result["user_records"]["partial"] = json!(true);
            }
        }
    }
    if tool_wire_bytes(&result) > limit {
        result = json!({"kind":context["kind"],"meeting_id":context["meeting_id"],
            "scope":context["scope"],"context_allowed":context["context_allowed"],
            "state":context["state"],"asr_through_ms":context["asr_through_ms"],
            "partial":true,"notice":"详细背景超过本轮剩余预算，请检索相关原文或用户记录；不能宣称完整读取。"});
    }
    result
}
fn remember_returned(reply: &Value, all: &[Value], seen: &mut HashMap<String, Value>) {
    let top_level_records = ["notes", "actions"]
        .into_iter()
        .flat_map(|key| reply["user_records"][key].as_array().into_iter().flatten());
    let nested_records = ["notes", "actions"].into_iter().flat_map(|key| {
        reply["context"]["user_records"][key]
            .as_array()
            .into_iter()
            .flatten()
    });
    for row in reply["rows"]
        .as_array()
        .into_iter()
        .flatten()
        .chain(top_level_records)
        .chain(nested_records)
    {
        let Some(source) = all
            .iter()
            .find(|source| source["source_id"] == row["source_id"])
        else {
            continue;
        };
        let mut returned = source.clone();
        returned["text"] = row["text"].clone();
        if row["text"] != source["text"] {
            returned["partial"] = json!(true);
        }
        let key = source["source_id"].as_str().unwrap_or("");
        // Later excerpts cannot erase a previously complete read of this source.
        if seen.get(key).is_none_or(|old| old["partial"] == true) {
            seen.insert(key.into(), returned);
        }
    }
}

fn tool(
    rt: &Runtime,
    turn: &str,
    question: &str,
    snapshot: &Value,
    m: &Value,
    seen: &mut HashMap<String, Value>,
    reply_limit: usize,
) -> Result<Value, String> {
    let all = snapshot["evidence"].as_array().ok_or("依据快照无效")?;
    let rows = match m["name"].as_str() {
        Some("get_context") => {
            let reply = bounded_context(&snapshot["context"], reply_limit);
            remember_returned(&reply, all, seen);
            return Ok(reply);
        }
        Some("get_methods") => all
            .iter()
            .filter(|v| v["kind"] == "method")
            .cloned()
            .collect::<Vec<_>>(),
        Some("get_transcript") => {
            if let Some(ids) = m["args"]["source_ids"].as_array() {
                if ids.len() > 32 {
                    return Err("单次最多核对32个来源".into());
                }
                let rows = ids
                    .iter()
                    .filter_map(|id| {
                        all.iter()
                            .find(|e| e["source_id"] == *id && e["kind"] == "meeting")
                            .cloned()
                    })
                    .collect::<Vec<_>>();
                let reply = evidence_page(&rows, 0, 0, reply_limit);
                remember_returned(&reply, all, seen);
                return Ok(reply);
            }
            let offset = m["args"]["offset"].as_u64().unwrap_or(0) as usize;
            let rows = all
                .iter()
                .filter(|v| v["kind"] == "meeting")
                .cloned()
                .collect::<Vec<_>>();
            let char_offset = m["args"]["char_offset"].as_u64().unwrap_or(0) as usize;
            let reply = evidence_page(&rows, offset, char_offset, reply_limit);
            remember_returned(&reply, all, seen);
            return Ok(reply);
        }
        Some("search_evidence") => {
            let query = m["args"]["query"].as_str().unwrap_or(question);
            if query.len() > 720 {
                return Err("检索词过长".into());
            }
            crate::assistant_context::ranked(snapshot, query)
        }
        Some("propose_action") => {
            let a = &m["args"];
            let kind = a["action"].as_str().ok_or("操作草稿无效")?;
            if !matches!(kind, "prepare_meeting" | "note" | "todo") {
                return Err("不支持该操作草稿".into());
            }
            let title = a["title"].as_str().unwrap_or("新会议");
            let text = a["text"].as_str().unwrap_or("");
            if title.len() > 360
                || text.len() > 9000
                || (kind == "todo" && text.len() > 6000)
                || text.trim().is_empty()
            {
                return Err("操作草稿过长".into());
            }
            if matches!(kind, "note" | "todo") && snapshot["context"]["meeting_id"].is_null() {
                return Err("保存记录需要明确会议".into());
            }
            let mut pid = id();
            let payload = json!({"action":kind,"title":title,"text":text,"meeting_type":a["meeting_type"].as_str().unwrap_or("general"),"meeting_id":snapshot["context"]["meeting_id"],"meeting_generation":snapshot["context"]["generation"],"question":question});
            if let Some(existing) = rt
                .db()?
                .query_row(
                    "SELECT id FROM assistant_proposals WHERE turn_id=? AND payload_json=?",
                    params![turn, payload.to_string()],
                    |r| r.get::<_, String>(0),
                )
                .optional()
                .map_err(err)?
            {
                pid = existing;
            }
            rt.db()?
                .execute(
                    "INSERT OR IGNORE INTO assistant_proposals(id,turn_id,payload_json) VALUES (?,?,?)",
                    params![pid, turn, payload.to_string()],
                )
                .map_err(err)?;
            return Ok(
                json!({"proposal_id":pid,"state":"pending","notice":"仅草稿，尚未执行，用户需在界面接受"}),
            );
        }
        _ => return Err("Agent工具未开放".into()),
    };
    let offset = m["args"]["offset"].as_u64().unwrap_or(0) as usize;
    let char_offset = m["args"]["char_offset"].as_u64().unwrap_or(0) as usize;
    let mut reply = evidence_page(&rows, offset, char_offset, reply_limit);
    reply["coverage"] = json!("这是检索结果，不代表整场完整扫描；未读部分不能推断不存在");
    reply["partial"] = json!(true);
    remember_returned(&reply, all, seen);
    Ok(reply)
}

fn unwrap_answer(mut value: Value) -> Value {
    if value["blocks"].is_array() {
        return value;
    }
    for key in ["answer", "result", "data", "output"] {
        if value[key]["blocks"].is_array() {
            let nested = value[key].clone();
            if let Some(object) = value.as_object_mut() {
                object.insert("blocks".into(), nested["blocks"].clone());
            }
            return value;
        }
    }
    value
}

fn canonical_reference(raw: &str, seen: &HashMap<String, Value>) -> Result<String, String> {
    let raw = raw
        .trim()
        .trim_matches('`')
        .trim_matches(|c| matches!(c, '[' | ']' | '(' | ')' | ',' | ';' | '"'));
    if seen.contains_key(raw) {
        return Ok(raw.to_owned());
    }
    let embedded = seen
        .keys()
        .filter(|key| raw.contains(key.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    if embedded.len() == 1 {
        return Ok(embedded[0].clone());
    }
    // Some providers shorten a UUID citation to the segment UUID or omit the
    // revision suffix. Resolve only an unambiguous suffix of at least 8 chars.
    if raw.len() >= 8 {
        let suffixes = seen
            .keys()
            .filter(|key| key.ends_with(raw))
            .cloned()
            .collect::<Vec<_>>();
        if suffixes.len() == 1 {
            return Ok(suffixes[0].clone());
        }
    }
    Err(format!(
        "小舵引用了未检索的依据：{}",
        raw.chars().take(80).collect::<String>()
    ))
}

fn normalize_reference(value: &Value, seen: &HashMap<String, Value>) -> Result<String, String> {
    let raw = value
        .as_str()
        .or_else(|| value.get("source_id").and_then(Value::as_str))
        .ok_or("引用ID无效：必须是字符串或source_id对象")?;
    canonical_reference(raw, seen)
}

fn advice_scope(
    snapshot: &Value,
    pack: Option<&Value>,
    question: &str,
) -> meeting_core::answer_policy::AdviceScope {
    let context = &snapshot["context"];
    let complete = pack.is_some_and(|p| {
        p["coverage"]["full_transcript"] == true
            && p["coverage"]["partial"] == false
            && p["chat_history"]["partial"] == false
            && p["chat_history"]["pending_turns"].as_u64() == Some(0)
    }) && context["scope"] == "all"
        && context["asr_missing_ranges"].as_u64() == Some(0)
        && context["asr_through_ms"]
            .as_u64()
            .zip(context["recording_through_ms"].as_u64())
            .is_some_and(|(asr, recording)| asr >= recording)
        && !snapshot["evidence"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|e| e["assistant_playback_overlap"] == true);
    meeting_core::answer_policy::AdviceScope {
        meeting: context["kind"] == "meeting",
        complete,
        meeting_only: meeting_core::answer_policy::meeting_only(question),
        validation_failed: false,
    }
}
#[cfg(test)]
pub fn validate_answer(text: &str, seen: &HashMap<String, Value>) -> Result<Value, String> {
    validate_answer_scoped(
        text,
        seen,
        meeting_core::answer_policy::AdviceScope::default(),
    )
}
fn validate_answer_scoped(
    text: &str,
    seen: &HashMap<String, Value>,
    scope: meeting_core::answer_policy::AdviceScope,
) -> Result<Value, String> {
    let mut value = unwrap_answer(analysis::parse(text)?);
    let mut blocks = value["blocks"]
        .as_array()
        .cloned()
        .or_else(|| value["summary"].as_array().map(|items| items.iter().map(|item| json!({"kind":"meeting_fact","text":item["text"],"sources":item["evidence"]})).collect()))
        .ok_or("小舵回答结构无效：缺少blocks数组")?;
    if blocks.is_empty() || blocks.len() > 16 {
        return Err("小舵回答条目无效：blocks数量必须为1到16条".into());
    }
    let mut used = HashMap::new();
    let mixed = blocks.iter().any(|b| {
        matches!(
            b["kind"].as_str(),
            Some("meeting_fact" | "knowledge" | "user_record")
        )
    });
    let mut experience_started = false;
    for b in &mut blocks {
        let kind = b["kind"]
            .as_str()
            .ok_or("回答类型无效：缺少kind")?
            .to_owned();
        if !matches!(
            kind.as_str(),
            "meeting_fact" | "knowledge" | "user_record" | "suggestion" | "unanswered"
        ) || b["text"]
            .as_str()
            .is_none_or(|t| t.trim().is_empty() || t.len() > 9000)
        {
            return Err("小舵回答内容无效：kind或text不符合协议".into());
        }
        if b["sources"].is_null() && b["evidence"].is_array() {
            b["sources"] = b["evidence"].clone();
        }
        let refs = b["sources"].as_array().cloned().unwrap_or_default();
        if let Some(basis) = b.get("basis") {
            if basis != "experience" || kind != "suggestion" || !refs.is_empty() {
                return Err("经验建议必须使用kind=suggestion、basis=experience及空sources；有依据的事实请另列条目".into());
            }
            if scope.validation_failed {
                return Err("本轮已发生格式或引用校验错误，不能改用经验建议掩盖；请修复原回答或明确返回validation_failed".into());
            }
            if scope.meeting_only {
                *b = json!({"kind":"unanswered","text":"目前我还没找到足以回答这个问题的会议依据；你要求只依据会议，我先不补充经验判断。","sources":[],"reason":"insufficient_evidence"});
                continue;
            }
            if !experience_started {
                b["text"] = json!(scope.introduce(b["text"].as_str().unwrap(), mixed));
                experience_started = true;
            }
            if b["text"].as_str().unwrap().len() > 9000 {
                return Err("经验建议含来源说明后超过正文长度限制".into());
            }
        }
        if let Some(reason) = b.get("reason") {
            if kind != "unanswered"
                || !matches!(
                    reason.as_str(),
                    Some("insufficient_evidence" | "validation_failed")
                )
                || !refs.is_empty()
            {
                return Err("未回答原因无效或携带了不适用的引用".into());
            }
        }
        if scope.validation_failed && kind == "unanswered" {
            *b = json!({"kind":"unanswered","reason":"validation_failed","text":"这轮回答的格式或引用校验未通过，暂时无法给出可靠答复。原文仍保留，请重试。","sources":[]});
            continue;
        }
        if scope.validation_failed
            && kind == "suggestion"
            && !refs.iter().any(|reference| {
                normalize_reference(reference, seen)
                    .ok()
                    .and_then(|key| seen.get(&key))
                    .is_some_and(|e| {
                        matches!(e["kind"].as_str(), Some("meeting" | "knowledge" | "record"))
                    })
            })
        {
            return Err(
                "格式或引用修复不能转成无依据的通用建议；请修复引用或明确返回validation_failed"
                    .into(),
            );
        }
        if refs.len() > 20
            || (matches!(kind.as_str(), "meeting_fact" | "knowledge" | "user_record")
                && refs.is_empty())
        {
            return Err(format!("小舵事实缺少依据：{}必须引用已读取来源", kind));
        }
        let mut normalized = Vec::with_capacity(refs.len());
        for reference in &refs {
            let key = normalize_reference(reference, seen)?;
            let e = seen.get(&key).ok_or("小舵引用了未检索的依据")?;
            if kind == "meeting_fact" && e["assistant_playback_overlap"] == true {
                return Err(
                    "会议事实引用了与助手播报重叠的原文：请标为待澄清或改用其他原文".into(),
                );
            }
            if (kind == "meeting_fact" && e["kind"] != "meeting")
                || (kind == "knowledge" && e["kind"] != "knowledge")
                || (kind == "user_record" && e["kind"] != "record")
            {
                return Err(format!(
                    "引用类型不匹配：{}不能引用{}来源",
                    kind,
                    e["kind"].as_str().unwrap_or("未知")
                ));
            }
            normalized.push(Value::String(key.clone()));
        }
        if scope.meeting_only
            && kind == "suggestion"
            && !normalized.iter().any(|key| {
                seen.get(key.as_str().unwrap())
                    .is_some_and(|e| e["kind"] == "meeting")
            })
        {
            *b = json!({"kind":"unanswered","reason":"insufficient_evidence","text":"目前我还没找到足以支持这条建议的会议依据；你要求只依据会议，我先不补充经验判断。","sources":[]});
            continue;
        }
        for key in &normalized {
            let key = key.as_str().unwrap();
            used.insert(key.to_owned(), &seen[key]);
        }
        b["sources"] = Value::Array(normalized);
    }
    value["blocks"] = Value::Array(blocks.clone());
    Ok(
        json!({"blocks":blocks,"evidence":used.values().collect::<Vec<_>>(),"engine":"pi-coding-agent/1.3.0","mode":value["mode"].as_str().filter(|m|meeting_core::dialogue::answer_mode(m).is_some())}),
    )
}

struct QueueEntry<'a> {
    rt: &'a Runtime,
    id: String,
}
pub(crate) fn with_model(
    rt: &Runtime,
    turn: &str,
    check: &dyn Fn() -> Result<(), String>,
    operation: impl FnOnce() -> Result<Value, String>,
) -> Result<Value, String> {
    let _entry = QueueEntry::new(rt, turn)?;
    loop {
        check()?;
        let queue = rt.analysis_waiting.lock().map_err(|_| "分析队列异常")?;
        let next = queue
            .iter()
            .enumerate()
            .min_by_key(|(i, (_, p))| (*p, *i))
            .map(|(_, (id, _))| id);
        if next.map(String::as_str) == Some(turn) {
            if let Ok(_permit) = rt.llm_gate.try_lock() {
                drop(queue);
                return operation();
            }
        }
        drop(queue);
        std::thread::sleep(Duration::from_millis(50));
    }
}
impl<'a> QueueEntry<'a> {
    fn new(rt: &'a Runtime, id: &str) -> Result<Self, String> {
        rt.analysis_waiting
            .lock()
            .map_err(|_| "分析队列异常")?
            .push((id.into(), 0));
        Ok(Self { rt, id: id.into() })
    }
}
impl Drop for QueueEntry<'_> {
    fn drop(&mut self) {
        if let Ok(mut queue) = self.rt.analysis_waiting.lock() {
            queue.retain(|(id, _)| id != &self.id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{TcpListener, TcpStream};

    #[test]
    fn adaptive_capacity_errors_are_retried_without_treating_auth_as_capacity() {
        assert!(adaptive_capacity_error("模型输出被截断：已达到输出上限"));
        assert!(adaptive_capacity_error(
            "模型基础上下文不足，请减少过长的问题或历史"
        ));
        assert!(adaptive_capacity_error(
            "模型上下文容量不足：服务拒绝了本轮输入长度"
        ));
        assert!(adaptive_capacity_error("请求体超过当前模型输入安全预算"));
        assert!(!adaptive_capacity_error(
            "模型服务返回 HTTP 401，请检查模型、凭据或额度"
        ));
        assert!(!adaptive_capacity_error(
            "模型输出校验失败：summary[0].evidence"
        ));
    }
    fn setup() -> (tempfile::TempDir, Arc<Runtime>) {
        let tmp = tempfile::tempdir().unwrap();
        let rt = Runtime::open_with_vault(
            tmp.path().into(),
            Arc::new(crate::models::tests::MemoryVault::default()),
        )
        .unwrap();
        (tmp, rt)
    }
    fn seed(rt: &Runtime, title: &str, text: &str) -> (String, String) {
        let mid = rt.create(title, "general").unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        let sid = id();
        let db = rt.db().unwrap();
        db.execute(
            "UPDATE meetings SET state='completed',recording_duration_ms=2000 WHERE id=?",
            [&mid],
        )
        .unwrap();
        db.execute(
            "INSERT INTO transcript_segments VALUES (?,?,0,1000,'mixed',1,'final')",
            params![sid, mid],
        )
        .unwrap();
        db.execute(
            "INSERT INTO transcript_revisions VALUES (?,1,?,?,'asr',?)",
            params![sid, mid, text, now()],
        )
        .unwrap();
        (mid, sid)
    }
    fn receive(s: &mut TcpStream) -> Value {
        s.set_read_timeout(Some(Duration::from_secs(6))).unwrap();
        let mut bytes = vec![];
        loop {
            let mut block = [0; 4096];
            let n = s.read(&mut block).unwrap();
            assert!(n > 0);
            bytes.extend_from_slice(&block[..n]);
            if let Some(end) = bytes.windows(4).position(|b| b == b"\r\n\r\n") {
                let header = String::from_utf8_lossy(&bytes[..end]).to_lowercase();
                let len = header
                    .lines()
                    .find_map(|l| {
                        l.strip_prefix("content-length:")
                            .and_then(|v| v.trim().parse::<usize>().ok())
                    })
                    .unwrap();
                if bytes.len() >= end + 4 + len {
                    return serde_json::from_slice(&bytes[end + 4..end + 4 + len]).unwrap();
                }
            }
        }
    }
    fn profile(rt: &Runtime, url: &str) {
        models::save(
            rt,
            models::ProfileInput {
                profile_id: None,
                expected_version: None,
                display_name: "合成Agent模型".into(),
                base_url: url.into(),
                model: "fake-tools".into(),
                context_tokens: 32768,
                max_output_tokens: 2048,
                timeout_seconds: 10,
                key_action: "clear".into(),
                api_key: String::new(),
            },
        )
        .unwrap();
    }
    fn wait(rt: &Runtime, turn: &str) -> String {
        let until = Instant::now() + Duration::from_secs(12);
        loop {
            let state: String = rt
                .db()
                .unwrap()
                .query_row(
                    "SELECT state FROM assistant_turns WHERE id=?",
                    [turn],
                    |r| r.get(0),
                )
                .unwrap();
            if !["pending", "running"].contains(&state.as_str()) {
                return state;
            }
            assert!(Instant::now() < until, "turn timeout");
            std::thread::sleep(Duration::from_millis(30));
        }
    }
    #[test]
    fn experience_advice_is_marked_once_and_never_relabels_bad_facts_or_validation_errors() {
        use meeting_core::answer_policy::AdviceScope;
        let partial = AdviceScope {
            meeting: true,
            complete: false,
            meeting_only: false,
            validation_failed: false,
        };
        let mut seen = HashMap::new();
        seen.insert(
            "meeting:synthetic:1".into(),
            json!({"source_id":"meeting:synthetic:1","kind":"meeting","text":"本场预算已确定"}),
        );
        let advice = json!({"blocks":[{"kind":"suggestion","basis":"experience","text":"我建议先小范围测试。","sources":[]}]});
        let answer = validate_answer_scoped(&advice.to_string(), &seen, partial).unwrap();
        assert!(answer["blocks"][0]["text"]
            .as_str()
            .unwrap()
            .starts_with("目前我还没找到相关讨论。"));
        assert_eq!(answer["blocks"][0]["basis"], "experience");
        assert_eq!(answer["evidence"], json!([]));
        assert_eq!(
            validate_answer_scoped(&answer.to_string(), &seen, partial).unwrap(),
            answer
        );
        assert!(validate_answer_scoped(
            &advice.to_string(),
            &seen,
            AdviceScope {
                validation_failed: true,
                ..partial
            }
        )
        .is_err());
        let full = validate_answer_scoped(
            &advice.to_string(),
            &seen,
            AdviceScope {
                complete: true,
                ..partial
            },
        )
        .unwrap();
        assert!(full["blocks"][0]["text"]
            .as_str()
            .unwrap()
            .starts_with("前面我们还没讨论这一点。"));
        let mut mixed = advice.clone();
        mixed["blocks"].as_array_mut().unwrap().insert(0,json!({"kind":"meeting_fact","text":"本场预算已确定。","sources":["meeting:synthetic:1"]}));
        let mixed = validate_answer_scoped(&mixed.to_string(), &seen, partial).unwrap();
        assert_eq!(mixed["blocks"][0]["text"], "本场预算已确定。");
        assert!(mixed["blocks"][1]["text"]
            .as_str()
            .unwrap()
            .starts_with("这部分还没找到"));
        assert_eq!(mixed["evidence"].as_array().unwrap().len(), 1);
        let only = validate_answer_scoped(
            &advice.to_string(),
            &seen,
            AdviceScope {
                meeting_only: true,
                ..partial
            },
        )
        .unwrap();
        assert_eq!(only["blocks"][0]["kind"], "unanswered");
        assert_eq!(only["blocks"][0]["reason"], "insufficient_evidence");
        assert!(!only.to_string().contains("先小范围测试"));
        let unmarked =
            json!({"blocks":[{"kind":"suggestion","text":"我建议先测试。","sources":[]}]});
        let only_unmarked = validate_answer_scoped(
            &unmarked.to_string(),
            &seen,
            AdviceScope {
                meeting_only: true,
                ..partial
            },
        )
        .unwrap();
        assert_eq!(only_unmarked["blocks"][0]["kind"], "unanswered");
        assert_eq!(only_unmarked["evidence"], json!([]));

        let failure = json!({"blocks":[{"kind":"unanswered","reason":"validation_failed","text":"这轮回答的格式或引用校验未通过，请重试。","sources":[]}]});
        let failure = validate_answer_scoped(&failure.to_string(), &seen, partial).unwrap();
        assert_eq!(failure["blocks"][0]["reason"], "validation_failed");
        assert!(!failure.to_string().contains("根据我的"));
        for block in [
            json!({"kind":"meeting_fact","basis":"experience","text":"虚构事实","sources":[]}),
            json!({"kind":"suggestion","basis":"experience","text":"虚构引用","sources":["meeting:unknown:1"]}),
            json!({"kind":"meeting_fact","text":"无依据事实","sources":[]}),
        ] {
            assert!(
                validate_answer_scoped(&json!({"blocks":[block]}).to_string(), &seen, partial)
                    .is_err()
            );
        }
    }
    #[test]
    fn only_complete_unfiltered_current_scope_can_claim_no_prior_discussion() {
        let snap = json!({"context":{"kind":"meeting","scope":"all","asr_missing_ranges":0,"asr_through_ms":2000,"recording_through_ms":2000},"evidence":[]});
        let pack = json!({"coverage":{"full_transcript":true,"partial":false},"chat_history":{"partial":false,"pending_turns":0}});
        assert!(advice_scope(&snap, Some(&pack), "给一点建议").complete);
        for (field, value) in [
            ("scope", json!("recent")),
            ("asr_missing_ranges", json!(1)),
            ("asr_through_ms", json!(1000)),
        ] {
            let mut altered = snap.clone();
            altered["context"][field] = value;
            assert!(!advice_scope(&altered, Some(&pack), "给一点建议").complete);
        }
        let mut overlap = snap.clone();
        overlap["evidence"] = json!([{"assistant_playback_overlap":true}]);
        assert!(!advice_scope(&overlap, Some(&pack), "给建议").complete);
        for field in ["coverage", "chat_history"] {
            let mut altered = pack.clone();
            altered[field]["partial"] = json!(true);
            assert!(!advice_scope(&snap, Some(&altered), "给建议").complete);
        }
        assert!(advice_scope(&snap, Some(&pack), "只依据本次会议").meeting_only);
        assert!(!advice_scope(&snap, None, "给建议").complete);
    }
    #[test]
    fn real_pi_broker_persists_experience_preface_and_blocks_it_for_meeting_only_question() {
        let (_tmp, rt) = setup();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        profile(&rt, &format!("http://{}", listener.local_addr().unwrap()));
        let (mid, _) = seed(&rt, "合成经验回答", "本场讨论产品名称，没有预算分配记录。");
        let c = open(&rt, Some(mid), false).unwrap();
        let cid = c["id"].as_str().unwrap();
        let server = std::thread::spawn(move || {
            for n in 0..4 {
                let (mut stream, _) = listener.accept().unwrap();
                let request = receive(&mut stream);
                assert!(request["messages"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|m| m["role"] == "system"
                        && m["content"]
                            .as_str()
                            .is_some_and(|t| t.contains("会议依据不足时的经验回答规则")
                                && t.contains("persona的性格")
                                && t.contains("本场模板"))));
                let content = if n == 2 {
                    "INVALID JSON".to_owned()
                } else {
                    json!({"blocks":[{"kind":"suggestion","basis":"experience","text":"我建议先小范围测试，再决定是否扩大预算。","sources":[]}]}).to_string()
                };
                let body =
                    json!({"choices":[{"finish_reason":"stop","message":{"content":content}}]})
                        .to_string();
                write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
            }
        });
        for (q, experience) in [
            ("按你的经验，预算分配可以怎么做？", true),
            ("只依据本次会议回答预算分配，不要补充经验", false),
        ] {
            let turn = submit(rt.clone(), cid, q, "text", None).unwrap();
            assert_eq!(
                wait(&rt, turn["turn_id"].as_str().unwrap()),
                "succeeded",
                "{}",
                detail(&rt, cid).unwrap()
            );
            let history = detail(&rt, cid).unwrap();
            let answer = &history["turns"].as_array().unwrap().last().unwrap()["answer"];
            assert_eq!(answer["evidence"], json!([]));
            if experience {
                assert_eq!(answer["blocks"][0]["basis"], "experience");
                assert!(answer["blocks"][0]["text"]
                    .as_str()
                    .unwrap()
                    .starts_with("目前我还没找到相关讨论。"));
            } else {
                assert_eq!(answer["blocks"][0]["kind"], "unanswered");
                assert_eq!(answer["blocks"][0]["reason"], "insufficient_evidence");
                assert!(!answer.to_string().contains("小范围测试"));
            }
        }
        let turn = submit(rt.clone(), cid, "合成格式失败问题", "text", None).unwrap();
        assert_eq!(
            wait(&rt, turn["turn_id"].as_str().unwrap()),
            "succeeded",
            "{}",
            detail(&rt, cid).unwrap()
        );
        let history = detail(&rt, cid).unwrap();
        let failure = &history["turns"].as_array().unwrap().last().unwrap()["answer"];
        assert_eq!(failure["blocks"][0]["reason"], "validation_failed");
        assert!(!failure.to_string().contains("根据我的"));
        assert!(failure["blocks"][0]["text"]
            .as_str()
            .unwrap()
            .contains("格式或引用校验"));
        server.join().unwrap();
    }
    #[test]
    fn unanswered_voice_turns_remain_null_without_fabricated_answer_objects() {
        let (_tmp, rt) = setup();
        let (mid, _) = seed(&rt, "合成白屏回归", "合成原文");
        let convo = open(&rt, Some(mid), false).unwrap();
        let cid = convo["id"].as_str().unwrap();
        let tid = id();
        rt.db().unwrap().execute("INSERT INTO assistant_turns(id,conversation_id,epoch,question,source,state,created_at) VALUES (?,?,0,'合成语音问题','voice','pending',?)",params![tid,cid,now()]).unwrap();
        for state in ["pending", "running", "failed", "cancelled"] {
            rt.db()
                .unwrap()
                .execute(
                    "UPDATE assistant_turns SET state=? WHERE id=?",
                    params![state, tid],
                )
                .unwrap();
            let d = detail(&rt, cid).unwrap();
            assert!(
                d["turns"][0]["answer"].is_null(),
                "{state}: {}",
                d["turns"][0]["answer"]
            );
        }
    }
    #[test]
    fn frozen_scope_and_fact_sources_cannot_cross_meetings() {
        let (_tmp, rt) = setup();
        let (a, sid) = seed(&rt, "A", "目标是降低交付等待");
        let (b, _) = seed(&rt, "B", "B的私有原文");
        let ca = open(&rt, Some(a.clone()), false).unwrap();
        let cb = open(&rt, Some(b), false).unwrap();
        assert_ne!(ca["id"], cb["id"]);
        let snap = snapshot(&rt, Some(&a), &format!("selection:{sid}")).unwrap();
        assert!(!snap.to_string().contains("B的私有原文"));
        let mut seen = HashMap::new();
        let methods = tool(
            &rt,
            "unused",
            "目标",
            &snap,
            &json!({"name":"get_methods","args":{}}),
            &mut seen,
            10 * 1024,
        )
        .unwrap();
        assert_eq!(methods["rows"][0]["kind"], "method");
        assert!(validate_answer(r#"{"blocks":[{"kind":"meeting_fact","text":"假共识","sources":["method:duozhou-v1"]}]}"#,&seen).is_err());
        assert!(validate_answer(r#"{"blocks":[{"kind":"suggestion","text":"核对验收条件","sources":["method:duozhou-v1"]}]}"#,&seen).is_ok());
    }
    #[test]
    fn answer_validation_repairs_common_provider_wrappers_and_reference_aliases() {
        let mut seen = HashMap::new();
        seen.insert(
            "meeting:12345678-1234-4234-8234-123456789abc:7".into(),
            json!({"source_id":"meeting:12345678-1234-4234-8234-123456789abc:7","kind":"meeting","text":"已读取原文"}),
        );
        let answer = validate_answer(
            r#"{"answer":{"blocks":[{"kind":"meeting_fact","text":"原文提到目标。","evidence":[{"source_id":"meeting:12345678-1234-4234-8234-123456789abc:7"}]}]}}"#,
            &seen,
        )
        .unwrap();
        assert_eq!(
            answer["blocks"][0]["sources"][0],
            "meeting:12345678-1234-4234-8234-123456789abc:7"
        );
    }
    #[test]
    fn paragraph_selection_freezes_every_original_member_and_keeps_revisions() {
        let (_tmp, rt) = setup();
        let (mid, first) = seed(&rt, "合成段落提问", "第一句。");
        let second = id();
        {
            let db = rt.db().unwrap();
            db.execute(
                "INSERT INTO transcript_segments VALUES (?,?,1000,2000,'mixed',1,'final')",
                params![second, mid],
            )
            .unwrap();
            db.execute(
                "INSERT INTO transcript_revisions VALUES (?,1,?,'第二句。','asr',?)",
                params![second, mid, now()],
            )
            .unwrap();
        }
        let snap = snapshot(&rt, Some(&mid), &format!("selection:p-{first}")).unwrap();
        let rows = snap["evidence"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e["kind"] == "meeting")
            .collect::<Vec<_>>();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["paragraph_id"], rows[1]["paragraph_id"]);
        assert_eq!(rows[0]["revision"], 1);
        let one = snapshot(&rt, Some(&mid), &format!("selection:{first}")).unwrap();
        assert_eq!(
            one["evidence"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|e| e["kind"] == "meeting")
                .count(),
            1
        );
    }
    #[test]
    fn confirmed_todo_is_persistent_idempotent_and_separate_from_meeting_facts() {
        let (_tmp, rt) = setup();
        let (mid, _) = seed(&rt, "合成待办会议", "待核对目标");
        rt.db()
            .unwrap()
            .execute("UPDATE meetings SET context_consent=1 WHERE id=?", [&mid])
            .unwrap();
        let c = open(&rt, Some(mid.clone()), false).unwrap();
        let cid = c["id"].as_str().unwrap();
        let turn = id();
        rt.db().unwrap().execute("INSERT INTO assistant_turns(id,conversation_id,epoch,question,source,state,created_at) VALUES (?,?,0,'记下核对目标','text','succeeded',?)",params![turn,cid,now()]).unwrap();
        let snap = snapshot(&rt, Some(&mid), "all").unwrap();
        let request = json!({"name":"propose_action","args":{"action":"todo","title":"核对目标","text":"与团队核对本周交付目标"}});
        let mut seen = HashMap::new();
        let p = tool(
            &rt,
            &turn,
            "记下核对目标",
            &snap,
            &request,
            &mut seen,
            10 * 1024,
        )
        .unwrap();
        let pid = p["proposal_id"].as_str().unwrap();
        assert!(rt.detail(&mid).unwrap()["actions"]
            .as_array()
            .unwrap()
            .is_empty());
        let saved = accept(&rt, pid).unwrap();
        assert_eq!(accept(&rt, pid).unwrap(), saved);
        let d = rt.detail(&mid).unwrap();
        assert_eq!(d["actions"].as_array().unwrap().len(), 1);
        assert!(d["actions"][0]["owner_text"].is_null());
        assert!(d["actions"][0]["due_text"].is_null());
        assert_eq!(d["actions"][0]["confirmation_state"], "confirmed");
        assert_eq!(d["actions"][0]["execution_state"], "todo");
        assert_eq!(
            rt.db()
                .unwrap()
                .query_row(
                    "SELECT origin FROM action_revisions WHERE action_id=?",
                    [pid],
                    |r| r.get::<_, String>(0)
                )
                .unwrap(),
            "human"
        );
        assert!(d["artifacts"].as_array().unwrap().is_empty());
        assert!(rt.capture.lock().unwrap().is_none());
        assert!(
            snapshot(&rt, Some(&mid), "all").unwrap()["context"]["user_records"]["actions"]
                .as_array()
                .unwrap()
                .len()
                == 1
        );
        let with_record = snapshot(&rt, Some(&mid), "all").unwrap();
        tool(
            &rt,
            &turn,
            "回顾记录",
            &with_record,
            &json!({"name":"get_context"}),
            &mut seen,
            10 * 1024,
        )
        .unwrap();
        let record = &with_record["context"]["user_records"]["actions"][0]["source_id"];
        let answer = json!({"blocks":[{"kind":"user_record","text":"你保存了核对目标这项待办。","sources":[record]}]});
        assert!(validate_answer(&answer.to_string(), &seen).is_ok());
        let mut fake_fact = answer;
        fake_fact["blocks"][0]["kind"] = json!("meeting_fact");
        assert!(validate_answer(&fake_fact.to_string(), &seen).is_err());
        rt.db()
            .unwrap()
            .execute("UPDATE meetings SET context_consent=0 WHERE id=?", [&mid])
            .unwrap();
        let hidden = snapshot(&rt, Some(&mid), "all").unwrap();
        assert!(hidden["context"]["user_records"].is_null());
        assert!(!hidden["evidence"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["kind"] == "record"));
        assert!(tool(
            &rt,
            &turn,
            "记下",
            &json!({"context":{"kind":"general","meeting_id":null}}),
            &request,
            &mut seen,
            10 * 1024
        )
        .is_err());
        let p=tool(&rt,&turn,"再记下",&snap,&json!({"name":"propose_action","args":{"action":"todo","title":"另一项","text":"合成第二项任务"}}),&mut seen, 10 * 1024).unwrap();
        rt.db()
            .unwrap()
            .execute(
                "UPDATE meetings SET generation=generation+1 WHERE id=?",
                [&mid],
            )
            .unwrap();
        assert!(accept(&rt, p["proposal_id"].as_str().unwrap()).is_err());
    }
    #[test]
    fn accepted_drafts_are_idempotent_and_never_start_capture() {
        let (_tmp, rt) = setup();
        let c = open(&rt, None, true).unwrap();
        let cid = c["id"].as_str().unwrap();
        let turn = id();
        rt.db().unwrap().execute("INSERT INTO assistant_turns(id,conversation_id,epoch,question,source,state,created_at) VALUES (?,?,0,'准备周会','text','succeeded',?)",params![turn,cid,now()]).unwrap();
        let mut seen = HashMap::new();
        let result=tool(&rt,&turn,"准备周会",&json!({"context":{"kind":"general"},"evidence":[]}),&json!({"name":"propose_action","args":{"action":"prepare_meeting","title":"合成周会","text":"讨论卡点和下一步","meeting_type":"weekly"}}),&mut seen, 10 * 1024).unwrap();
        let pid = result["proposal_id"].as_str().unwrap();
        let first = accept(&rt, pid).unwrap();
        assert_eq!(accept(&rt, pid).unwrap(), first);
        assert_eq!(rt.detail(pid).unwrap()["state"], "ready");
        assert!(rt.capture.lock().unwrap().is_none());
    }
    #[test]
    fn tool_pages_count_escaped_wire_bytes_and_never_register_unseen_suffixes() {
        let source = json!({"source_id":"meeting:synthetic:1","kind":"meeting",
            "meeting_id":"host-only-meeting-id","segment_id":"host-only-segment-id",
            "revision":1,"start_ms":0,"text":"中文\"引号\\换行\n".repeat(500)});
        let rows = vec![source.clone()];
        let mut restored = String::new();
        let mut offset = 0;
        let mut seen = HashMap::new();
        loop {
            let page = evidence_page(&rows, 0, offset, 1800);
            assert!(tool_wire_bytes(&page) <= 1800);
            assert_ne!(page["budget_exhausted"], true);
            let row = &page["rows"][0];
            assert!(row["meeting_id"].is_null());
            assert!(row["segment_id"].is_null());
            restored.push_str(row["text"].as_str().unwrap());
            remember_returned(&page, &rows, &mut seen);
            assert_eq!(seen["meeting:synthetic:1"]["partial"], true);
            assert_eq!(
                seen["meeting:synthetic:1"]["segment_id"],
                "host-only-segment-id"
            );
            if page["next_offset"].is_null() {
                break;
            }
            let next = page["next_char_offset"].as_u64().unwrap() as usize;
            assert!(next > offset);
            offset = next;
        }
        assert_eq!(restored, source["text"].as_str().unwrap());
        let no_space = evidence_page(&rows, 0, 0, 512);
        assert!(tool_wire_bytes(&no_space) <= 512);
        assert_eq!(no_space["partial"], true);
        let context = json!({"kind":"meeting","meeting_id":"synthetic","scope":"all","context_allowed":true,
            "user_records":{"notes":[{"source_id":"record:secret","text":"未返回笔记".repeat(1000)}],"actions":[]},
            "meeting_memory":{"facts":[{"text":"历史索引".repeat(1000)}]}});
        let bounded = bounded_context(&context, 1024);
        assert!(tool_wire_bytes(&bounded) <= 1024);
        assert_eq!(bounded["context_allowed"], true);
        assert_eq!(bounded["partial"], true);
        let mut hidden = HashMap::new();
        remember_returned(
            &bounded,
            &[json!({"source_id":"record:secret","kind":"record","text":"未返回笔记"})],
            &mut hidden,
        );
        assert!(hidden.is_empty());
    }
    #[test]
    fn meeting_pagination_respects_capacity_and_returns_full_or_explicit_partial_coverage() {
        for count in [84, 207] {
            let (_tmp, rt) = setup();
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            models::save(
                &rt,
                models::ProfileInput {
                    profile_id: None,
                    expected_version: None,
                    display_name: "合成32768窗口".into(),
                    base_url: format!("http://{}", listener.local_addr().unwrap()),
                    model: "fake-tools".into(),
                    context_tokens: 32768,
                    max_output_tokens: 4096,
                    timeout_seconds: 10,
                    key_action: "clear".into(),
                    api_key: String::new(),
                },
            )
            .unwrap();
            let (mid, _) = seed(&rt, "合成分页会议", "合成目标是核对范围。");
            {
                let db = rt.db().unwrap();
                db.execute(
                    "UPDATE meetings SET state='recording',recording_duration_ms=173489 WHERE id=?",
                    [&mid],
                )
                .unwrap();
                db.execute("INSERT INTO recording_sources(meeting_id,source,state,device) VALUES (?,'microphone','active',0)",[&mid]).unwrap();
                for n in 1..count {
                    let sid = id();
                    db.execute(
                        "INSERT INTO transcript_segments VALUES (?,?,?,?,'mixed',1,'final')",
                        params![sid, mid, n * 2000, n * 2000 + 1000],
                    )
                    .unwrap();
                    db.execute(
                        "INSERT INTO transcript_revisions VALUES (?,1,?,?,'asr',?)",
                        params![sid, mid, "合成目标是核对范围。", now()],
                    )
                    .unwrap();
                }
            }
            let c = open(&rt, Some(mid), false).unwrap();
            let cid = c["id"].as_str().unwrap();
            let server = std::thread::spawn(move || {
                let mut first_source = Value::Null;
                let mut largest = 0;
                for n in 0..6 {
                    let (mut stream, _) = listener.accept().unwrap();
                    let request = receive(&mut stream);
                    largest = largest.max(request.to_string().len());
                    assert!(request.to_string().len() <= 128 * 1024);
                    assert!(meeting_core::budget::estimate(&request.to_string()) <= 25395);
                    let messages = request["messages"].as_array().unwrap();
                    let last = messages
                        .iter()
                        .rev()
                        .find(|m| m["role"] == "tool")
                        .map(|m| {
                            serde_json::from_str::<Value>(m["content"].as_str().unwrap()).unwrap()
                        });
                    let pack = messages
                        .iter()
                        .filter_map(|m| m["content"].as_str())
                        .filter_map(|raw| serde_json::from_str::<Value>(raw).ok())
                        .find_map(|v| v.get("context_pack").cloned())
                        .unwrap();
                    assert!(pack["context"].get("behavior").is_none());
                    if first_source.is_null() {
                        first_source = pack["rows"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .find(|r| r["kind"] == "meeting")
                            .map_or(Value::Null, |r| r["source_id"].clone());
                    }
                    let full_pack = pack["coverage"]["full_transcript"] == true;
                    let message = if n == 0 && !full_pack {
                        json!({"content":null,"tool_calls":[{"id":format!("context-{n}"),"type":"function","function":{"name":"get_context","arguments":"{}"}}]})
                    } else {
                        let offset = last
                            .as_ref()
                            .and_then(|v| v["next_offset"].as_u64())
                            .unwrap_or(0);
                        if let Some(data) = last.as_ref().filter(|v| v["rows"].is_array()) {
                            if data["budget_exhausted"] == true {
                                assert_eq!(data["partial"], true);
                            } else {
                                assert!(!data["rows"].as_array().unwrap().is_empty());
                            }
                            if first_source.is_null() {
                                first_source = data["rows"][0]["source_id"].clone();
                            }
                            assert!(data["rows"][0]["meeting_id"].is_null());
                        }
                        let complete = full_pack
                            || last.as_ref().is_some_and(|v| {
                                v["rows"].is_array() && v["next_offset"].is_null()
                            });
                        let exhausted = last.as_ref().is_some_and(|v| {
                            v["budget_exhausted"] == true || v["retrieval_exhausted"] == true
                        });
                        if complete
                            || exhausted
                            || request["tools"].as_array().is_none_or(Vec::is_empty)
                        {
                            assert!(n <= 4, "must leave the final model call for repair");
                            let mut blocks = vec![
                                json!({"kind":"meeting_fact","text":"已读原文提到核对范围。","sources":[first_source]}),
                            ];
                            if !complete {
                                blocks.push(json!({"kind":"unanswered","text":"我只读取了部分原文，尚未完整覆盖整场会议。","sources":[]}));
                            }
                            json!({"content":json!({"blocks":blocks}).to_string()})
                        } else {
                            json!({"content":null,"tool_calls":[{"id":format!("page-{n}"),"type":"function","function":{"name":"get_transcript","arguments":json!({"offset":offset}).to_string()}}]})
                        }
                    };
                    let finished = message["tool_calls"].is_null();
                    let body=json!({"choices":[{"finish_reason":if finished{"stop"}else{"tool_calls"},"message":message}]}).to_string();
                    write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
                    if finished {
                        break;
                    }
                }
                largest
            });
            let turn =
                submit(rt.clone(), cid, "帮我核对整场会议目标", "voice", Some(id())).unwrap();
            assert_eq!(
                wait(&rt, turn["turn_id"].as_str().unwrap()),
                "succeeded",
                "{}",
                detail(&rt, cid).unwrap()
            );
            let largest = server.join().unwrap();
            assert!(largest > 25395);
            let response = detail(&rt, cid).unwrap()["turns"][0]["answer"].clone();
            let returned = response["coverage"]["transcript_returned"]
                .as_u64()
                .unwrap();
            assert_eq!(response["coverage"]["transcript_total"], count);
            if count == 84 {
                assert_eq!(returned, count as u64);
            } else {
                assert!(returned > 0 && returned < count as u64);
                assert!(response["blocks"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|b| b["kind"] == "unanswered"
                        && b["text"].as_str().unwrap().contains("部分")));
            }
            assert!(response["evidence"][0]["segment_id"].is_string());
        }
    }
    #[test]
    fn provider_capacity_error_restarts_once_with_smaller_evidence_and_same_question() {
        let (_tmp, rt) = setup();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        profile(&rt, &format!("http://{}", listener.local_addr().unwrap()));
        let (mid, _) = seed(&rt, "合成容量降级", &"核对范围。".repeat(8000));
        let c = open(&rt, Some(mid), false).unwrap();
        let cid = c["id"].as_str().unwrap();
        let server = std::thread::spawn(move || {
            let mut first_page = 0;
            for n in 0..4 {
                let (mut stream, _) = listener.accept().unwrap();
                let request = receive(&mut stream);
                assert!(request.to_string().contains("合成容量问题"));
                let tool = request["messages"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .rev()
                    .find(|m| m["role"] == "tool")
                    .map(|m| {
                        serde_json::from_str::<Value>(m["content"].as_str().unwrap()).unwrap()
                    });
                let (code, body) = if n == 1 {
                    first_page = tool.unwrap()["rows"][0]["text"].as_str().unwrap().len();
                    (
                        400,
                        json!({"error":{"code":"context_length_exceeded","message":"maximum context length"}}),
                    )
                } else if n == 3 {
                    let data = tool.unwrap();
                    assert!(data["rows"][0]["text"].as_str().unwrap().len() < first_page);
                    (
                        200,
                        json!({"choices":[{"finish_reason":"stop","message":{"content":json!({"blocks":[{"kind":"meeting_fact","text":"我看到需要核对范围。","sources":[data["rows"][0]["source_id"]]}]}).to_string()}}]}),
                    )
                } else {
                    (
                        200,
                        json!({"choices":[{"finish_reason":"tool_calls","message":{"content":null,"tool_calls":[{"id":format!("raw-{n}"),"type":"function","function":{"name":"get_transcript","arguments":"{\"offset\":0}"}}]}}]}),
                    )
                };
                let body = body.to_string();
                write!(stream,"HTTP/1.1 {code} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
            }
        });
        let turn = submit(rt.clone(), cid, "合成容量问题", "text", None).unwrap();
        assert_eq!(
            wait(&rt, turn["turn_id"].as_str().unwrap()),
            "succeeded",
            "{}",
            detail(&rt, cid).unwrap()
        );
        server.join().unwrap();
        let d = detail(&rt, cid).unwrap();
        assert_eq!(d["turns"].as_array().unwrap().len(), 1);
        assert_eq!(
            d["turns"][0]["answer"]["coverage"]["transcript_returned"],
            0
        );
    }
    #[test]
    fn real_pi_broker_unifies_voice_and_text_history_and_deduplicates() {
        let (_tmp, rt) = setup();
        let mut preferences = settings(&rt).unwrap();
        preferences["wake_word"] = json!("小帆同学");
        save_settings(&rt, preferences).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        profile(
            &rt,
            &format!("http://{}/v1", listener.local_addr().unwrap()),
        );
        let (mid, _) = seed(&rt, "合成会议", "目标是本周交付，不虚构负责人");
        rt.db()
            .unwrap()
            .execute("UPDATE meetings SET state='recording' WHERE id=?", [&mid])
            .unwrap();
        rt.db().unwrap().execute("INSERT INTO recording_sources(meeting_id,source,state,device) VALUES (?,'microphone','active',0)",[&mid]).unwrap();
        let c = open(&rt, None, false).unwrap();
        assert_eq!(c["meeting_id"], mid);
        let cid = c["id"].as_str().unwrap();
        let server = std::thread::spawn(move || {
            let mut requests = vec![];
            for n in 0..4 {
                let (mut s, _) = listener.accept().unwrap();
                let request = receive(&mut s);
                let message = if n % 2 == 0 {
                    json!({"role":"assistant","content":null,"tool_calls":[{"id":format!("call-{n}"),"type":"function","function":{"name":"get_transcript","arguments":"{\"offset\":0}"}}]})
                } else {
                    let data = request["messages"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .rev()
                        .find(|m| m["role"] == "tool")
                        .unwrap()["content"]
                        .as_str()
                        .unwrap();
                    let data: Value = serde_json::from_str(data).unwrap();
                    let source = &data["rows"][0]["source_id"];
                    json!({"role":"assistant","content":json!({"blocks":[{"kind":"meeting_fact","text":"目标是本周交付；负责人尚待确认。","sources":[source]}]}).to_string()})
                };
                let body=json!({"choices":[{"message":message,"finish_reason":if n%2==0{"tool_calls"}else{"stop"}}]}).to_string();
                write!(s,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
                requests.push(request);
            }
            requests
        });
        let utterance = id();
        let first = submit(
            rt.clone(),
            cid,
            "合成语音问题：目标是什么？",
            "voice",
            Some(utterance.clone()),
        )
        .unwrap();
        let t = first["turn_id"].as_str().unwrap();
        assert_eq!(
            submit(
                rt.clone(),
                cid,
                "合成语音问题：目标是什么？",
                "voice",
                Some(utterance)
            )
            .unwrap(),
            first
        );
        assert_eq!(wait(&rt, t), "succeeded", "{}", detail(&rt, cid).unwrap());
        let second = submit(
            rt.clone(),
            cid,
            "合成文字追问：还有哪些不明确？",
            "text",
            Some(id()),
        )
        .unwrap();
        assert_eq!(wait(&rt, second["turn_id"].as_str().unwrap()), "succeeded");
        let requests = server.join().unwrap();
        for request in &requests {
            let system = request["messages"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|message| message["role"] == "system")
                .filter_map(|message| message["content"].as_str())
                .collect::<Vec<_>>()
                .join("\n");
            assert!(system.contains("你的名字是“小帆同学”"));
            assert!(system.contains("必须以第一人称"));
        }
        assert!(requests[2].to_string().contains("合成语音问题"));
        assert!(requests[2].to_string().contains("目标是本周交付"));
        let d = detail(&rt, cid).unwrap();
        assert_eq!(d["turns"].as_array().unwrap().len(), 2);
        assert_eq!(d["turns"][0]["source"], "voice");
        assert_eq!(d["turns"][1]["source"], "text");
        assert!(!d.to_string().contains("credential_ref"));
        clear(&rt, cid).unwrap();
        assert!(detail(&rt, cid).unwrap()["turns"]
            .as_array()
            .unwrap()
            .is_empty());
    }
    #[test]
    fn large_history_is_compacted_persisted_and_reused_without_remote_services() {
        let (_tmp, mut rt) = setup();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        profile(&rt, &format!("http://{}", listener.local_addr().unwrap()));
        let c = open(&rt, None, true).unwrap();
        let cid = c["id"].as_str().unwrap();
        let context = snapshot(&rt, None, "all").unwrap()["context"].clone();
        let config = configuration(&rt, None).unwrap();
        for n in 0..9 {
            rt.db().unwrap().execute("INSERT INTO assistant_turns(id,conversation_id,epoch,question,source,state,response_json,model_json,snapshot_json,created_at) VALUES (?,?,0,?,'text','succeeded',?,?,?,?)",params![id(),cid,format!("第{n}轮合成目标{}","范围待核对".repeat(70)),json!({"blocks":[{"kind":"suggestion","text":"核对验收".repeat(70),"sources":[]}]}).to_string(),serde_json::to_string(&config).unwrap(),json!({"context":context}).to_string(),now()]).unwrap();
        }
        let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop = done.clone();
        let server = std::thread::spawn(move || {
            let mut compact = 0;
            let mut answers = 0;
            while !stop.load(std::sync::atomic::Ordering::Acquire) {
                let (mut stream, _) = match listener.accept() {
                    Ok(v) => v,
                    Err(_) => {
                        std::thread::sleep(Duration::from_millis(10));
                        continue;
                    }
                };
                let input = receive(&mut stream);
                let content = if input["tools"].is_null() {
                    compact += 1;
                    "合成目标是核对范围和验收，尚未确认会议决定。".to_string()
                } else {
                    answers += 1;
                    assert!(input.to_string().contains("合成目标"));
                    json!({"blocks":[{"kind":"suggestion","text":"先核对范围与验收。","sources":[]}]}).to_string()
                };
                let body=json!({"usage":{"prompt_tokens":500,"completion_tokens":40,"total_tokens":540},"choices":[{"message":{"content":content},"finish_reason":"stop"}]}).to_string();
                write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
            }
            (compact, answers)
        });
        let first = submit(rt.clone(), cid, "回顾范围", "text", None).unwrap();
        let state = wait(&rt, first["turn_id"].as_str().unwrap());
        assert_eq!(state, "succeeded", "{}", detail(&rt, cid).unwrap());
        let count: usize = rt
            .db()
            .unwrap()
            .query_row("SELECT count(*) FROM assistant_compactions", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert!(count > 0);
        let root = rt.root.clone();
        for h in rt.analysis_threads.lock().unwrap().drain(..) {
            h.join().unwrap();
        }
        drop(rt);
        rt = Runtime::open_with_vault(root, Arc::new(models::tests::MemoryVault::default()))
            .unwrap();
        let second = submit(rt.clone(), cid, "继续核对", "text", None).unwrap();
        assert_eq!(wait(&rt, second["turn_id"].as_str().unwrap()), "succeeded");
        assert_eq!(
            rt.db()
                .unwrap()
                .query_row("SELECT count(*) FROM assistant_compactions", [], |r| r
                    .get::<_, usize>(0))
                .unwrap(),
            count
        );
        done.store(true, std::sync::atomic::Ordering::Release);
        let (compact, answers) = server.join().unwrap();
        assert!(compact > 0);
        assert_eq!(answers, 2);
        assert_eq!(
            detail(&rt, cid).unwrap()["turns"].as_array().unwrap().len(),
            11
        );
        clear(&rt, cid).unwrap();
        let snap = snapshot(&rt, None, "all").unwrap();
        assert!(history(
            &rt,
            cid,
            second["turn_id"].as_str().unwrap(),
            &snap["context"]
        )
        .unwrap()
        .is_empty());
    }
    #[test]
    fn composed_template_and_one_turn_mode_reach_same_model_and_persist_in_detail() {
        let (_tmp, rt) = setup();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        profile(
            &rt,
            &format!("http://{}/v1", listener.local_addr().unwrap()),
        );
        let (mid, _) = seed(&rt, "合成组合规则", "合成原文");
        let custom=crate::templates::save(&rt,crate::templates::Input{id:None,name:"合成业务模板".into(),instructions:"合成纪要专用规则".into(),policy:Some(json!({"goal":"合成业务目标","criteria":"合成业务标准","qa_rules":"合成当前问答规则"})),expected_version:None}).unwrap();
        rt.db()
            .unwrap()
            .execute(
                "UPDATE meetings SET assistant_id=?,template_version=1 WHERE id=?",
                params![custom["id"].as_str().unwrap(), mid],
            )
            .unwrap();
        let convo = open(&rt, Some(mid), false).unwrap();
        let cid = convo["id"].as_str().unwrap();
        let server = std::thread::spawn(move || {
            for mode in ["judge", "auto"] {
                let (mut stream, _) = listener.accept().unwrap();
                let input = receive(&mut stream);
                let system = input["messages"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|m| m["role"] == "system")
                    .unwrap()["content"]
                    .as_str()
                    .unwrap();
                for text in ["合成业务目标", "合成业务标准", "合成当前问答规则"]
                {
                    assert!(system.contains(text), "{text}");
                }
                assert!(system.contains(&format!("本轮问答方式：\"{mode}\"")));
                assert!(!system.contains("合成纪要专用规则"));
                let content=json!({"mode":if mode=="auto" {"clarify"} else {mode},"blocks":[{"kind":"suggestion","text":"合成回应。","sources":[]}]}).to_string();
                let body =
                    json!({"choices":[{"message":{"content":content},"finish_reason":"stop"}]})
                        .to_string();
                write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
            }
        });
        for mode in ["judge", "auto"] {
            let turn = submit_with_mode(
                rt.clone(),
                cid,
                "合成问题",
                "text",
                None,
                "all",
                false,
                mode,
            )
            .unwrap();
            assert_eq!(
                wait(&rt, turn["turn_id"].as_str().unwrap()),
                "succeeded",
                "{}",
                detail(&rt, cid).unwrap()
            );
        }
        server.join().unwrap();
        let d = detail(&rt, cid).unwrap();
        assert_eq!(d["turns"][0]["policy"]["answer_mode"], "judge");
        assert_eq!(d["turns"][1]["policy"]["answer_mode"], "auto");
        assert_eq!(d["turns"][1]["policy"]["actual_mode"], "clarify");
        assert_eq!(d["template"]["version"], 1);
    }
    #[test]
    fn waking_during_model_thinking_cancels_inflight_turn_and_new_question_wins() {
        let (_tmp, rt) = setup();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        profile(
            &rt,
            &format!("http://{}/v1", listener.local_addr().unwrap()),
        );
        let (mid, _) = seed(&rt, "合成思考打断", "合成原文");
        rt.db()
            .unwrap()
            .execute("UPDATE meetings SET state='recording' WHERE id=?", [&mid])
            .unwrap();
        rt.db().unwrap().execute("INSERT INTO recording_sources(meeting_id,source,state,device) VALUES (?,'microphone','active',0)",[&mid]).unwrap();
        let c = open(&rt, Some(mid.clone()), false).unwrap();
        let cid = c["id"].as_str().unwrap();
        let (arrived, receiving) = std::sync::mpsc::sync_channel(1);
        let server = std::thread::spawn(move || {
            let until = Instant::now() + Duration::from_secs(10);
            let accept = || loop {
                if let Ok((stream, _)) = listener.accept() {
                    stream.set_nonblocking(false).unwrap();
                    break stream;
                }
                assert!(
                    Instant::now() < until,
                    "new question was blocked by cancelled model request"
                );
                std::thread::sleep(Duration::from_millis(10));
            };
            let mut old = accept();
            let old_request = receive(&mut old);
            assert!(!old_request.to_string().contains("合成新版人设"));
            arrived.send(()).unwrap(); // first request deliberately has no response
            let mut new = accept();
            let input = receive(&mut new);
            assert!(input.to_string().contains("新的合成问题"));
            assert!(input.to_string().contains("合成新版人设"));
            let content =
                json!({"blocks":[{"kind":"suggestion","text":"合成新回答。","sources":[]}]})
                    .to_string();
            let body = json!({"choices":[{"message":{"content":content},"finish_reason":"stop"}]})
                .to_string();
            write!(new,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
            // A late response to the old request must never become a new answer.
            let _ = write!(
                old,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        });
        let initial_behavior_version = crate::assistant_policy::current(&rt).unwrap()["version"]
            .as_u64()
            .unwrap();
        let first = submit(rt.clone(), cid, "旧的合成问题", "voice", Some(id())).unwrap();
        receiving.recv_timeout(Duration::from_secs(6)).unwrap();
        let began = Instant::now();
        crate::voice::interrupt_answer(&rt, &mid).unwrap();
        let mut preferences = settings(&rt).unwrap();
        preferences["behavior"]["persona_prompt"] = json!("合成新版人设");
        save_settings(&rt, preferences).unwrap();
        let second = submit(rt.clone(), cid, "新的合成问题", "voice", Some(id())).unwrap();
        assert_eq!(wait(&rt, second["turn_id"].as_str().unwrap()), "succeeded");
        assert!(began.elapsed() < Duration::from_secs(4));
        server.join().unwrap();
        for h in rt.analysis_threads.lock().unwrap().drain(..) {
            h.join().unwrap();
        }
        assert_eq!(wait(&rt, first["turn_id"].as_str().unwrap()), "cancelled");
        let snapshots: Vec<String> = rt
            .db()
            .unwrap()
            .prepare(
                "SELECT snapshot_json FROM assistant_turns WHERE conversation_id=? ORDER BY rowid",
            )
            .unwrap()
            .query_map([cid], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&snapshots[0]).unwrap()["context"]["behavior"]["version"],
            initial_behavior_version
        );
        assert_eq!(
            serde_json::from_str::<Value>(&snapshots[1]).unwrap()["context"]["behavior"]["version"],
            initial_behavior_version + 1
        );
        let turns = detail(&rt, cid).unwrap();
        assert!(turns["turns"][0]["answer"].is_null());
        assert_eq!(
            turns["turns"][1]["answer"]["blocks"][0]["text"],
            "合成新回答。"
        );
    }
    #[test]
    fn cancellation_kills_queued_agent_before_any_http_dispatch() {
        let (_tmp, rt) = setup();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        profile(
            &rt,
            &format!("http://{}/v1", listener.local_addr().unwrap()),
        );
        let (mid, _) = seed(&rt, "取消合成问题", "合成事实");
        let c = open(&rt, Some(mid), false).unwrap();
        let permit = rt.llm_gate.lock().unwrap();
        let result = submit(
            rt.clone(),
            c["id"].as_str().unwrap(),
            "合成取消问题",
            "text",
            Some(id()),
        )
        .unwrap();
        let turn = result["turn_id"].as_str().unwrap();
        let until = Instant::now() + Duration::from_secs(6);
        while !rt
            .analysis_waiting
            .lock()
            .unwrap()
            .iter()
            .any(|(id, _)| id == turn)
        {
            assert!(Instant::now() < until);
            std::thread::sleep(Duration::from_millis(30));
        }
        cancel(&rt, turn).unwrap();
        let began = Instant::now();
        for thread in rt.analysis_threads.lock().unwrap().drain(..) {
            thread.join().unwrap();
        }
        assert!(began.elapsed() < Duration::from_secs(2));
        assert_eq!(wait(&rt, turn), "cancelled");
        assert!(rt.analysis_waiting.lock().unwrap().is_empty());
        assert!(listener.accept().is_err());
        drop(permit);
    }
    #[test]
    fn withdrawing_general_knowledge_cancels_work_and_does_not_resend_old_answers() {
        let (_tmp, rt) = setup();
        let set = crate::knowledge::save(
            &rt,
            crate::knowledge::Save {
                id: None,
                expected_version: None,
                name: "合成范围资料".into(),
                kind: "documents".into(),
                content: "仅用于范围撤销测试".into(),
                terms: vec![],
            },
        )
        .unwrap();
        let mut prefs = settings(&rt).unwrap();
        prefs["collections"] = json!([{"set_id":set["id"],"version":1}]);
        save_settings(&rt, prefs.clone()).unwrap();
        let c = open(&rt, None, true).unwrap();
        let cid = c["id"].as_str().unwrap();
        let old = snapshot(&rt, None, "all").unwrap();
        let completed = id();
        let pending = id();
        rt.db().unwrap().execute("INSERT INTO assistant_turns(id,conversation_id,epoch,question,source,state,response_json,snapshot_json,created_at) VALUES (?,?,0,'旧范围问题','text','succeeded',?,?,?)",params![completed,cid,json!({"blocks":[{"kind":"suggestion","text":"旧范围答案","sources":[]}],"evidence":[]}).to_string(),json!({"context":old["context"]}).to_string(),now()]).unwrap();
        rt.db().unwrap().execute("INSERT INTO assistant_turns(id,conversation_id,epoch,question,source,state,snapshot_json,created_at) VALUES (?,?,0,'待执行问题','text','pending',?,?)",params![pending,cid,json!({"context":old["context"]}).to_string(),now()]).unwrap();
        assert_eq!(
            history(&rt, cid, &pending, &old["context"]).unwrap().len(),
            2
        );
        prefs["collections"] = json!([]);
        save_settings(&rt, prefs).unwrap();
        assert_eq!(wait(&rt, &pending), "cancelled");
        let current = snapshot(&rt, None, "all").unwrap();
        assert!(history(&rt, cid, &pending, &current["context"])
            .unwrap()
            .is_empty());
        assert!(detail(&rt, cid).unwrap().to_string().contains("旧范围答案"));
    }
}
