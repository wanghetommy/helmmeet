use crate::{audio::CaptureSession, config::Config};
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, MutexGuard,
    },
    time::Duration,
};

pub fn id() -> String {
    uuid::Uuid::new_v4().to_string()
}
pub fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}
pub fn err(e: impl std::fmt::Display) -> String {
    format!("本地操作失败：{e}")
}
pub struct Runtime {
    pub root: PathBuf,
    pub conn: Mutex<Connection>,
    pub config: Mutex<Config>,
    pub capture: Mutex<Option<CaptureSession>>,
    pub voice: crate::voice::Hub,
    pub activity: crate::activity::Hub,
    pub asr_preparation: crate::asr::Preparation,
    pub llm_gate: Mutex<()>,
    pub analysis_waiting: Mutex<Vec<(String, u8)>>,
    pub analysis_threads: Mutex<Vec<std::thread::JoinHandle<()>>>,
    pub model_gate: Mutex<()>,
    pub import_gate: Mutex<()>,
    pub vault: Arc<dyn crate::models::CredentialStore>,
    pub transient: Mutex<Value>,
    pub shutting_down: AtomicBool,
}
impl Runtime {
    pub fn open(root: PathBuf) -> Result<Arc<Self>, String> {
        Self::open_inner(
            root.clone(),
            Arc::new(crate::credentials::FileStore::new(root)),
            true,
        )
    }
    pub fn open_with_vault(
        root: PathBuf,
        vault: Arc<dyn crate::models::CredentialStore>,
    ) -> Result<Arc<Self>, String> {
        Self::open_inner(root, vault, false)
    }
    fn open_inner(
        root: PathBuf,
        vault: Arc<dyn crate::models::CredentialStore>,
        migrate: bool,
    ) -> Result<Arc<Self>, String> {
        std::fs::create_dir_all(root.join("meetings")).map_err(err)?;
        let conn = Connection::open(root.join("app.sqlite")).map_err(err)?;
        conn.busy_timeout(Duration::from_secs(5)).map_err(err)?;
        conn.execute_batch(
            "PRAGMA foreign_keys=ON; PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;",
        )
        .map_err(err)?;
        let version: u32 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .map_err(err)?;
        if version > 24 {
            return Err("数据库版本较新，请使用匹配的应用版本".into());
        }
        if version > 0 && version < 24 {
            let mut backup = Connection::open(root.join("before-migration.sqlite")).map_err(err)?;
            rusqlite::backup::Backup::new(&conn, &mut backup)
                .map_err(err)?
                .run_to_completion(64, Duration::from_millis(10), None)
                .map_err(err)?;
        }
        if version == 0 {
            conn.execute_batch(include_str!("../../../../migrations/0001_init.sql"))
                .map_err(err)?;
        }
        if version < 2 {
            conn.execute_batch(include_str!("../../../../migrations/0002_runtime.sql"))
                .map_err(err)?;
        }
        if version < 3 {
            conn.execute_batch(include_str!(
                "../../../../migrations/0003_action_proposals.sql"
            ))
            .map_err(err)?;
        }
        if version < 4 {
            conn.execute_batch(include_str!(
                "../../../../migrations/0004_meeting_context.sql"
            ))
            .map_err(err)?;
        }
        if version < 5 {
            conn.execute_batch(include_str!(
                "../../../../migrations/0005_model_profiles.sql"
            ))
            .map_err(err)?;
        }
        if version < 6 {
            conn.execute_batch(include_str!(
                "../../../../migrations/0006_recording_sources.sql"
            ))
            .map_err(err)?;
        }
        if version < 7 {
            conn.execute_batch(include_str!("../../../../migrations/0007_workspace.sql"))
                .map_err(err)?;
        }
        if version < 8 {
            conn.execute_batch(include_str!(
                "../../../../migrations/0008_audio_enhancement.sql"
            ))
            .map_err(err)?;
        }
        if version < 9 {
            conn.execute_batch(include_str!(
                "../../../../migrations/0009_echo_cancellation.sql"
            ))
            .map_err(err)?;
        }
        if version < 10 {
            conn.execute_batch(include_str!(
                "../../../../migrations/0010_product_workspace.sql"
            ))
            .map_err(err)?;
        }
        if version < 11 {
            conn.execute_batch(include_str!(
                "../../../../migrations/0011_duozhou_assistant.sql"
            ))
            .map_err(err)?;
        }
        if version < 12 {
            conn.execute_batch(include_str!(
                "../../../../migrations/0012_summary_memory.sql"
            ))
            .map_err(err)?;
        }
        if version < 13 {
            conn.execute_batch(include_str!(
                "../../../../migrations/0013_paragraph_activity.sql"
            ))
            .map_err(err)?;
        }
        if version < 14 {
            conn.execute_batch(include_str!(
                "../../../../migrations/0014_assistant_review.sql"
            ))
            .map_err(err)?;
        }
        if version < 15 {
            conn.execute_batch(include_str!(
                "../../../../migrations/0015_history_memory.sql"
            ))
            .map_err(err)?;
        }
        if version < 16 {
            conn.execute_batch(include_str!(
                "../../../../migrations/0016_transcript_calibration.sql"
            ))
            .map_err(err)?;
        }
        if version < 17 {
            conn.execute_batch(include_str!(
                "../../../../migrations/0017_calibration_runs.sql"
            ))
            .map_err(err)?;
        }
        if version < 18 {
            conn.execute_batch(include_str!("../../../../migrations/0018_debug_mode.sql"))
                .map_err(err)?;
        }
        conn.execute("UPDATE assistant_turns SET state='failed',error='应用退出中断了本次问答，可重新提问' WHERE state IN ('pending','running')", []).map_err(err)?;
        for template in [
            include_str!("../../../../templates/general.json"),
            include_str!("../../../../templates/project.json"),
        ] {
            let v: Value = serde_json::from_str(template).map_err(err)?;
            conn.execute(
                "INSERT OR IGNORE INTO assistant_templates VALUES (?,1,?,?)",
                params![
                    v["id"].as_str(),
                    v["name"].as_str(),
                    v["instructions"].as_str()
                ],
            )
            .map_err(err)?;
        }
        if version < 19 {
            conn.execute_batch(include_str!("../../../../migrations/0019_tts_settings.sql"))
                .map_err(err)?;
        }
        if version < 20 {
            conn.execute_batch(include_str!(
                "../../../../migrations/0020_tts_agent_plan.sql"
            ))
            .map_err(err)?;
        }
        if version < 21 {
            conn.execute_batch(include_str!(
                "../../../../migrations/0021_assistant_behavior.sql"
            ))
            .map_err(err)?;
        }
        if version < 22 {
            conn.execute_batch(include_str!(
                "../../../../migrations/0022_persona_dimensions.sql"
            ))
            .map_err(err)?;
        }
        if version < 23 {
            conn.execute_batch(include_str!(
                "../../../../migrations/0023_persona_consolidation.sql"
            ))
            .map_err(err)?;
        }
        if version < 24 {
            conn.execute_batch(include_str!(
                "../../../../migrations/0024_composed_assistant.sql"
            ))
            .map_err(err)?;
        }
        crate::workspace::seed_templates(&conn)?;
        conn.execute("INSERT OR IGNORE INTO template_versions SELECT id,version,name,instructions FROM assistant_templates", []).map_err(err)?;
        crate::templates::seed_missing(&conn)?;
        conn.execute("UPDATE summary_schedules SET tail_done=1", [])
            .map_err(err)?;
        conn.execute("UPDATE meetings SET state='interrupted',last_error='应用退出导致采集中断，请恢复或结束会议' WHERE state IN ('recording','paused')", []).map_err(err)?;
        conn.execute("UPDATE recording_sources SET state='failed',reason='应用退出导致采集中断' WHERE state='active'",[]).map_err(err)?;
        conn.execute("UPDATE analysis_jobs SET state='failed',error_code='INTERRUPTED' WHERE state IN ('pending','running')", []).map_err(err)?;
        conn.execute("UPDATE transcript_calibrations SET status='failed',error='应用退出中断了本次校准',updated_at=? WHERE status='pending'", [now()]).map_err(err)?;
        let config = Config::load(&root);
        let runtime = Arc::new(Self {
            root,
            conn: Mutex::new(conn),
            config: Mutex::new(config),
            capture: Mutex::new(None),
            voice: crate::voice::Hub::default(),
            activity: crate::activity::Hub::default(),
            asr_preparation: crate::asr::Preparation::default(),
            llm_gate: Mutex::new(()),
            analysis_waiting: Mutex::new(vec![]),
            analysis_threads: Mutex::new(vec![]),
            model_gate: Mutex::new(()),
            import_gate: Mutex::new(()),
            vault,
            transient: Mutex::new(json!({})),
            shutting_down: AtomicBool::new(false),
        });
        if migrate {
            if let Err(e) = crate::credentials::migrate(&runtime, false) {
                runtime.transient.lock().map_err(|_| "状态锁异常")?["startup_warning"] = json!(e);
            }
        }
        if let Err(e) = crate::models::bootstrap(&runtime) {
            runtime.transient.lock().map_err(|_| "状态锁异常")?["startup_warning"] = json!(
                format!("旧模型配置迁移未完成：{e}。录音转写仍可使用，可在设置中重试。")
            );
        }
        Ok(runtime)
    }
    pub fn db(&self) -> Result<MutexGuard<'_, Connection>, String> {
        self.conn.lock().map_err(|_| "数据库锁异常".into())
    }
    pub fn ensure_running(&self) -> Result<(), String> {
        if self.shutting_down.load(Ordering::Acquire) {
            return Err("应用正在退出，已停止接收新任务".into());
        }
        Ok(())
    }
    pub fn check_id(&self, meeting: &str) -> Result<(), String> {
        uuid::Uuid::parse_str(meeting).map_err(|_| "会议 ID 无效")?;
        let state: String = self
            .db()?
            .query_row("SELECT state FROM meetings WHERE id=?", [meeting], |r| {
                r.get(0)
            })
            .map_err(|_| "会议不存在")?;
        if state == "deleting" {
            return Err("会议正在删除".into());
        }
        Ok(())
    }
    pub fn meeting_dir(&self, meeting: &str) -> PathBuf {
        self.root.join("meetings").join(meeting)
    }
    pub fn list(&self) -> Result<Value, String> {
        let db = self.db()?;
        let mut s=db.prepare("SELECT m.id,m.title,m.state,m.recording_duration_ms,m.created_at,m.last_error,m.assistant_id,v.name,m.parent_meeting_id FROM meetings m LEFT JOIN template_versions v ON v.template_id=m.assistant_id AND v.version=m.template_version WHERE m.state!='deleting' ORDER BY m.created_at DESC").map_err(err)?;
        let rows=s.query_map([],|r|Ok(json!({"id":r.get::<_,String>(0)?,"title":r.get::<_,String>(1)?,"state":r.get::<_,String>(2)?,"duration_ms":r.get::<_,u64>(3)?,"created_at":r.get::<_,String>(4)?,"last_error":r.get::<_,Option<String>>(5)?,"assistant_id":r.get::<_,String>(6)?,"type_name":r.get::<_,Option<String>>(7)?,"parent_meeting_id":r.get::<_,Option<String>>(8)?}))).map_err(err)?;
        Ok(Value::Array(
            rows.collect::<Result<Vec<_>, _>>().map_err(err)?,
        ))
    }
    pub fn create(&self, title: &str, assistant: &str) -> Result<Value, String> {
        let meeting = id();
        self.create_with_id(&meeting, title, assistant)
    }
    pub(crate) fn create_with_id(
        &self,
        meeting: &str,
        title: &str,
        assistant: &str,
    ) -> Result<Value, String> {
        uuid::Uuid::parse_str(meeting).map_err(|_| "会议ID无效")?;
        if title.trim().is_empty() || title.len() > 500 {
            return Err("请输入不超过 500 字节的会议名称".into());
        }
        let db = self.db()?;
        if db
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM meetings WHERE id=?)",
                [meeting],
                |r| r.get::<_, bool>(0),
            )
            .map_err(err)?
        {
            return Ok(json!({"id":meeting}));
        }
        let valid: u32 = db
            .query_row(
                "SELECT count(*) FROM assistant_templates WHERE id=?",
                [assistant],
                |r| r.get(0),
            )
            .map_err(err)?;
        if valid == 0 {
            return Err("助手不存在".into());
        }
        let mut db = db;
        let tx = db.transaction().map_err(err)?;
        tx.execute("INSERT INTO meetings(id,title,timezone,created_at,assistant_id) VALUES (?,?,'Asia/Shanghai',?,?)",params![meeting,title.trim(),now(),assistant]).map_err(err)?;
        tx.execute("INSERT INTO meeting_model_bindings SELECT ?,p.id,p.current_version FROM model_profiles p JOIN app_preferences a ON a.key='default_profile' AND a.value=p.id WHERE p.archived=0", [&meeting]).map_err(err)?;
        tx.execute("UPDATE meetings SET template_source='selected',template_version=(SELECT version FROM assistant_templates WHERE id=assistant_id),auto_summary_minutes=0 WHERE id=?", [&meeting]).map_err(err)?;
        tx.execute(
            "INSERT INTO summary_schedules(meeting_id) VALUES (?)",
            [&meeting],
        )
        .map_err(err)?;
        tx.execute("INSERT INTO audio_enhancement(meeting_id,mode) SELECT ?,value FROM app_preferences WHERE key='audio_enhancement_default'", [&meeting]).map_err(err)?;
        tx.commit().map_err(err)?;
        drop(db);
        crate::product::initialize_analysis(self, meeting)?;
        Ok(json!({"id":meeting}))
    }
    pub fn detail(&self, meeting: &str) -> Result<Value, String> {
        self.check_id(meeting)?;
        let db = self.db()?;
        let mut data=db.query_row("SELECT id,title,state,recording_duration_ms,analysis_mode,assistant_id,auto_summary_minutes,last_error,generation,version,source_mask,context_json,created_at FROM meetings WHERE id=?",[meeting],|r|Ok(json!({"id":r.get::<_,String>(0)?,"title":r.get::<_,String>(1)?,"state":r.get::<_,String>(2)?,"duration_ms":r.get::<_,u64>(3)?,"analysis_mode":r.get::<_,String>(4)?,"assistant_id":r.get::<_,String>(5)?,"auto_summary_minutes":r.get::<_,u32>(6)?,"last_error":r.get::<_,Option<String>>(7)?,"generation":r.get::<_,u64>(8)?,"version":r.get::<_,u64>(9)?,"source_mask":r.get::<_,i32>(10)?,"context":serde_json::from_str::<Value>(&r.get::<_,String>(11)?).unwrap_or(json!({})),"created_at":r.get::<_,String>(12)?}))).map_err(err)?;
        macro_rules! query {
            ($sql:expr,$mapper:expr) => {{
                let mut s = db.prepare($sql).map_err(err)?;
                let rows = s.query_map([meeting], $mapper).map_err(err)?;
                Value::Array(rows.collect::<Result<Vec<Value>, _>>().map_err(err)?)
            }};
        }
        data["segments"]=query!("SELECT s.id,s.start_ms,s.end_ms,s.current_revision,r.text,s.state FROM transcript_segments s JOIN transcript_revisions r ON r.segment_id=s.id AND r.revision=s.current_revision WHERE s.meeting_id=? ORDER BY s.start_ms,s.id",|r|Ok(json!({"id":r.get::<_,String>(0)?,"start_ms":r.get::<_,u64>(1)?,"end_ms":r.get::<_,u64>(2)?,"revision":r.get::<_,u32>(3)?,"text":r.get::<_,String>(4)?,"state":r.get::<_,String>(5)?})));
        data["artifacts"]=query!("SELECT id,type,version,content_json,status,coverage_json FROM artifacts WHERE meeting_id=? ORDER BY rowid DESC",|r|Ok(json!({"id":r.get::<_,String>(0)?,"type":r.get::<_,String>(1)?,"version":r.get::<_,u32>(2)?,"content":serde_json::from_str::<Value>(&r.get::<_,String>(3)?).unwrap_or(Value::Null),"status":r.get::<_,String>(4)?,"coverage":serde_json::from_str::<Value>(&r.get::<_,String>(5)?).unwrap_or(Value::Null)})));
        data["actions"]=query!("SELECT id,version,task,owner_text,due_text,confirmation_state,execution_state FROM action_items WHERE meeting_id=? ORDER BY rowid",|r|Ok(json!({"id":r.get::<_,String>(0)?,"version":r.get::<_,u32>(1)?,"task":r.get::<_,String>(2)?,"owner_text":r.get::<_,Option<String>>(3)?,"due_text":r.get::<_,Option<String>>(4)?,"confirmation_state":r.get::<_,String>(5)?,"execution_state":r.get::<_,String>(6)?})));
        data["notes"]=query!("SELECT id,recording_time_ms,text FROM user_notes WHERE meeting_id=? ORDER BY recording_time_ms",|r|Ok(json!({"id":r.get::<_,String>(0)?,"time_ms":r.get::<_,u64>(1)?,"text":r.get::<_,String>(2)?})));
        data["jobs"]=query!("SELECT id,type,state,error_code FROM analysis_jobs WHERE meeting_id=? ORDER BY rowid DESC LIMIT 20",|r|Ok(json!({"id":r.get::<_,String>(0)?,"type":r.get::<_,String>(1)?,"state":r.get::<_,String>(2)?,"error":r.get::<_,Option<String>>(3)?})));
        data["coverage"]=query!("SELECT kind,start_ms,end_ms,state,reason FROM coverage_intervals WHERE meeting_id=? ORDER BY start_ms",|r|Ok(json!({"kind":r.get::<_,String>(0)?,"start_ms":r.get::<_,u64>(1)?,"end_ms":r.get::<_,u64>(2)?,"state":r.get::<_,String>(3)?,"reason":r.get::<_,Option<String>>(4)?})));
        data["proposals"]=query!("SELECT p.id,p.action_id,p.proposal_json FROM action_proposals p JOIN action_items a ON a.id=p.action_id WHERE a.meeting_id=? AND p.state='pending'",|r|Ok(json!({"id":r.get::<_,String>(0)?,"action_id":r.get::<_,String>(1)?,"changes":serde_json::from_str::<Value>(&r.get::<_,String>(2)?).unwrap_or(Value::Null)})));
        data["consent_version"] = json!(db
            .query_row(
                "SELECT consent_version FROM meetings WHERE id=?",
                [meeting],
                |r| r.get::<_, u64>(0)
            )
            .map_err(err)?);
        data["asr_processed_ms"] = json!(db
            .query_row(
                "SELECT coalesce(sum(end_ms-start_ms),0) FROM asr_chunks WHERE meeting_id=?",
                [meeting],
                |r| r.get::<_, u64>(0)
            )
            .map_err(err)?);
        data["sources"]=query!("SELECT source,state,device,gap_start_ms,reason FROM recording_sources WHERE meeting_id=? ORDER BY source",|r|Ok(json!({"source":r.get::<_,String>(0)?,"state":r.get::<_,String>(1)?,"device":r.get::<_,u32>(2)?,"gap_start_ms":r.get::<_,Option<u64>>(3)?,"reason":r.get::<_,Option<String>>(4)?})));
        data["legacy_answer_ids"]=query!("SELECT a.id FROM artifacts a WHERE a.meeting_id=? AND a.type='answer' AND NOT EXISTS(SELECT 1 FROM conversation_turns t WHERE t.artifact_id=a.id)",|r|Ok(json!(r.get::<_,String>(0)?)));
        data["conversation"]=query!("SELECT t.job_id,t.question,t.scope,t.artifact_id,j.state,j.error_code FROM conversation_turns t JOIN meetings m ON m.id=t.meeting_id JOIN analysis_jobs j ON j.id=t.job_id WHERE t.meeting_id=? AND t.epoch=m.chat_epoch ORDER BY t.rowid",|r|Ok(json!({"job_id":r.get::<_,String>(0)?,"question":r.get::<_,String>(1)?,"scope":r.get::<_,String>(2)?,"artifact_id":r.get::<_,Option<String>>(3)?,"state":r.get::<_,String>(4)?,"error":r.get::<_,Option<String>>(5)?})));
        let projected = crate::paragraphs::project(
            &db,
            meeting,
            data["segments"]
                .as_array()
                .map(Vec::as_slice)
                .unwrap_or(&[]),
            matches!(data["state"].as_str(), Some("recording" | "finalizing")),
        )?;
        data["paragraphs"] = json!(crate::paragraphs::readable(crate::calibration::decorate(
            &db, meeting, projected
        )?));
        data["calibration"] = crate::calibration::detail(&db, meeting)?;
        data["summary_pending_segments"]=json!(db.query_row("SELECT count(*) FROM transcript_segments s LEFT JOIN summary_coverage c ON c.meeting_id=s.meeting_id AND c.segment_id=s.id WHERE s.meeting_id=? AND (c.revision IS NULL OR c.revision!=s.current_revision)",[meeting],|r|r.get::<_,u64>(0)).map_err(err)?);
        data["summary_pending_bytes"]=json!(db.query_row("SELECT coalesce(sum(length(CAST(r.text AS BLOB))),0) FROM transcript_segments s JOIN transcript_revisions r ON r.segment_id=s.id AND r.revision=s.current_revision LEFT JOIN summary_coverage c ON c.meeting_id=s.meeting_id AND c.segment_id=s.id WHERE s.meeting_id=? AND s.state IN('final','edited') AND (c.revision IS NULL OR c.revision!=s.current_revision)",[meeting],|r|r.get::<_,u64>(0)).map_err(err)?);
        data["schedule"] = db.query_row("SELECT enabled,mode,version,failures,paused_reason,migration_notice FROM summary_schedules WHERE meeting_id=?", [meeting], |r| Ok(json!({"enabled":r.get::<_,bool>(0)?,"mode":r.get::<_,String>(1)?,"version":r.get::<_,u64>(2)?,"failures":r.get::<_,u32>(3)?,"paused_reason":r.get::<_,Option<String>>(4)?,"migration_notice":r.get::<_,Option<String>>(5)?}))).map_err(err)?;
        data["context_consent"] = json!(db
            .query_row(
                "SELECT context_consent FROM meetings WHERE id=?",
                [meeting],
                |r| r.get::<_, bool>(0)
            )
            .map_err(err)?);
        data["template_source"] = json!(db
            .query_row(
                "SELECT template_source FROM meetings WHERE id=?",
                [meeting],
                |r| r.get::<_, String>(0)
            )
            .map_err(err)?);
        data["template"] = crate::templates::bound(&db, meeting)?;
        drop(db);
        data["enhancement"] = crate::enhancement::detail(self, meeting)?;
        data["model_profile"] = crate::models::meeting_dto(self, meeting)?;
        data["transient"] = self
            .transient
            .lock()
            .map_err(|_| "状态锁异常")?
            .get(meeting)
            .cloned()
            .unwrap_or(json!({}));
        data["knowledge"] = crate::knowledge::bound(self, meeting)?;
        data["capabilities"] = crate::product::capabilities(self, &data)?;
        data["capture_epoch"] = self
            .db()?
            .query_row(
                "SELECT id FROM recording_epochs WHERE meeting_id=? ORDER BY rowid DESC LIMIT 1",
                [meeting],
                |r| r.get::<_, String>(0),
            )
            .map(Value::String)
            .unwrap_or(Value::Null);
        data["parent_meeting_id"] = self
            .db()?
            .query_row(
                "SELECT parent_meeting_id FROM meetings WHERE id=?",
                [meeting],
                |r| r.get::<_, Option<String>>(0),
            )
            .map_err(err)?
            .map(Value::String)
            .unwrap_or(Value::Null);
        Ok(data)
    }
    pub fn error(&self, meeting: &str, message: &str) {
        if let Ok(db) = self.db() {
            let _ = db.execute(
                "UPDATE meetings SET last_error=? WHERE id=?",
                params![message, meeting],
            );
        }
    }
}
pub fn safe_relative(root: &Path, relative: &str) -> Result<PathBuf, String> {
    let p = Path::new(relative);
    if p.is_absolute()
        || p.components()
            .any(|c| !matches!(c, std::path::Component::Normal(_)))
    {
        return Err("文件路径越界".into());
    }
    Ok(root.join(p))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{self, Request};
    #[test]
    fn runtime_revision_cas_and_action_protection() {
        let root = std::env::temp_dir().join(format!("helmmeet-{}", id()));
        let rt = Runtime::open(root.clone()).unwrap();
        let m = rt.create("测试会议", "general").unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        let segment = id();
        {
            let db = rt.db().unwrap();
            db.execute(
                "INSERT INTO transcript_segments VALUES (?,?,0,1000,'mixed',1,'final')",
                params![segment, m],
            )
            .unwrap();
            db.execute(
                "INSERT INTO transcript_revisions VALUES (?,1,?,'原始文本','asr',?)",
                params![segment, m, now()],
            )
            .unwrap();
        }
        commands::run(
            rt.clone(),
            Request::Revise {
                meeting_id: m.clone(),
                segment_id: segment.clone(),
                expected_revision: 1,
                text: "人工修订".into(),
            },
        )
        .unwrap();
        assert!(commands::run(
            rt.clone(),
            Request::Revise {
                meeting_id: m.clone(),
                segment_id: segment.clone(),
                expected_revision: 1,
                text: "旧版本写入".into()
            }
        )
        .is_err());
        assert_eq!(rt.detail(&m).unwrap()["segments"][0]["text"], "人工修订");
        assert_eq!(
            rt.db()
                .unwrap()
                .query_row(
                    "SELECT count(*) FROM transcript_revisions WHERE segment_id=?",
                    [&segment],
                    |r| r.get::<_, u32>(0)
                )
                .unwrap(),
            2
        );
        assert!(safe_relative(&root, "../escape").is_err());
        commands::run(
            rt.clone(),
            Request::Delete {
                meeting_id: m.clone(),
            },
        )
        .unwrap();
        assert!(rt.detail(&m).is_err());
        drop(rt);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn restart_marks_recording_interrupted() {
        let root = std::env::temp_dir().join(format!("helmmeet-{}", id()));
        let rt = Runtime::open(root.clone()).unwrap();
        let m = rt.create("恢复测试", "general").unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        rt.db()
            .unwrap()
            .execute("UPDATE meetings SET state='recording' WHERE id=?", [&m])
            .unwrap();
        drop(rt);
        let rt = Runtime::open(root.clone()).unwrap();
        assert_eq!(rt.detail(&m).unwrap()["state"], "interrupted");
        drop(rt);
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(test)]
mod recovery_tests {
    use super::*;
    #[test]
    fn upgrades_existing_v1_and_backs_up() {
        let root = std::env::temp_dir().join(format!("helmmeet-upgrade-{}", id()));
        std::fs::create_dir_all(&root).unwrap();
        let old = Connection::open(root.join("app.sqlite")).unwrap();
        old.execute_batch(include_str!("../../../../migrations/0001_init.sql"))
            .unwrap();
        drop(old);
        let rt = Runtime::open(root.clone()).unwrap();
        assert_eq!(
            rt.db()
                .unwrap()
                .query_row("PRAGMA user_version", [], |r| r.get::<_, u32>(0))
                .unwrap(),
            24
        );
        assert!(root.join("before-migration.sqlite").is_file());
        drop(rt);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn recovers_unsealed_pcm_idempotently() {
        let root = std::env::temp_dir().join(format!("helmmeet-audio-recovery-{}", id()));
        let rt = Runtime::open(root.clone()).unwrap();
        let mid = rt.create("恢复检查", "general").unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        let audio = rt.meeting_dir(&mid).join("audio");
        std::fs::create_dir_all(&audio).unwrap();
        let mut wav = crate::audio::WavFile::create(&audio.join("tail.wav")).unwrap();
        wav.write(&vec![0.2; 16000]).unwrap();
        drop(wav);
        rt.db().unwrap().execute("INSERT INTO audio_chunks(id,meeting_id,source,relative_path,start_ms,status) VALUES (?,?,'microphone',?,0,'writing')",params![id(),mid,format!("meetings/{mid}/audio/tail.wav")]).unwrap();
        crate::asr::recover(&rt).unwrap();
        crate::asr::recover(&rt).unwrap();
        assert_eq!(rt.detail(&mid).unwrap()["duration_ms"], 1000);
        assert_eq!(crate::asr::mix(&rt, &mid, 0, 1000).unwrap().len(), 16000);
        drop(rt);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn human_action_update_uses_cas_and_keeps_history() {
        use crate::commands::{run, Request};
        let root = std::env::temp_dir().join(format!("helmmeet-actions-{}", id()));
        let rt = Runtime::open(root.clone()).unwrap();
        let mid = rt.create("待办检查", "general").unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        let aid = id();
        {
            let db = rt.db().unwrap();
            db.execute("INSERT INTO action_items(id,meeting_id,task,confirmation_state) VALUES (?,?,'旧事项','pending')",params![aid,mid]).unwrap();
            db.execute(
                "INSERT INTO action_revisions VALUES (?,1,'model','{}','[]',NULL)",
                [&aid],
            )
            .unwrap();
        }
        let change = || Request::UpdateAction {
            meeting_id: mid.clone(),
            action_id: aid.clone(),
            expected_version: 1,
            task: "人工事项".into(),
            owner_text: "Alice".into(),
            due_text: "明天".into(),
            confirmation_state: "confirmed".into(),
            execution_state: "doing".into(),
            proposal_id: None,
        };
        run(rt.clone(), change()).unwrap();
        assert!(run(rt.clone(), change()).is_err());
        assert_eq!(rt.detail(&mid).unwrap()["actions"][0]["task"], "人工事项");
        assert_eq!(
            rt.db()
                .unwrap()
                .query_row(
                    "SELECT count(*) FROM action_revisions WHERE action_id=?",
                    [&aid],
                    |r| r.get::<_, u32>(0)
                )
                .unwrap(),
            2
        );
        drop(rt);
        std::fs::remove_dir_all(root).unwrap();
    }
}
