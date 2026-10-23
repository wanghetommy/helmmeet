//! Product operations on persisted meetings. Never capture from a presentation event.
use crate::{
    analysis,
    db::{err, now, Runtime},
    models,
};
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};

pub fn globally_paused(rt: &Runtime) -> Result<bool, String> {
    rt.db()?
        .query_row(
            "SELECT value='true' FROM app_preferences WHERE key='analysis_paused'",
            [],
            |r| r.get(0),
        )
        .map_err(err)
}
pub fn initialize_analysis(rt: &Runtime, meeting: &str) -> Result<(), String> {
    let Ok(c) = models::selected(rt, meeting) else {
        return Ok(());
    };
    let Ok((_, local)) = c.endpoint() else {
        return Ok(());
    };
    if c.credential_ref.is_some() && models::secret(rt, &c).is_err() {
        return Ok(());
    }
    let mut db = rt.db()?;
    let tx = db.transaction().map_err(err)?;
    tx.execute("UPDATE meetings SET analysis_mode=?,consent_target=?,context_consent=1,analysis_choice='default',consent_version=1 WHERE id=? AND state='ready'",params![if local {"local"}else{"remote"},analysis::target(&c),meeting]).map_err(err)?;
    tx.execute("INSERT INTO meeting_configs VALUES (?,1,'{}',?,?)",params![meeting,serde_json::to_string(&c).map_err(err)?,json!({"mode":if local {"local"}else{"remote"},"target":analysis::target(&c),"scope":"confirmed_text_context_actions_selected_knowledge","automatic_minutes":0,"default_ready":true}).to_string()]).map_err(err)?;
    tx.commit().map_err(err)
}
pub fn capabilities(rt: &Runtime, d: &Value) -> Result<Value, String> {
    let state = d["state"].as_str().unwrap_or("ready");
    let text = d["segments"].as_array().map_or(0, |a| {
        a.iter()
            .filter(|s| !s["text"].as_str().unwrap_or("").trim().is_empty())
            .count()
    });
    let has_audio:bool=rt.db()?.query_row("SELECT EXISTS(SELECT 1 FROM audio_chunks WHERE meeting_id=? AND status='sealed' AND sample_count>0)",[d["id"].as_str().unwrap_or("")],|r|r.get(0)).map_err(err)?;
    let model = d["analysis_mode"] != "transcript_only"
        && !d["model_profile"].is_null()
        && d["model_profile"]["credential_valid"] == true
        && d["model_profile"]["archived"] != true;
    let paused = globally_paused(rt)?;
    Ok(
        json!({"phase":match meeting_core::product::phase(state){meeting_core::product::Phase::Preparation=>"preparation",meeting_core::product::Phase::Live=>"live",meeting_core::product::Phase::After=>"after"},"has_audio":has_audio,"has_confirmed_text":text>0,"has_minutes":d["artifacts"].as_array().is_some_and(|a|a.iter().any(|a|a["type"]=="stage" || a["type"]=="minutes")),"has_actions":d["actions"].as_array().is_some_and(|a|!a.is_empty()),"model_ready":model,"analysis_paused":paused,"can_analyze":meeting_core::product::can_analyze(state,text,model,paused)}),
    )
}
pub fn preferences(rt: &Runtime) -> Result<Value, String> {
    let db = rt.db()?;
    let mut q=db.prepare("SELECT key,value FROM app_preferences WHERE key IN ('analysis_paused','default_source_mask','default_input_device','debug_mode')").map_err(err)?;
    let mut result = json!({});
    for row in q
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .map_err(err)?
    {
        let (k, v) = row.map_err(err)?;
        result[k] = serde_json::from_str(&v).unwrap_or(json!(v));
    }
    Ok(result)
}
pub fn set_preferences(
    rt: &Runtime,
    paused: bool,
    source: i32,
    device: u32,
    debug_mode: bool,
) -> Result<Value, String> {
    if !(1..=3).contains(&source) {
        return Err("默认音源无效".into());
    }
    let mut db = rt.db()?;
    let tx = db.transaction().map_err(err)?;
    let before: bool = tx
        .query_row(
            "SELECT value='true' FROM app_preferences WHERE key='analysis_paused'",
            [],
            |r| r.get(0),
        )
        .map_err(err)?;
    for (k, v) in [
        ("analysis_paused", paused.to_string()),
        ("default_source_mask", source.to_string()),
        ("default_input_device", device.to_string()),
        ("debug_mode", debug_mode.to_string()),
    ] {
        tx.execute(
            "UPDATE app_preferences SET value=? WHERE key=?",
            params![v, k],
        )
        .map_err(err)?;
    }
    if before != paused {
        tx.execute(
            "UPDATE meetings SET generation=generation+1 WHERE state!='deleting'",
            [],
        )
        .map_err(err)?;
        if paused {
            tx.execute(
                "UPDATE analysis_jobs SET state='cancelled' WHERE state IN ('pending','running')",
                [],
            )
            .map_err(err)?;
            tx.execute(
                "UPDATE summary_schedules SET paused_reason='全局暂停分析' WHERE enabled=1",
                [],
            )
            .map_err(err)?;
        } else {
            tx.execute("UPDATE summary_schedules SET paused_reason=NULL WHERE paused_reason='全局暂停分析'",[]).map_err(err)?;
        }
    }
    tx.commit().map_err(err)?;
    Ok(json!({"saved":true}))
}
pub fn preparation(
    rt: &Runtime,
    mid: &str,
    expected: u64,
    title: &str,
    assistant: &str,
    source: i32,
    context: Value,
) -> Result<Value, String> {
    rt.check_id(mid)?;
    if title.trim().is_empty()
        || title.len() > 500
        || !(1..=3).contains(&source)
        || context.to_string().len() > 30000
    {
        return Err("请检查名称、音源或背景长度".into());
    }
    let mut db = rt.db()?;
    let tx = db.transaction().map_err(err)?;
    let version: u32 = tx
        .query_row(
            "SELECT version FROM assistant_templates WHERE id=?",
            [assistant],
            |r| r.get(0),
        )
        .map_err(|_| "会议类型不存在")?;
    if tx.execute("UPDATE meetings SET title=?,assistant_id=?,template_version=?,template_source='selected',source_mask=?,context_json=?,version=version+1,generation=generation+1 WHERE id=? AND version=? AND state='ready' AND wall_start IS NULL",params![title.trim(),assistant,version,source,context.to_string(),mid,expected]).map_err(err)?!=1 {return Err("准备信息已变化或已开始录制，请刷新后重试".into());}
    tx.commit().map_err(err)?;
    Ok(json!({"saved":true}))
}
pub fn follow_up(rt: &Runtime, mid: &str) -> Result<Value, String> {
    rt.check_id(mid)?;
    let (title,assistant,context):(String,String,String)=rt.db()?.query_row("SELECT title,assistant_id,context_json FROM meetings WHERE id=? AND state IN ('completed','needs_attention','finalizing')",[mid],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).map_err(|_|"请结束原会议后再发起续会")?;
    let title = format!("{} · 续会", title.chars().take(120).collect::<String>());
    let result = rt.create(&title, &assistant)?;
    let new = result["id"].as_str().ok_or("缺少会议ID")?;
    let mut db = rt.db()?;
    let tx = db.transaction().map_err(err)?;
    tx.execute(
        "UPDATE meetings SET parent_meeting_id=?,context_json=? WHERE id=?",
        params![mid, context, new],
    )
    .map_err(err)?;
    tx.execute("INSERT INTO meeting_knowledge_bindings SELECT ?,set_id,version FROM meeting_knowledge_bindings WHERE meeting_id=?",params![new,mid]).map_err(err)?;
    tx.commit().map_err(err)?;
    Ok(result)
}
pub fn confirm_artifact(
    rt: &Runtime,
    mid: &str,
    artifact: &str,
    version: u32,
) -> Result<Value, String> {
    rt.check_id(mid)?;
    let changed=rt.db()?.execute("UPDATE artifacts SET status='confirmed' WHERE meeting_id=? AND id=? AND version=? AND type='minutes' AND status IN ('draft','confirmed') AND coalesce(json_extract(coverage_json,'$.incomplete'),0)=0",params![mid,artifact,version]).map_err(err)?;
    if changed != 1 {
        return Err("纪要版本已变化或覆盖不完整，请先核对并生成完整版本".into());
    }
    Ok(json!({"confirmed":true,"at":now()}))
}
pub fn capture_epoch(rt: &Runtime, mid: &str) -> Result<String, String> {
    Ok(rt
        .db()?
        .query_row(
            "SELECT id FROM recording_epochs WHERE meeting_id=? ORDER BY rowid DESC LIMIT 1",
            [mid],
            |r| r.get::<_, String>(0),
        )
        .optional()
        .map_err(err)?
        .unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        audio,
        commands::{self, Request},
        models::{tests::MemoryVault, ProfileInput},
    };
    use std::sync::Arc;
    fn setup() -> (std::path::PathBuf, Arc<Runtime>) {
        let p = std::env::temp_dir().join(format!("helmmeet-product-{}", crate::db::id()));
        let rt = Runtime::open_with_vault(p.clone(), Arc::new(MemoryVault::default())).unwrap();
        (p, rt)
    }
    fn meeting(rt: &Runtime) -> String {
        rt.create("合成产品会议", "general").unwrap()["id"]
            .as_str()
            .unwrap()
            .into()
    }
    #[test]
    fn end_confirmation_requires_the_current_target_epoch_and_is_idempotent() {
        let (root, rt) = setup();
        let a = meeting(&rt);
        let b = meeting(&rt);
        rt.db()
            .unwrap()
            .execute(
                "UPDATE meetings SET state='paused',version=3 WHERE id=?",
                [&a],
            )
            .unwrap();
        rt.db().unwrap().execute("INSERT INTO recording_epochs VALUES ('epoch-a',?,0,1000,'synthetic','synthetic','pause')",[&a]).unwrap();
        assert!(audio::finish_confirmed(&rt, &b, 1, "epoch-a").is_err());
        assert!(audio::finish_confirmed(&rt, &a, 2, "epoch-a").is_err());
        assert_eq!(rt.detail(&a).unwrap()["state"], "paused");
        // A resumed capture has a new epoch even if a caller accidentally reuses a version.
        rt.db().unwrap().execute("INSERT INTO recording_epochs VALUES ('epoch-b',?,1000,2000,'synthetic','synthetic','pause')",[&a]).unwrap();
        assert!(audio::finish_confirmed(&rt, &a, 3, "epoch-a").is_err());
        audio::finish_confirmed(&rt, &a, 3, "epoch-b").unwrap();
        audio::finish_confirmed(&rt, &a, 3, "epoch-b").unwrap();
        assert_eq!(rt.detail(&a).unwrap()["version"], 4);
        assert_eq!(rt.detail(&a).unwrap()["state"], "finalizing");
        assert_eq!(rt.detail(&b).unwrap()["state"], "ready");
        assert!(commands::run(
            rt.clone(),
            Request::Record {
                meeting_id: b,
                action: "stop".into(),
                source_mask: 1,
                input_device: 0,
                confirm_microphone: false
            }
        )
        .is_err());
        drop(rt);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn new_meeting_model_is_ready_but_old_explicit_optout_and_global_pause_are_preserved() {
        let (root, rt) = setup();
        let legacy = meeting(&rt);
        models::save(
            &rt,
            ProfileInput {
                profile_id: None,
                expected_version: None,
                display_name: "合成本地模型".into(),
                base_url: "http://127.0.0.1:11434/v1".into(),
                model: "fixture".into(),
                context_tokens: 32768,
                max_output_tokens: 4096,
                timeout_seconds: 10,
                key_action: "clear".into(),
                api_key: String::new(),
            },
        )
        .unwrap();
        let mid = meeting(&rt);
        let d = rt.detail(&mid).unwrap();
        assert_eq!(d["analysis_mode"], "local");
        assert_eq!(d["context_consent"], true);
        assert_eq!(d["schedule"]["enabled"], false);
        assert_eq!(d["capabilities"]["can_analyze"], false);
        assert_eq!(
            rt.detail(&legacy).unwrap()["analysis_mode"],
            "transcript_only"
        );
        models::authorize(&rt, &mid, d["version"].as_u64().unwrap(), false, 0, None).unwrap();
        set_preferences(&rt, true, 3, 0, false).unwrap();
        set_preferences(&rt, false, 3, 0, false).unwrap();
        assert_eq!(preferences(&rt).unwrap()["debug_mode"], false);
        set_preferences(&rt, false, 3, 0, true).unwrap();
        assert_eq!(preferences(&rt).unwrap()["debug_mode"], true);
        set_preferences(&rt, false, 3, 0, false).unwrap();
        assert_eq!(rt.detail(&mid).unwrap()["analysis_mode"], "transcript_only");
        let next = meeting(&rt);
        {
            let db = rt.db().unwrap();
            db.execute("UPDATE meetings SET state='completed' WHERE id=?", [&next])
                .unwrap();
            db.execute("INSERT INTO transcript_segments VALUES ('empty-fixture',?,0,1000,'mixed',1,'final')",[&next]).unwrap();
            db.execute("INSERT INTO transcript_revisions VALUES ('empty-fixture',1,?,'   ','asr','synthetic')",[&next]).unwrap();
        }
        assert_eq!(
            rt.detail(&next).unwrap()["capabilities"]["has_confirmed_text"],
            false
        );
        assert!(analysis::submit(rt.clone(), &next, "stage", "", "all").is_err());
        set_preferences(&rt, true, 3, 0, false).unwrap();
        assert_eq!(rt.detail(&next).unwrap()["analysis_mode"], "local");
        assert_eq!(
            rt.detail(&next).unwrap()["capabilities"]["analysis_paused"],
            true
        );
        assert!(analysis::submit(rt.clone(), &next, "stage", "", "all").is_err());
        drop(rt);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn deleting_a_prepared_meeting_removes_only_that_draft_and_cannot_start_it() {
        let (root, rt) = setup();
        let draft = meeting(&rt);
        let other = meeting(&rt);
        preparation(
            &rt,
            &draft,
            1,
            "待删除准备",
            "weekly",
            1,
            json!({"goal":"合成目标"}),
        )
        .unwrap();
        crate::commands::run(
            rt.clone(),
            crate::commands::Request::Delete {
                meeting_id: draft.clone(),
            },
        )
        .unwrap();
        assert!(rt.detail(&draft).is_err());
        assert_eq!(rt.detail(&other).unwrap()["state"], "ready");
        assert!(crate::audio::start(rt.clone(), &draft, 1, 0, false).is_err());
        assert!(rt.capture.lock().unwrap().is_none());
        drop(rt);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn preparation_cas_and_followup_keep_results_out_of_unstarted_meetings() {
        let (root, rt) = setup();
        let mid = meeting(&rt);
        assert!(follow_up(&rt, &mid).is_err());
        preparation(
            &rt,
            &mid,
            1,
            "准备方案",
            "review",
            3,
            json!({"goal":"核对目标"}),
        )
        .unwrap();
        assert!(preparation(&rt, &mid, 1, "过期修改", "general", 1, json!({})).is_err());
        let d = rt.detail(&mid).unwrap();
        assert_eq!(d["capabilities"]["phase"], "preparation");
        assert_eq!(d["capabilities"]["has_audio"], false);
        assert_eq!(d["capabilities"]["has_confirmed_text"], false);
        rt.db()
            .unwrap()
            .execute("UPDATE meetings SET state='completed' WHERE id=?", [&mid])
            .unwrap();
        assert!(preparation(&rt, &mid, 2, "不可改回准备", "general", 1, json!({})).is_err());
        let next = follow_up(&rt, &mid).unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        let d = rt.detail(&next).unwrap();
        assert_eq!(d["parent_meeting_id"], mid);
        assert_eq!(d["context"]["goal"], "核对目标");
        assert!(d["segments"].as_array().unwrap().is_empty());
        drop(rt);
        std::fs::remove_dir_all(root).unwrap();
    }
}
