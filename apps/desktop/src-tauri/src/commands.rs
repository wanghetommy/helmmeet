use crate::{
    analysis, asr, audio,
    db::{err, id, now, Runtime},
    models,
};
use rusqlite::params;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    io::{Read, Write},
    sync::Arc,
};

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Request {
    List,
    Create {
        title: String,
        assistant_id: String,
    },
    Detail {
        meeting_id: String,
    },
    Record {
        meeting_id: String,
        action: String,
        source_mask: i32,
        #[serde(default)]
        input_device: u32,
        #[serde(default)]
        confirm_microphone: bool,
    },
    Devices,
    PreviewAudio {
        source_mask: i32,
        input_device: u32,
    },
    Evidence {
        meeting_id: String,
        segment_id: String,
        revision: u32,
    },
    Settings,
    SaveAsrSettings {
        language: String,
    },
    ModelProfiles,
    RetryModelImport,
    SaveModelProfile {
        profile: models::ProfileInput,
    },
    SetDefaultModelProfile {
        profile_id: String,
    },
    ArchiveModelProfile {
        profile_id: String,
        expected_version: u32,
    },
    TestModelProfile {
        profile_id: String,
        config_version: u32,
    },
    SwitchMeetingModel {
        meeting_id: String,
        expected_version: u64,
        profile_id: String,
        config_version: u32,
        enabled: bool,
        interval_minutes: u32,
    },
    SetMicrophoneEnabled {
        meeting_id: String,
        expected_version: u64,
        enabled: bool,
    },
    SelectModel,
    TestModel,
    Templates,
    SaveTemplate {
        id: Option<String>,
        name: String,
        instructions: String,
    },
    Analyze {
        meeting_id: String,
        analysis_type: String,
        question: String,
        #[serde(default)]
        scope: String,
    },
    Consent {
        meeting_id: String,
        expected_version: u64,
        enabled: bool,
        interval_minutes: u32,
    },
    Revise {
        meeting_id: String,
        segment_id: String,
        expected_revision: u32,
        text: String,
    },
    UpdateAction {
        meeting_id: String,
        action_id: String,
        expected_version: u32,
        task: String,
        owner_text: String,
        due_text: String,
        confirmation_state: String,
        execution_state: String,
        #[serde(default)]
        proposal_id: Option<String>,
    },
    Note {
        meeting_id: String,
        time_ms: u64,
        text: String,
    },
    MeetingContext {
        meeting_id: String,
        assistant_id: String,
        goal: String,
        agenda: String,
        participants: String,
        terms: String,
    },
    Rename {
        meeting_id: String,
        title: String,
    },
    Cancel {
        meeting_id: String,
    },
    RetryAsr {
        meeting_id: String,
    },
    Delete {
        meeting_id: String,
    },
    Export {
        meeting_id: String,
        format: String,
        #[serde(default)]
        artifact_id: Option<String>,
    },
    Playback {
        meeting_id: String,
        start_ms: u64,
    },
    EditArtifact {
        meeting_id: String,
        artifact_id: String,
        expected_version: u32,
        text: String,
    },
}
fn valid_text(text: &str, max: usize) -> Result<(), String> {
    if text.trim().is_empty() || text.len() > max {
        Err("文本为空或超过长度限制".into())
    } else {
        Ok(())
    }
}
pub fn run(rt: Arc<Runtime>, request: Request) -> Result<Value, String> {
    match request {
        Request::List => rt.list(),
        Request::Create {
            title,
            assistant_id,
        } => rt.create(&title, &assistant_id),
        Request::Detail { meeting_id } => rt.detail(&meeting_id),
        Request::Record {
            meeting_id,
            action,
            source_mask,
            input_device,
            confirm_microphone,
        } => {
            if action == "start" || action == "resume" {
                audio::start(
                    rt.clone(),
                    &meeting_id,
                    source_mask,
                    input_device,
                    confirm_microphone,
                )?;
                rt.db()?
                    .execute(
                        "UPDATE meetings SET source_mask=? WHERE id=?",
                        params![source_mask, meeting_id],
                    )
                    .map_err(err)?;
            } else {
                audio::control(&rt, &meeting_id, &action)?;
            }
            rt.detail(&meeting_id)
        }
        Request::Devices => audio::devices(),
        Request::PreviewAudio {
            source_mask,
            input_device,
        } => audio::preview(&rt, source_mask, input_device),
        Request::Evidence {
            meeting_id,
            segment_id,
            revision,
        } => {
            rt.check_id(&meeting_id)?;
            rt.db()?.query_row("SELECT r.text,s.start_ms,s.end_ms FROM transcript_revisions r JOIN transcript_segments s ON s.id=r.segment_id WHERE r.meeting_id=? AND r.segment_id=? AND r.revision=?",params![meeting_id,segment_id,revision],|r|Ok(json!({"text":r.get::<_,String>(0)?,"start_ms":r.get::<_,u64>(1)?,"end_ms":r.get::<_,u64>(2)?,"revision":revision}))).map_err(err)
        }
        Request::Settings => {
            let cfg = rt.config.lock().map_err(|_| "配置锁异常")?;
            Ok(
                json!({"model_file":std::path::Path::new(&cfg.asr.model_path).file_name().and_then(|s|s.to_str()),"asr_configured":std::path::Path::new(&cfg.asr.model_path).is_file(),"language":cfg.asr.language,"startup_warning":rt.transient.lock().map_err(|_|"状态锁异常")?["startup_warning"]}),
            )
        }
        Request::SaveAsrSettings { language } => {
            if !["zh", "en", "auto"].contains(&language.as_str()) {
                return Err("语言选项无效".into());
            }
            let mut cfg = rt.config.lock().map_err(|_| "配置锁异常")?;
            cfg.asr.language = language;
            cfg.save(&rt.root)?;
            Ok(json!({"saved":true}))
        }
        Request::ModelProfiles => models::list(&rt),
        Request::RetryModelImport => {
            models::bootstrap(&rt)?;
            rt.transient.lock().map_err(|_| "状态锁异常")?["startup_warning"] = Value::Null;
            Ok(json!({"imported":true}))
        }
        Request::SaveModelProfile { profile } => models::save(&rt, profile),
        Request::SetDefaultModelProfile { profile_id } => models::set_default(&rt, &profile_id),
        Request::ArchiveModelProfile {
            profile_id,
            expected_version,
        } => models::archive(&rt, &profile_id, expected_version),
        Request::TestModelProfile {
            profile_id,
            config_version,
        } => models::test(&rt, &profile_id, config_version),
        Request::SwitchMeetingModel {
            meeting_id,
            expected_version,
            profile_id,
            config_version,
            enabled,
            interval_minutes,
        } => models::authorize(
            &rt,
            &meeting_id,
            expected_version,
            enabled,
            interval_minutes,
            Some((&profile_id, config_version)),
        ),
        Request::SetMicrophoneEnabled {
            meeting_id,
            expected_version,
            enabled,
        } => {
            audio::set_microphone_enabled(&rt, &meeting_id, expected_version, enabled)?;
            rt.detail(&meeting_id)
        }

        Request::SelectModel => {
            let Some(path) = rfd::FileDialog::new()
                .add_filter("Whisper GGML 模型", &["bin"])
                .pick_file()
            else {
                return Ok(Value::Null);
            };
            let mut file = std::fs::File::open(&path).map_err(err)?;
            let mut header = [0; 4];
            file.read_exact(&mut header).map_err(err)?;
            if header != [0x6c, 0x6d, 0x67, 0x67] {
                return Err("模型文件头不是受支持的 GGML Whisper 格式".into());
            }
            let mut hash = Sha256::new();
            hash.update(header);
            let mut block = [0u8; 1024 * 1024];
            loop {
                let n = file.read(&mut block).map_err(err)?;
                if n == 0 {
                    break;
                }
                hash.update(&block[..n]);
            }
            let mut cfg = rt.config.lock().map_err(|_| "配置锁异常")?;
            cfg.asr.model_path = path.to_string_lossy().into();
            cfg.asr.model_sha256 = Some(format!("{:x}", hash.finalize()));
            cfg.save(&rt.root)?;
            Ok(json!({"selected":true}))
        }
        Request::TestModel => {
            let directory = models::list(&rt)?;
            let p = directory["default_profile_id"]
                .as_str()
                .ok_or("没有默认文本模型")?;
            let v = directory["profiles"]
                .as_array()
                .unwrap()
                .iter()
                .find(|v| v["profile_id"] == p)
                .and_then(|v| v["config_version"].as_u64())
                .ok_or("默认配置不可用")?;
            models::test(&rt, p, v as u32)
        }
        Request::Templates => {
            let db = rt.db()?;
            let mut s = db
                .prepare(
                    "SELECT id,version,name,instructions FROM assistant_templates ORDER BY name",
                )
                .map_err(err)?;
            let v=s.query_map([],|r|Ok(json!({"id":r.get::<_,String>(0)?,"version":r.get::<_,u32>(1)?,"name":r.get::<_,String>(2)?,"instructions":r.get::<_,String>(3)?}))).map_err(err)?.collect::<Result<Vec<_>,_>>().map_err(err)?;
            Ok(json!(v))
        }
        Request::SaveTemplate {
            id: template,
            name,
            instructions,
        } => {
            valid_text(&name, 200)?;
            valid_text(&instructions, 12000)?;
            let template = template.unwrap_or_else(id);
            let mut db = rt.db()?;
            let tx = db.transaction().map_err(err)?;
            tx.execute("INSERT INTO assistant_templates VALUES (?,1,?,?) ON CONFLICT(id) DO UPDATE SET name=excluded.name,instructions=excluded.instructions,version=version+1",params![template,name,instructions]).map_err(err)?;
            tx.execute(
                "UPDATE meetings SET generation=generation+1 WHERE assistant_id=?",
                [&template],
            )
            .map_err(err)?;
            tx.execute("UPDATE artifacts SET status='stale' WHERE meeting_id IN (SELECT id FROM meetings WHERE assistant_id=?)",[&template]).map_err(err)?;
            tx.commit().map_err(err)?;
            Ok(json!({"id":template}))
        }
        Request::Analyze {
            meeting_id,
            analysis_type,
            question,
            scope,
        } => analysis::submit(rt, &meeting_id, &analysis_type, &question, &scope),
        Request::Consent {
            meeting_id,
            expected_version,
            enabled,
            interval_minutes,
        } => models::authorize(
            &rt,
            &meeting_id,
            expected_version,
            enabled,
            interval_minutes,
            None,
        ),
        Request::Revise {
            meeting_id,
            segment_id,
            expected_revision,
            text,
        } => {
            rt.check_id(&meeting_id)?;
            valid_text(&text, 30000)?;
            let mut db = rt.db()?;
            let tx = db.transaction().map_err(err)?;
            let changed=tx.execute("UPDATE transcript_segments SET current_revision=current_revision+1,state='edited' WHERE id=? AND meeting_id=? AND current_revision=?",params![segment_id,meeting_id,expected_revision]).map_err(err)?;
            if changed != 1 {
                return Err("原文版本已变化，请刷新重试".into());
            }
            tx.execute(
                "INSERT INTO transcript_revisions VALUES (?,?,?,?,'human',?)",
                params![segment_id, expected_revision + 1, meeting_id, text, now()],
            )
            .map_err(err)?;
            tx.execute(
                "UPDATE artifacts SET status='stale' WHERE meeting_id=?",
                [&meeting_id],
            )
            .map_err(err)?;
            tx.execute(
                "UPDATE meetings SET generation=generation+1 WHERE id=?",
                [&meeting_id],
            )
            .map_err(err)?;
            tx.commit().map_err(err)?;
            Ok(json!({"revision":expected_revision+1}))
        }
        Request::UpdateAction {
            meeting_id,
            action_id,
            expected_version,
            task,
            owner_text,
            due_text,
            confirmation_state,
            execution_state,
            proposal_id,
        } => {
            rt.check_id(&meeting_id)?;
            valid_text(&task, 6000)?;
            if !["pending", "confirmed", "ignored"].contains(&confirmation_state.as_str())
                || !["todo", "doing", "done"].contains(&execution_state.as_str())
            {
                return Err("待办状态无效".into());
            }
            let changes = json!({"task":task,"owner_text":owner_text,"due_text":due_text,"confirmation_state":confirmation_state,"execution_state":execution_state});
            let mut db = rt.db()?;
            let tx = db.transaction().map_err(err)?;
            let n=tx.execute("UPDATE action_items SET task=?,owner_text=?,due_text=?,confirmation_state=?,execution_state=?,locked_fields_json='[\"task\",\"owner_text\",\"due_text\"]',version=version+1 WHERE id=? AND meeting_id=? AND version=?",params![task,owner_text,due_text,confirmation_state,execution_state,action_id,meeting_id,expected_version]).map_err(err)?;
            if n != 1 {
                return Err("待办版本冲突，请刷新".into());
            }
            tx.execute(
                "INSERT INTO action_revisions VALUES (?,?,'human',?,'[]',?)",
                params![action_id, expected_version + 1, changes.to_string(), now()],
            )
            .map_err(err)?;
            if let Some(proposal) = proposal_id {
                tx.execute(
                    "UPDATE action_proposals SET state='accepted' WHERE id=? AND action_id=?",
                    params![proposal, action_id],
                )
                .map_err(err)?;
            }
            tx.commit().map_err(err)?;
            Ok(json!({"saved":true}))
        }
        Request::Note {
            meeting_id,
            time_ms,
            text,
        } => {
            rt.check_id(&meeting_id)?;
            valid_text(&text, 30000)?;
            rt.db()?
                .execute(
                    "INSERT INTO user_notes VALUES (?,?,?,?)",
                    params![id(), meeting_id, time_ms, text],
                )
                .map_err(err)?;
            Ok(json!({"saved":true}))
        }
        Request::MeetingContext {
            meeting_id,
            assistant_id,
            goal,
            agenda,
            participants,
            terms,
        } => {
            rt.check_id(&meeting_id)?;
            if goal.len() + agenda.len() + participants.len() + terms.len() > 30000 {
                return Err("会议信息过长".into());
            }
            let mut db = rt.db()?;
            let tx = db.transaction().map_err(err)?;
            let count: u32 = tx
                .query_row(
                    "SELECT count(*) FROM assistant_templates WHERE id=?",
                    [&assistant_id],
                    |r| r.get(0),
                )
                .map_err(err)?;
            if count != 1 {
                return Err("助手不存在".into());
            }
            tx.execute("UPDATE meetings SET assistant_id=?,context_json=?,generation=generation+1 WHERE id=?",params![assistant_id,json!({"goal":goal,"agenda":agenda,"participants":participants,"terms":terms}).to_string(),meeting_id]).map_err(err)?;
            tx.commit().map_err(err)?;
            Ok(json!({"saved":true}))
        }
        Request::Rename { meeting_id, title } => {
            rt.check_id(&meeting_id)?;
            valid_text(&title, 500)?;
            rt.db()?
                .execute(
                    "UPDATE meetings SET title=?,version=version+1 WHERE id=?",
                    params![title, meeting_id],
                )
                .map_err(err)?;
            Ok(json!({"saved":true}))
        }
        Request::Cancel { meeting_id } => {
            rt.check_id(&meeting_id)?;
            let mut db = rt.db()?;
            let tx = db.transaction().map_err(err)?;
            tx.execute(
                "UPDATE meetings SET generation=generation+1 WHERE id=?",
                [&meeting_id],
            )
            .map_err(err)?;
            tx.execute("UPDATE analysis_jobs SET state='cancelled' WHERE meeting_id=? AND state IN ('pending','running')",[&meeting_id]).map_err(err)?;
            tx.commit().map_err(err)?;
            Ok(json!({"cancelled":true}))
        }
        Request::RetryAsr { meeting_id } => {
            rt.check_id(&meeting_id)?;
            rt.db()?.execute("UPDATE meetings SET last_error=NULL,state=CASE WHEN state IN ('completed','needs_attention') THEN 'finalizing' ELSE state END WHERE id=?",[&meeting_id]).map_err(err)?;
            Ok(json!({"queued":true}))
        }
        Request::Delete { meeting_id } => {
            rt.check_id(&meeting_id)?;
            let capture = rt.capture.lock().map_err(|_| "采集锁异常")?;
            if capture.as_ref().is_some_and(|s| s.meeting == meeting_id) {
                return Err("请先结束录制再删除".into());
            }
            drop(capture);
            let mut db = rt.db()?;
            let tx = db.transaction().map_err(err)?;
            tx.execute(
                "UPDATE meetings SET state='deleting',generation=generation+1 WHERE id=?",
                [&meeting_id],
            )
            .map_err(err)?;
            tx.execute(
                "INSERT INTO cleanup_queue VALUES (?,?,?,0,NULL)",
                params![id(), meeting_id, format!("meetings/{meeting_id}")],
            )
            .map_err(err)?;
            tx.commit().map_err(err)?;
            drop(db);
            cleanup(&rt)?;
            Ok(json!({"deleted":true}))
        }
        Request::Playback {
            meeting_id,
            start_ms,
        } => {
            rt.check_id(&meeting_id)?;
            let duration: u64 = rt
                .db()?
                .query_row(
                    "SELECT recording_duration_ms FROM meetings WHERE id=?",
                    [&meeting_id],
                    |r| r.get(0),
                )
                .map_err(err)?;
            let end = (start_ms + 30000).min(duration);
            if end <= start_ms {
                return Err("录音位置超出范围".into());
            }
            let pcm = asr::mix(&rt, &meeting_id, start_ms, end)?;
            Ok(
                json!({"samples":pcm.iter().map(|s|(s.clamp(-1.0,1.0)*32767.0) as i16).collect::<Vec<_>>(),"start_ms":start_ms,"end_ms":end}),
            )
        }
        Request::Export {
            meeting_id,
            format,
            artifact_id,
        } => {
            let mut detail = rt.detail(&meeting_id)?;
            if let Some(artifact) = artifact_id {
                let selected = detail["artifacts"]
                    .as_array()
                    .and_then(|items| items.iter().find(|a| a["id"].as_str() == Some(&artifact)))
                    .cloned()
                    .ok_or("所选纪要版本不存在")?;
                detail["artifacts"] = json!([selected]);
                let db = rt.db()?;
                let mut q=db.prepare("SELECT DISTINCT r.segment_id,r.revision,r.text,s.start_ms FROM artifact_evidence e JOIN transcript_revisions r ON r.segment_id=e.segment_id AND r.revision=e.revision JOIN transcript_segments s ON s.id=r.segment_id WHERE e.artifact_id=? AND e.meeting_id=? ORDER BY s.start_ms").map_err(err)?;
                let rows=q.query_map(params![artifact,meeting_id],|r|Ok(json!({"id":r.get::<_,String>(0)?,"revision":r.get::<_,u32>(1)?,"text":r.get::<_,String>(2)?,"start_ms":r.get::<_,u64>(3)?}))).map_err(err)?.collect::<Result<Vec<_>,_>>().map_err(err)?;
                detail["export_evidence"] = json!(rows);
            }
            if !["md", "txt", "csv", "wav", "wav_microphone", "wav_system"]
                .contains(&format.as_str())
            {
                return Err("导出格式无效".into());
            }
            let Some(path) = rfd::FileDialog::new()
                .set_file_name(format!(
                    "meeting.{}",
                    if format.starts_with("wav") {
                        "wav"
                    } else {
                        &format
                    }
                ))
                .save_file()
            else {
                return Ok(json!({"cancelled":true}));
            };
            if format.starts_with("wav") {
                let temp = path.with_extension(format!("{}.pending", id()));
                let mut file = audio::WavFile::create(&temp)?;
                let duration = detail["duration_ms"].as_u64().unwrap_or(0);
                let mut start = 0;
                while start < duration {
                    let end = (start + 8000).min(duration);
                    file.write(&asr::mix_source(
                        &rt,
                        &meeting_id,
                        start,
                        end,
                        match format.as_str() {
                            "wav_microphone" => Some("microphone"),
                            "wav_system" => Some("system"),
                            _ => None,
                        },
                    )?)?;
                    start = end;
                }
                file.sync()?;
                drop(file);
                std::fs::rename(temp, path).map_err(err)?;
            } else {
                let output = export_text(&detail, &format);
                let mut file = std::fs::File::create(path).map_err(err)?;
                file.write_all(output.as_bytes()).map_err(err)?;
                file.sync_all().map_err(err)?;
            }
            Ok(json!({"exported":true}))
        }
        Request::EditArtifact {
            meeting_id,
            artifact_id,
            expected_version,
            text,
        } => {
            rt.check_id(&meeting_id)?;
            valid_text(&text, 200000)?;
            let mut db = rt.db()?;
            let tx = db.transaction().map_err(err)?;
            let(kind,coverage):(String,String)=tx.query_row("SELECT type,coverage_json FROM artifacts WHERE id=? AND meeting_id=? AND version=?",params![artifact_id,meeting_id,expected_version],|r|Ok((r.get(0)?,r.get(1)?))).map_err(|_|"纪要版本不存在")?;
            let version:u32=tx.query_row("SELECT coalesce(max(version),0)+1 FROM artifacts WHERE meeting_id=? AND type=?",params![meeting_id,kind],|r|r.get(0)).map_err(err)?;
            tx.execute(
                "INSERT INTO artifacts VALUES (?,?,?,?,?,?, 'confirmed','human')",
                params![
                    id(),
                    meeting_id,
                    kind,
                    version,
                    json!({"manual_text":text,"source_artifact_id":artifact_id}).to_string(),
                    coverage
                ],
            )
            .map_err(err)?;
            tx.commit().map_err(err)?;
            Ok(json!({"saved":true}))
        }
    }
}
pub fn cleanup(rt: &Runtime) -> Result<(), String> {
    let meetings = {
        let db = rt.db()?;
        let mut q = db
            .prepare("SELECT id FROM meetings WHERE state='deleting'")
            .map_err(err)?;
        let rows = q
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(err)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(err)?;
        rows
    };
    for meeting in meetings {
        uuid::Uuid::parse_str(&meeting).map_err(err)?;
        let dir = rt.meeting_dir(&meeting);
        if !dir.exists() || std::fs::remove_dir_all(&dir).is_ok() {
            rt.db()?
                .execute("DELETE FROM meetings WHERE id=?", [&meeting])
                .map_err(err)?;
        } else {
            rt.db()?.execute("UPDATE cleanup_queue SET attempts=attempts+1,error_code='FILE_DELETE_FAILED' WHERE meeting_id=?",[&meeting]).map_err(err)?;
        }
    }
    Ok(())
}
fn csv(value: &str) -> String {
    let normalized = value.trim_start_matches(|c: char| c.is_whitespace() || c.is_control());
    let safe = if normalized.starts_with(['=', '+', '-', '@']) {
        format!("'{value}")
    } else {
        value.to_string()
    };
    format!("\"{}\"", safe.replace('"', "\"\""))
}
pub fn export_text(detail: &Value, format: &str) -> String {
    if format == "csv" {
        let mut out = "\u{feff}事项,负责人,截止时间,确认状态,执行状态\r\n".to_string();
        for a in detail["actions"].as_array().unwrap_or(&vec![]) {
            out += &[
                "task",
                "owner_text",
                "due_text",
                "confirmation_state",
                "execution_state",
            ]
            .map(|k| csv(a[k].as_str().unwrap_or("")))
            .join(",");
            out += "\r\n";
        }
        return out;
    }
    let mut out = format!("# {}\n\n", detail["title"].as_str().unwrap_or("会议"));
    if format == "md" {
        for artifact in detail["artifacts"].as_array().unwrap_or(&vec![]) {
            out += &format!(
                "## {} · v{} · {}\n\n",
                artifact["type"].as_str().unwrap_or("纪要"),
                artifact["version"],
                artifact["status"].as_str().unwrap_or("")
            );
            if let Some(profile) = artifact["coverage"]["profile_name"].as_str() {
                out += &format!(
                    "原生成配置：{} · {} · 配置 v{}\n\n",
                    profile,
                    artifact["coverage"]["model"].as_str().unwrap_or("未知模型"),
                    artifact["coverage"]["config_version"]
                );
            }
            out += &artifact["content"]
                .get("manual_text")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .unwrap_or_else(|| {
                    serde_json::to_string_pretty(&artifact["content"]).unwrap_or_default()
                });
            out += "\n\n";
        }
        out += "## 待办\n\n";
        for a in detail["actions"].as_array().unwrap_or(&vec![]) {
            out += &format!(
                "- {} / {} / {} / {}\n",
                a["task"].as_str().unwrap_or(""),
                a["owner_text"].as_str().unwrap_or("待确认"),
                a["due_text"].as_str().unwrap_or("待确认"),
                a["confirmation_state"].as_str().unwrap_or("pending")
            );
        }
    }
    if let Some(rows) = detail["export_evidence"].as_array() {
        out += "\n## 生成时引用原文\n\n";
        for s in rows {
            let ms = s["start_ms"].as_u64().unwrap_or(0);
            out += &format!(
                "[{:02}:{:02}] {} r{}: {}\n\n",
                ms / 60000,
                (ms / 1000) % 60,
                s["id"].as_str().unwrap_or(""),
                s["revision"],
                s["text"].as_str().unwrap_or("")
            );
        }
    }
    out += "\n## 完整转写\n\n";
    for s in detail["segments"].as_array().unwrap_or(&vec![]) {
        let ms = s["start_ms"].as_u64().unwrap_or(0);
        out += &format!(
            "[{:02}:{:02}] {} ({} r{})\n\n",
            ms / 60000,
            (ms / 1000) % 60,
            s["text"].as_str().unwrap_or(""),
            s["id"].as_str().unwrap_or(""),
            s["revision"]
        );
    }
    out
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn csv_formula_and_quotes() {
        assert!(csv("\t=1+1").starts_with("\"'"));
        assert_eq!(csv("a\"b"), "\"a\"\"b\"");
    }
}
