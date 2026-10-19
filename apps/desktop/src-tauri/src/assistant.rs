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
    serde_json::from_str(&raw).map_err(err)
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
    let mut db = rt.db()?;
    let tx = db.transaction().map_err(err)?;
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
    crate::voice::invalidate(rt);
    Ok(value)
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
            "与小舵聊一聊".into()
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
    let mut q=db.prepare("SELECT t.id,t.question,t.source,t.state,t.response_json,t.error,t.created_at,t.model_json,a.content_json FROM assistant_turns t LEFT JOIN conversation_turns old ON old.job_id=t.legacy_job_id LEFT JOIN artifacts a ON a.id=old.artifact_id WHERE t.conversation_id=? AND t.epoch=? ORDER BY t.rowid DESC LIMIT 120").map_err(err)?;
    let mut turns=q.query_map(params![cid,epoch],|r|{
        let response:Option<String>=r.get(4)?;let legacy:Option<String>=r.get(8)?;
        let result=response.and_then(|s|serde_json::from_str::<Value>(&s).ok()).or_else(||legacy.and_then(|s|serde_json::from_str::<Value>(&s).ok()).map(|v|json!({"legacy":true,"blocks":v["summary"].as_array().into_iter().flatten().map(|s|json!({"kind":"legacy","text":s["text"],"sources":s["evidence"]})).collect::<Vec<_>>(),"evidence":[]})));
        let model:Option<String>=r.get(7)?;let model=model.and_then(|s|serde_json::from_str::<Value>(&s).ok());
        Ok(json!({"id":r.get::<_,String>(0)?,"question":r.get::<_,String>(1)?,"source":r.get::<_,String>(2)?,"state":r.get::<_,String>(3)?,"answer":result,"error":r.get::<_,Option<String>>(5)?,"created_at":r.get::<_,String>(6)?,"model_name":model.as_ref().map(|m|&m["display_name"])}))
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
    Ok(json!({"id":cid,"meeting_id":mid,"title":title,"turns":turns,"proposals":proposals}))
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
fn snapshot(rt: &Runtime, mid: Option<&str>, scope: &str) -> Result<Value, String> {
    let mut evidence = vec![];
    let context = if let Some(mid) = mid {
        let d = rt.detail(mid)?;
        if d["state"] == "ready" {
            return Err("会议尚未开始，请在通用聊天中准备会议".into());
        }
        let cutoff = d["duration_ms"]
            .as_u64()
            .unwrap_or(0)
            .saturating_sub(600000);
        let selected = scope.strip_prefix("selection:");
        for s in d["segments"].as_array().into_iter().flatten() {
            if (scope == "recent" && s["end_ms"].as_u64().unwrap_or(0) <= cutoff)
                || selected.is_some_and(|id| s["id"].as_str() != Some(id))
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
            evidence.push(json!({"assistant_playback_overlap":overlap,"source_id":format!("meeting:{}:{}",s["id"].as_str().unwrap_or(""),s["revision"]),"kind":"meeting","text":s["text"],"meeting_id":mid,"segment_id":s["id"],"revision":s["revision"],"start_ms":s["start_ms"]}));
        }
        json!({"kind":"meeting","scope":scope,"context_allowed":d["context_consent"],"meeting_id":mid,"title":d["title"],"state":d["state"],"context":if d["context_consent"]==true { d["context"].clone() } else { Value::Null },"template":d["template"],"duration_ms":d["duration_ms"],"asr_through_ms":d["asr_processed_ms"],"generation":d["generation"],"chat_epoch":d["chat_epoch"],"consent_version":d["consent_version"]})
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
            evidence.push(json!({"source_id":format!("knowledge:{sid}:{version}:term-{i}"),"kind":"knowledge","text":format!("标准名称：{}；别名：{}",term["canonical"].as_str().unwrap_or(""),term["aliases"]),"set_id":sid,"version":version,"name":name}));
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
pub fn submit(
    rt: Arc<Runtime>,
    cid: &str,
    question: &str,
    source: &str,
    utterance: Option<String>,
) -> Result<Value, String> {
    submit_scoped(rt, cid, question, source, utterance, "all")
}
pub fn submit_scoped(
    rt: Arc<Runtime>,
    cid: &str,
    question: &str,
    source: &str,
    utterance: Option<String>,
    scope: &str,
) -> Result<Value, String> {
    rt.ensure_running()?;
    valid_uuid(cid)?;
    if !matches!(scope, "all" | "recent")
        && !scope
            .strip_prefix("selection:")
            .is_some_and(|s| valid_uuid(s).is_ok())
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
    let snapshot = snapshot(&rt, mid.as_deref(), scope)?;
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
        let check=||check_turn(&worker_rt,&task,&config,&snapshot);
        let result:Result<(),String>=(||{
            check()?;
            worker_rt.db()?.execute("UPDATE assistant_turns SET state='running' WHERE id=? AND state='pending'",[&task]).map_err(err)?;
            let history=if snapshot["context"]["context_allowed"]==false { vec![] } else { history(&worker_rt,&cid,&task,&snapshot["context"])? };
            let answer=run_pi(&worker_rt,&task,&config,&question,&history,&snapshot,&check)?;
            let _gate=worker_rt.model_gate.lock().map_err(|_|"模型锁异常")?;check()?;
            let mut db=worker_rt.db()?;let tx=db.transaction().map_err(err)?;
            if let Some(mid)=snapshot["context"]["meeting_id"].as_str(){let generation:u64=tx.query_row("SELECT generation FROM meetings WHERE id=?",[mid],|r|r.get(0)).map_err(err)?;if Some(generation)!=snapshot["context"]["generation"].as_u64(){return Err("会议内容已变化，请重新提问".into());}}
            let changed=tx.execute("UPDATE assistant_turns SET state='succeeded',response_json=? WHERE id=? AND state='running' AND epoch=(SELECT epoch FROM assistant_conversations WHERE id=conversation_id)",params![answer.to_string(),task]).map_err(err)?;
            if changed!=1{return Err("本次回答已取消".into());}tx.commit().map_err(err)?;Ok(())
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
fn history(rt: &Runtime, cid: &str, turn: &str, context: &Value) -> Result<Vec<Value>, String> {
    let d = detail(rt, cid)?;
    let mut history = vec![];
    for t in d["turns"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|t| t["id"] != turn && t["state"] == "succeeded")
        .rev()
        .take(8)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
    {
        if context["kind"] == "general" {
            let raw: Option<String> = rt
                .db()?
                .query_row(
                    "SELECT snapshot_json FROM assistant_turns WHERE id=?",
                    [t["id"].as_str().unwrap_or("")],
                    |r| r.get(0),
                )
                .map_err(err)?;
            let old = raw
                .and_then(|s| serde_json::from_str::<Value>(&s).ok())
                .unwrap_or(Value::Null);
            if old["context"]["knowledge_scope"] != context["knowledge_scope"] {
                continue;
            }
        }
        history.push(json!({"role":"user","text":t["question"]}));
        history.push(json!({"role":"assistant","text":t["answer"].to_string()}));
    }
    Ok(history)
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
fn run_pi(
    rt: &Runtime,
    turn: &str,
    config: &ModelConfig,
    question: &str,
    history: &[Value],
    snapshot: &Value,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<Value, String> {
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
    writeln!(input,"{}",json!({"type":"start","question":question,"scope":snapshot["context"],"history":history,"context_tokens":config.context_tokens,"max_tokens":config.max_output_tokens})).map_err(|_|"Agent启动消息失败")?;
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
        let m: Value = serde_json::from_str(&line).map_err(|_| "Agent协议无效")?;
        let result = match m["type"].as_str() {
            Some("ready") => continue,
            Some("tool_request") => {
                calls += 1;
                if calls > 8 {
                    return Err("已达到本轮检索上限".into());
                }
                if m["name"] == "search_evidence" {
                    searches += 1;
                    if searches > 2 {
                        return Err("本轮检索已达上限，请缩小问题范围".into());
                    }
                }
                match tool(rt, turn, question, snapshot, &m, &mut seen) {
                    Ok(v) => v,
                    Err(e) => json!({"error":e,"executed":false}),
                }
            }
            Some("model_request") => {
                models += 1;
                if models > 6 {
                    return Err("已达到本轮模型调用上限".into());
                }
                let tools = m["tools"].as_array().ok_or("Agent工具声明无效")?;
                let names = tools
                    .iter()
                    .filter_map(|t| t["function"]["name"].as_str())
                    .collect::<std::collections::HashSet<_>>();
                if names.len() != TOOLS.len()
                    || tools.len() != TOOLS.len()
                    || tools
                        .iter()
                        .any(|t| !TOOLS.contains(&t["function"]["name"].as_str().unwrap_or("")))
                {
                    return Err("Agent请求了未开放工具".into());
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
                analysis::request_agent(rt, config, m["messages"].clone(), &m["tools"], &bounded)?
            }
            Some("result") => {
                let mut answer =
                    validate_answer(m["text"].as_str().ok_or("Agent没有回答正文")?, &seen)?;
                answer["coverage"] = json!({"scope":snapshot["context"]["scope"],"through_ms":snapshot["context"]["asr_through_ms"],"transcript_returned":seen.values().filter(|e|e["kind"]=="meeting").count(),"transcript_total":snapshot["evidence"].as_array().into_iter().flatten().filter(|e|e["kind"]=="meeting").count()});
                return Ok(answer);
            }
            Some("validate") => match validate_answer(m["text"].as_str().unwrap_or(""), &seen) {
                Ok(_) => json!({"ok":true}),
                Err(e) => json!({"ok":false,"error":e}),
            },
            Some("failure") => return Err("小舵Agent失败，请检查模型工具能力后重试".into()),
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
fn tool(
    rt: &Runtime,
    turn: &str,
    question: &str,
    snapshot: &Value,
    m: &Value,
    seen: &mut HashMap<String, Value>,
) -> Result<Value, String> {
    let all = snapshot["evidence"].as_array().ok_or("依据快照无效")?;
    let rows = match m["name"].as_str() {
        Some("get_context") => return Ok(snapshot["context"].clone()),
        Some("get_methods") => all
            .iter()
            .filter(|v| v["kind"] == "method")
            .cloned()
            .collect::<Vec<_>>(),
        Some("get_transcript") => {
            let offset = m["args"]["offset"].as_u64().unwrap_or(0) as usize;
            let rows = all
                .iter()
                .filter(|v| v["kind"] == "meeting")
                .collect::<Vec<_>>();
            let selected = rows
                .iter()
                .skip(offset)
                .take(24)
                .map(|v| (*v).clone())
                .collect::<Vec<_>>();
            for v in &selected {
                seen.insert(v["source_id"].as_str().unwrap_or("").into(), v.clone());
            }
            return Ok(
                json!({"rows":selected,"offset":offset,"total":rows.len(),"next_offset":(offset+24<rows.len()).then_some(offset+24),"coverage":"仅这些分页已返回；未读部分不能推断不存在"}),
            );
        }
        Some("search_evidence") => {
            let query = m["args"]["query"].as_str().unwrap_or(question);
            if query.len() > 720 {
                return Err("检索词过长".into());
            }
            let folded = query.to_lowercase();
            let chars = folded
                .chars()
                .filter(|c| c.is_alphanumeric())
                .take(100)
                .collect::<Vec<_>>();
            let needles = if chars.len() == 1 {
                vec![chars.iter().collect::<String>()]
            } else {
                chars
                    .windows(2)
                    .map(|s| s.iter().collect::<String>())
                    .collect::<Vec<_>>()
            };
            let mut scored = all
                .iter()
                .map(|e| {
                    (
                        needles
                            .iter()
                            .filter(|n| {
                                e["text"]
                                    .as_str()
                                    .unwrap_or("")
                                    .to_lowercase()
                                    .contains(n.as_str())
                            })
                            .count(),
                        e,
                    )
                })
                .filter(|(s, e)| *s > 0 || e["kind"] == "method")
                .collect::<Vec<_>>();
            scored.sort_by_key(|(s, _)| std::cmp::Reverse(*s));
            scored
                .into_iter()
                .take(10)
                .map(|(_, e)| e.clone())
                .collect()
        }
        Some("propose_action") => {
            let a = &m["args"];
            let kind = a["action"].as_str().ok_or("操作草稿无效")?;
            if !matches!(kind, "prepare_meeting" | "note") {
                return Err("不支持该操作草稿".into());
            }
            let title = a["title"].as_str().unwrap_or("新会议");
            let text = a["text"].as_str().unwrap_or("");
            if title.len() > 360 || text.len() > 9000 {
                return Err("操作草稿过长".into());
            }
            if kind == "note" && snapshot["context"]["meeting_id"].is_null() {
                return Err("记笔记需要明确会议".into());
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
    for v in &rows {
        seen.insert(v["source_id"].as_str().unwrap_or("").into(), v.clone());
    }
    Ok(json!({"rows":rows,"coverage":"这是检索结果，不代表整场完整扫描"}))
}
pub fn validate_answer(text: &str, seen: &HashMap<String, Value>) -> Result<Value, String> {
    let value = analysis::parse(text)?;
    let blocks = value["blocks"].as_array().ok_or("小舵回答结构无效")?;
    if blocks.is_empty() || blocks.len() > 16 {
        return Err("小舵回答条目无效".into());
    }
    let mut used = HashMap::new();
    for b in blocks {
        let kind = b["kind"].as_str().ok_or("回答类型无效")?;
        if !matches!(
            kind,
            "meeting_fact" | "knowledge" | "suggestion" | "unanswered"
        ) || b["text"]
            .as_str()
            .is_none_or(|t| t.trim().is_empty() || t.len() > 9000)
        {
            return Err("小舵回答内容无效".into());
        }
        let refs = b["sources"].as_array().ok_or("回答引用无效")?;
        if refs.len() > 20 || (matches!(kind, "meeting_fact" | "knowledge") && refs.is_empty()) {
            return Err("小舵事实缺少依据".into());
        }
        for reference in refs {
            let key = reference.as_str().ok_or("引用ID无效")?;
            let e = seen.get(key).ok_or("小舵引用了未检索的依据")?;
            if kind == "meeting_fact" && e["assistant_playback_overlap"] == true {
                return Err(
                    "此段与助手播报重叠，不能确认为会议事实；请标为待澄清或使用其他原文".into(),
                );
            }
            if (kind == "meeting_fact" && e["kind"] != "meeting")
                || (kind == "knowledge" && e["kind"] != "knowledge")
            {
                return Err("资料不能作为会议共识引用".into());
            }
            used.insert(key, e);
        }
    }
    Ok(
        json!({"blocks":blocks,"evidence":used.values().collect::<Vec<_>>(),"engine":"pi-coding-agent/1.3.0"}),
    )
}

struct QueueEntry<'a> {
    rt: &'a Runtime,
    id: String,
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
        )
        .unwrap();
        assert_eq!(methods["rows"][0]["kind"], "method");
        assert!(validate_answer(r#"{"blocks":[{"kind":"meeting_fact","text":"假共识","sources":["method:duozhou-v1"]}]}"#,&seen).is_err());
        assert!(validate_answer(r#"{"blocks":[{"kind":"suggestion","text":"核对验收条件","sources":["method:duozhou-v1"]}]}"#,&seen).is_ok());
    }
    #[test]
    fn accepted_drafts_are_idempotent_and_never_start_capture() {
        let (_tmp, rt) = setup();
        let c = open(&rt, None, true).unwrap();
        let cid = c["id"].as_str().unwrap();
        let turn = id();
        rt.db().unwrap().execute("INSERT INTO assistant_turns(id,conversation_id,epoch,question,source,state,created_at) VALUES (?,?,0,'准备周会','text','succeeded',?)",params![turn,cid,now()]).unwrap();
        let mut seen = HashMap::new();
        let result=tool(&rt,&turn,"准备周会",&json!({"context":{"kind":"general"},"evidence":[]}),&json!({"name":"propose_action","args":{"action":"prepare_meeting","title":"合成周会","text":"讨论卡点和下一步","meeting_type":"weekly"}}),&mut seen).unwrap();
        let pid = result["proposal_id"].as_str().unwrap();
        let first = accept(&rt, pid).unwrap();
        assert_eq!(accept(&rt, pid).unwrap(), first);
        assert_eq!(rt.detail(pid).unwrap()["state"], "ready");
        assert!(rt.capture.lock().unwrap().is_none());
    }
    #[test]
    fn real_pi_broker_unifies_voice_and_text_history_and_deduplicates() {
        let (_tmp, rt) = setup();
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
