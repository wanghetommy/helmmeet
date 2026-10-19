//! Versioned model directory. Secrets never enter SQLite or response DTOs.
use crate::{
    analysis,
    config::ModelConfig,
    db::{err, id, now, Runtime},
};
use rusqlite::{params, OptionalExtension, Transaction};
use serde::Deserialize;
use serde_json::{json, Value};

pub trait CredentialStore: Send + Sync {
    fn get(&self, account: &str) -> Result<Option<String>, String>;
    fn put(&self, account: &str, secret: &str) -> Result<(), String>;
    fn remove(&self, account: &str) -> Result<(), String>;
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileInput {
    pub profile_id: Option<String>,
    pub expected_version: Option<u32>,
    pub display_name: String,
    pub base_url: String,
    pub model: String,
    pub context_tokens: u32,
    pub max_output_tokens: u32,
    pub timeout_seconds: u64,
    pub key_action: String,
    #[serde(default)]
    pub api_key: String,
}
fn config_at(
    db: &rusqlite::Connection,
    profile: &str,
    version: u32,
) -> Result<ModelConfig, String> {
    let raw: String = db
        .query_row(
            "SELECT config_json FROM model_configs WHERE id=? AND version=?",
            params![profile, version],
            |r| r.get(0),
        )
        .map_err(|_| "模型配置版本不存在")?;
    serde_json::from_str(&raw).map_err(|_| "模型配置损坏".into())
}
fn active(db: &rusqlite::Connection, config: &ModelConfig) -> Result<(), String> {
    let archived: bool = db
        .query_row(
            "SELECT archived FROM model_profiles WHERE id=?",
            [&config.profile_id],
            |r| r.get(0),
        )
        .map_err(|_| "模型配置不存在")?;
    if archived {
        return Err("该模型配置已停用，请为本场选择其他配置".into());
    }
    if let Some(reference) = &config.credential_ref {
        let revoked: bool = db
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM revoked_credentials WHERE credential_ref=?)",
                [reference],
                |r| r.get(0),
            )
            .map_err(err)?;
        if revoked {
            return Err("该版本凭据已失效，请显式采用新配置版本".into());
        }
    }
    Ok(())
}
pub fn load(rt: &Runtime, profile: &str, version: u32) -> Result<ModelConfig, String> {
    let db = rt.db()?;
    let c = config_at(&db, profile, version)?;
    active(&db, &c)?;
    Ok(c)
}
pub fn selected(rt: &Runtime, meeting: &str) -> Result<ModelConfig, String> {
    let db = rt.db()?;
    let (p, v): (String, u32) = db
        .query_row(
            "SELECT profile_id,config_version FROM meeting_model_bindings WHERE meeting_id=?",
            [meeting],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(|_| "请先为本场会议选择文本模型配置")?;
    let c = config_at(&db, &p, v)?;
    active(&db, &c)?;
    Ok(c)
}
pub fn secret(rt: &Runtime, config: &ModelConfig) -> Result<Option<String>, String> {
    active(&*rt.db()?, config)?;
    match &config.credential_ref {
        Some(reference) => rt
            .vault
            .get(reference)?
            .map(Some)
            .ok_or_else(|| "该配置的凭据不可用，请更新配置".into()),
        None => Ok(None),
    }
}
fn dto(rt: &Runtime, c: &ModelConfig, current: u32, archived: bool) -> Result<Value, String> {
    let revoked = if let Some(k) = &c.credential_ref {
        rt.db()?
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM revoked_credentials WHERE credential_ref=?)",
                [k],
                |r| r.get::<_, bool>(0),
            )
            .map_err(err)?
    } else {
        false
    };
    let mut value = serde_json::to_value(c).map_err(err)?;
    value.as_object_mut().unwrap().remove("credential_ref");
    let present = c
        .credential_ref
        .as_ref()
        .is_none_or(|k| rt.vault.get(k).ok().flatten().is_some());
    value["has_api_key"] = json!(c.credential_ref.is_some() && !revoked && present);
    value["credential_valid"] = json!(!revoked && present);
    value["is_local"] = json!(c.endpoint().map(|(_, local)| local).unwrap_or(false));
    value["bound_meetings"] = json!(rt
        .db()?
        .query_row(
            "SELECT count(*) FROM meeting_model_bindings WHERE profile_id=?",
            [&c.profile_id],
            |r| r.get::<_, u64>(0)
        )
        .map_err(err)?);
    value["archived"] = json!(archived);
    value["latest_version"] = json!(current);
    let test: Option<String> = rt
        .db()?
        .query_row(
            "SELECT value FROM app_preferences WHERE key=?",
            [format!("test:{}:{}", c.profile_id, c.config_version)],
            |r| r.get(0),
        )
        .optional()
        .map_err(err)?;
    value["last_test"] = test
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(Value::Null);
    Ok(value)
}
pub fn list(rt: &Runtime) -> Result<Value, String> {
    let entries = {
        let db = rt.db()?;
        let mut s = db
            .prepare("SELECT id,current_version,archived FROM model_profiles ORDER BY rowid")
            .map_err(err)?;
        let result = s
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, u32>(1)?,
                    r.get::<_, bool>(2)?,
                ))
            })
            .map_err(err)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(err)?;
        result
    };
    let mut profiles = vec![];
    for (p, v, a) in entries {
        let c = config_at(&*rt.db()?, &p, v)?;
        profiles.push(dto(rt, &c, v, a)?);
    }
    let default: Option<String> = rt
        .db()?
        .query_row(
            "SELECT value FROM app_preferences WHERE key='default_profile'",
            [],
            |r| r.get(0),
        )
        .optional()
        .map_err(err)?;
    Ok(json!({"profiles":profiles,"default_profile_id":default}))
}
pub fn meeting_dto(rt: &Runtime, meeting: &str) -> Result<Value, String> {
    let bound:Option<(String,u32,u32,bool)> = rt.db()?.query_row("SELECT b.profile_id,b.config_version,p.current_version,p.archived FROM meeting_model_bindings b JOIN model_profiles p ON p.id=b.profile_id WHERE b.meeting_id=?", [meeting], |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional().map_err(err)?;
    match bound {
        Some((p, v, latest, archived)) => {
            let c = config_at(&*rt.db()?, &p, v)?;
            dto(rt, &c, latest, archived)
        }
        None => Ok(Value::Null),
    }
}
fn invalidate(tx: &Transaction<'_>, profile: &str, credential: Option<&str>) -> Result<(), String> {
    // Filter by immutable credential reference for rotation; by profile for archive.
    tx.execute("UPDATE meetings SET analysis_mode='transcript_only',consent_target=NULL,consent_version=consent_version+1,generation=generation+1,version=version+1 WHERE id IN (SELECT b.meeting_id FROM meeting_model_bindings b JOIN model_configs c ON c.id=b.profile_id AND c.version=b.config_version WHERE b.profile_id=? AND (? IS NULL OR c.credential_ref=?))",params![profile,credential,credential]).map_err(err)?;
    tx.execute("UPDATE analysis_jobs SET state='cancelled' WHERE state IN ('pending','running') AND meeting_id IN (SELECT b.meeting_id FROM meeting_model_bindings b JOIN model_configs c ON c.id=b.profile_id AND c.version=b.config_version WHERE b.profile_id=? AND (? IS NULL OR c.credential_ref=?))",params![profile,credential,credential]).map_err(err)?;
    Ok(())
}
pub fn save(rt: &Runtime, input: ProfileInput) -> Result<Value, String> {
    save_internal(rt, input, false)
}
fn save_internal(rt: &Runtime, input: ProfileInput, importing: bool) -> Result<Value, String> {
    let _gate = rt.model_gate.lock().map_err(|_| "模型目录锁异常")?;
    if input.display_name.trim().is_empty()
        || input.display_name.len() > 200
        || input.base_url.len() > 2048
        || input.model.len() > 500
    {
        return Err("请填写有效配置名称、服务地址与模型名称".into());
    }
    if !(1024..=2_000_000).contains(&input.context_tokens)
        || !(128..=65536).contains(&input.max_output_tokens)
        || input.max_output_tokens >= input.context_tokens
        || !(10..=180).contains(&input.timeout_seconds)
    {
        return Err("上下文/输出上限或超时无效；输出必须小于上下文".into());
    }
    if !matches!(input.key_action.as_str(), "keep" | "replace" | "clear")
        || input.api_key.len() > 8192
        || (input.key_action == "replace" && input.api_key.trim().is_empty())
        || (input.key_action != "replace" && !input.api_key.is_empty())
    {
        return Err("请选择保留、更换或清除密钥，更换时需填写新密钥".into());
    }
    let profile = input.profile_id.clone().unwrap_or_else(id);
    let old = if input.profile_id.is_some() {
        let version: u32 = rt
            .db()?
            .query_row(
                "SELECT current_version FROM model_profiles WHERE id=? AND archived=0",
                [&profile],
                |r| r.get(0),
            )
            .map_err(|_| "配置不存在或已停用")?;
        if Some(version) != input.expected_version {
            return Err("配置版本冲突，请刷新重试".into());
        }
        Some(load(rt, &profile, version).or_else(|_| config_at(&*rt.db()?, &profile, version))?)
    } else {
        None
    };
    let base_url = input.base_url.trim().trim_end_matches('/').to_string();
    if input.key_action == "keep"
        && old
            .as_ref()
            .is_some_and(|o| o.base_url != base_url && o.credential_ref.is_some())
    {
        return Err("服务地址已变更，请重新指定或明确清除凭据".into());
    }
    if input.key_action == "keep"
        && old
            .as_ref()
            .is_some_and(|o| rt.db().and_then(|db| active(&db, o)).is_err())
    {
        return Err("旧凭据已失效，请更换或清除".into());
    }
    let reference = match input.key_action.as_str() {
        "replace" => Some(format!("profile:{profile}:{}", id())),
        "keep" => old.as_ref().and_then(|c| c.credential_ref.clone()),
        _ => None,
    };
    let c = ModelConfig {
        profile_id: profile.clone(),
        config_version: old.as_ref().map_or(1, |o| o.config_version + 1),
        display_name: input.display_name.trim().into(),
        base_url,
        model: input.model.trim().into(),
        credential_ref: reference.clone(),
        context_tokens: input.context_tokens,
        max_output_tokens: input.max_output_tokens,
        timeout_seconds: input.timeout_seconds,
        ..ModelConfig::default()
    };
    c.endpoint()?;
    if input.key_action == "replace" {
        let k = reference.as_ref().unwrap();
        rt.db()?
            .execute("INSERT INTO credential_pending VALUES (?)", [k])
            .map_err(err)?;
        rt.vault.put(k, &input.api_key)?;
        if rt.vault.get(k)?.as_deref() != Some(input.api_key.as_str()) {
            return Err("本地凭据写入校验失败，配置未提交".into());
        }
    }
    let result = (|| -> Result<(), String> {
        let mut db = rt.db()?;
        let tx = db.transaction().map_err(err)?;
        tx.execute(
            "INSERT INTO model_configs VALUES (?,?,?,?)",
            params![
                profile,
                c.config_version,
                serde_json::to_string(&c).map_err(err)?,
                reference
            ],
        )
        .map_err(err)?;
        tx.execute("INSERT INTO model_profiles VALUES (?,?,0) ON CONFLICT(id) DO UPDATE SET current_version=excluded.current_version",params![profile,c.config_version]).map_err(err)?;
        tx.execute(
            "INSERT OR IGNORE INTO app_preferences VALUES ('default_profile',?)",
            [&profile],
        )
        .map_err(err)?;
        if importing {
            tx.execute(
                "INSERT OR REPLACE INTO app_preferences VALUES ('legacy_import_profile',?)",
                [&profile],
            )
            .map_err(err)?;
        }
        if input.key_action != "keep" {
            if let Some(k) = old.as_ref().and_then(|o| o.credential_ref.as_ref()) {
                tx.execute("INSERT OR IGNORE INTO revoked_credentials VALUES (?)", [k])
                    .map_err(err)?;
                tx.execute("INSERT OR IGNORE INTO credential_cleanup VALUES (?)", [k])
                    .map_err(err)?;
                invalidate(&tx, &profile, Some(k))?;
            }
        }
        if let Some(k) = &reference {
            tx.execute("DELETE FROM credential_pending WHERE credential_ref=?", [k])
                .map_err(err)?;
        }
        tx.commit().map_err(err)
    })();
    if result.is_err() && input.key_action == "replace" {
        let _ = cleanup_credentials(rt);
    }
    result?;
    // Cleanup failures are durable and retried at startup, never undo committed config.
    let _ = cleanup_credentials(rt);
    dto(rt, &c, c.config_version, false)
}
pub fn set_default(rt: &Runtime, profile: &str) -> Result<Value, String> {
    let db = rt.db()?;
    let n: u32 = db
        .query_row(
            "SELECT count(*) FROM model_profiles WHERE id=? AND archived=0",
            [profile],
            |r| r.get(0),
        )
        .map_err(err)?;
    if n != 1 {
        return Err("默认配置不可用".into());
    }
    db.execute("INSERT INTO app_preferences VALUES ('default_profile',?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",[profile]).map_err(err)?;
    Ok(json!({"saved":true}))
}
pub fn archive(rt: &Runtime, profile: &str, expected: u32) -> Result<Value, String> {
    let _gate = rt.model_gate.lock().map_err(|_| "模型目录锁异常")?;
    let mut db = rt.db()?;
    let tx = db.transaction().map_err(err)?;
    if tx
        .execute(
            "UPDATE model_profiles SET archived=1 WHERE id=? AND current_version=? AND archived=0",
            params![profile, expected],
        )
        .map_err(err)?
        != 1
    {
        return Err("配置版本冲突或已经停用".into());
    }
    invalidate(&tx, profile, None)?;
    tx.execute(
        "DELETE FROM app_preferences WHERE key='default_profile' AND value=?",
        [profile],
    )
    .map_err(err)?;
    tx.execute("INSERT OR IGNORE INTO revoked_credentials SELECT credential_ref FROM model_configs WHERE id=? AND credential_ref IS NOT NULL",[profile]).map_err(err)?;
    tx.execute("INSERT OR IGNORE INTO credential_cleanup SELECT credential_ref FROM model_configs WHERE id=? AND credential_ref IS NOT NULL",[profile]).map_err(err)?;
    tx.commit().map_err(err)?;
    drop(db);
    let _ = cleanup_credentials(rt);
    Ok(json!({"archived":true}))
}
pub fn authorize(
    rt: &Runtime,
    meeting: &str,
    expected: u64,
    enabled: bool,
    interval: u32,
    selection: Option<(&str, u32)>,
) -> Result<Value, String> {
    rt.check_id(meeting)?;
    if ![0, 3, 5, 10].contains(&interval) {
        return Err("总结间隔无效".into());
    }
    let mut db = rt.db()?;
    let tx = db.transaction().map_err(err)?;
    let c = if let Some((p, v)) = selection {
        let c = config_at(&tx, p, v)?;
        active(&tx, &c)?;
        Some(c)
    } else {
        let bound: Option<(String, u32)> = tx
            .query_row(
                "SELECT profile_id,config_version FROM meeting_model_bindings WHERE meeting_id=?",
                [meeting],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(err)?;
        bound.map(|(p, v)| config_at(&tx, &p, v)).transpose()?
    };
    let (mode, target) = if enabled {
        let c = c.as_ref().ok_or("请先选择文本模型配置")?;
        active(&tx, c)?;
        let (_, local) = c.endpoint()?;
        (
            if local { "local" } else { "remote" },
            Some(analysis::target(c)),
        )
    } else {
        ("transcript_only", None)
    };
    if tx.execute("UPDATE meetings SET analysis_mode=?,consent_target=?,auto_summary_minutes=?,context_consent=?,analysis_choice=?,generation=generation+1,version=version+1,consent_version=consent_version+1 WHERE id=? AND version=? AND state!='deleting'",params![mode,target,interval,enabled,if enabled {"enabled"}else{"disabled"},meeting,expected]).map_err(err)?!=1{return Err("会议版本冲突，请刷新重试".into());}
    if let Some(c) = &c {
        if selection.is_some() {
            tx.execute("INSERT INTO meeting_model_bindings VALUES (?,?,?) ON CONFLICT(meeting_id) DO UPDATE SET profile_id=excluded.profile_id,config_version=excluded.config_version",params![meeting,c.profile_id,c.config_version]).map_err(err)?;
        }
        let version: u64 = tx
            .query_row(
                "SELECT coalesce(max(version),0)+1 FROM meeting_configs WHERE meeting_id=?",
                [meeting],
                |r| r.get(0),
            )
            .map_err(err)?;
        tx.execute("INSERT INTO meeting_configs VALUES (?,?,'{}',?,?)",params![meeting,version,serde_json::to_string(c).map_err(err)?,json!({"mode":mode,"target":target,"scope":"confirmed_text_context_actions","automatic_minutes":interval}).to_string()]).map_err(err)?;
    }
    tx.execute("UPDATE analysis_jobs SET state='cancelled' WHERE meeting_id=? AND state IN ('pending','running')",[meeting]).map_err(err)?;
    tx.commit().map_err(err)?;
    Ok(json!({"mode":mode}))
}
pub fn cleanup_credentials(rt: &Runtime) -> Result<(), String> {
    let entries = {
        let db = rt.db()?;
        let mut s=db.prepare("SELECT credential_ref FROM credential_pending UNION SELECT credential_ref FROM credential_cleanup").map_err(err)?;
        let rows = s
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(err)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(err)?;
        rows
    };
    for k in entries {
        let live:bool=rt.db()?.query_row("SELECT EXISTS(SELECT 1 FROM model_configs WHERE credential_ref=? AND NOT EXISTS(SELECT 1 FROM revoked_credentials WHERE credential_ref=?))",params![k,k],|r|r.get(0)).map_err(err)?;
        if live {
            continue;
        }
        rt.vault.remove(&k)?;
        let db = rt.db()?;
        db.execute(
            "DELETE FROM credential_pending WHERE credential_ref=?",
            [&k],
        )
        .map_err(err)?;
        db.execute(
            "DELETE FROM credential_cleanup WHERE credential_ref=?",
            [&k],
        )
        .map_err(err)?;
    }
    Ok(())
}
pub fn bootstrap(rt: &Runtime) -> Result<(), String> {
    let _import = rt.import_gate.lock().map_err(|_| "迁移锁异常")?;
    // Pending refs may belong to a concurrent save; cleanup shares its directory lock.
    {
        let _gate = rt.model_gate.lock().map_err(|_| "模型目录锁异常")?;
        let _ = cleanup_credentials(rt);
    }
    let done: bool = rt
        .db()?
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM app_preferences WHERE key='profile_import_done')",
            [],
            |r| r.get(0),
        )
        .map_err(err)?;
    if !done {
        let legacy = rt.config.lock().map_err(|_| "配置锁异常")?.analysis.clone();
        if !legacy.base_url.is_empty() && !legacy.model.is_empty() {
            let existing: Option<String> = rt
                .db()?
                .query_row(
                    "SELECT value FROM app_preferences WHERE key='legacy_import_profile'",
                    [],
                    |r| r.get(0),
                )
                .optional()
                .map_err(err)?;
            if existing.is_none() {
                let key = if !legacy.api_key.is_empty() {
                    Some(legacy.api_key.clone())
                } else {
                    rt.vault.get(&legacy.base_url)?
                };
                save_internal(
                    rt,
                    ProfileInput {
                        profile_id: None,
                        expected_version: None,
                        display_name: "已迁移的文本模型".into(),
                        base_url: legacy.base_url.clone(),
                        model: legacy.model,
                        context_tokens: legacy.context_tokens,
                        max_output_tokens: legacy.max_output_tokens,
                        timeout_seconds: legacy.timeout_seconds,
                        key_action: if key.is_some() { "replace" } else { "clear" }.into(),
                        api_key: key.unwrap_or_default(),
                    },
                    true,
                )?;
            }
            // The new credential was committed and verified by the store before old cleanup.
            rt.db()?
                .execute(
                    "INSERT OR IGNORE INTO credential_cleanup VALUES (?)",
                    [&legacy.base_url],
                )
                .map_err(err)?;
        }
        let mut db = rt.db()?;
        let tx = db.transaction().map_err(err)?;
        tx.execute("UPDATE meetings SET analysis_mode='transcript_only',consent_target=NULL,consent_version=consent_version+1,generation=generation+1,version=version+1",[]).map_err(err)?;
        tx.execute(
            "INSERT INTO app_preferences VALUES ('profile_import_done','1')",
            [],
        )
        .map_err(err)?;
        tx.commit().map_err(err)?;
    }
    let mut config = rt.config.lock().map_err(|_| "配置锁异常")?;
    if !config.analysis.base_url.is_empty() {
        config.analysis = ModelConfig::default();
        config.save(&rt.root)?;
    }
    drop(config);
    {
        let _gate = rt.model_gate.lock().map_err(|_| "模型目录锁异常")?;
        let _ = cleanup_credentials(rt);
    }
    Ok(())
}

pub fn test(rt: &Runtime, profile: &str, version: u32) -> Result<Value, String> {
    let config = load(rt, profile, version)?;
    let _permit = loop {
        rt.ensure_running()?;
        if let Ok(permit) = rt.llm_gate.try_lock() {
            break permit;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    };
    let check = || {
        rt.ensure_running()?;
        load(rt, profile, version).map(|_| ())
    };
    analysis::test_structure(rt, &config, &check)?;
    let result = json!({"ok":true,"streaming":false,"structured_output":"validated","tested_at":now(),"message":"连接、JSON结构及合成原文引用校验通过"});
    rt.db()?.execute("INSERT INTO app_preferences VALUES (?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",params![format!("test:{profile}:{version}"),result.to_string()]).map_err(err)?;
    Ok(result)
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use std::{
        collections::HashMap,
        io::{Read, Write},
        net::{TcpListener, TcpStream},
        sync::{
            atomic::{AtomicBool, Ordering},
            Arc, Mutex,
        },
        time::{Duration, Instant},
    };
    #[derive(Default)]
    pub struct MemoryVault {
        pub values: Mutex<HashMap<String, String>>,
        fail: AtomicBool,
    }
    impl CredentialStore for MemoryVault {
        fn get(&self, k: &str) -> Result<Option<String>, String> {
            Ok(self.values.lock().unwrap().get(k).cloned())
        }
        fn put(&self, k: &str, v: &str) -> Result<(), String> {
            if self.fail.load(Ordering::Relaxed) {
                return Err("injected vault failure".into());
            }
            self.values.lock().unwrap().insert(k.into(), v.into());
            Ok(())
        }
        fn remove(&self, k: &str) -> Result<(), String> {
            self.values.lock().unwrap().remove(k);
            Ok(())
        }
    }
    fn runtime() -> (std::path::PathBuf, Arc<Runtime>, Arc<MemoryVault>) {
        let root = std::env::temp_dir().join(format!("helmmeet-models-{}", id()));
        let vault = Arc::new(MemoryVault::default());
        let rt = Runtime::open_with_vault(root.clone(), vault.clone()).unwrap();
        (root, rt, vault)
    }
    fn input(name: &str, url: &str, key: &str) -> ProfileInput {
        ProfileInput {
            profile_id: None,
            expected_version: None,
            display_name: name.into(),
            base_url: url.into(),
            model: "test-model".into(),
            context_tokens: 32768,
            max_output_tokens: 4096,
            timeout_seconds: 10,
            key_action: if key.is_empty() { "clear" } else { "replace" }.into(),
            api_key: key.into(),
        }
    }
    fn mid(rt: &Runtime) -> String {
        rt.create("模型检查", "general").unwrap()["id"]
            .as_str()
            .unwrap()
            .into()
    }
    fn version(rt: &Runtime, m: &str) -> u64 {
        rt.detail(m).unwrap()["version"].as_u64().unwrap()
    }
    fn select(rt: &Runtime, m: &str, p: &str, v: u32, enabled: bool) {
        authorize(rt, m, version(rt, m), enabled, 0, Some((p, v))).unwrap();
    }
    fn seed(rt: &Runtime, m: &str) {
        rt.db()
            .unwrap()
            .execute("UPDATE meetings SET state='completed' WHERE id=?", [m])
            .unwrap();
        let s = id();
        let db = rt.db().unwrap();
        db.execute(
            "INSERT INTO transcript_segments VALUES (?,?,0,1000,'mixed',1,'final')",
            params![s, m],
        )
        .unwrap();
        db.execute(
            "INSERT INTO transcript_revisions VALUES (?,1,?,'合成测试文字','asr',?)",
            params![s, m, now()],
        )
        .unwrap();
    }
    fn receive(stream: &mut TcpStream) -> String {
        stream
            .set_read_timeout(Some(Duration::from_secs(4)))
            .unwrap();
        let mut bytes = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            let n = stream.read(&mut buf).unwrap();
            assert!(n > 0);
            bytes.extend_from_slice(&buf[..n]);
            if let Some(end) = bytes.windows(4).position(|b| b == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&bytes[..end]).to_lowercase();
                let len = headers
                    .lines()
                    .find_map(|l| {
                        l.strip_prefix("content-length:")
                            .and_then(|v| v.trim().parse::<usize>().ok())
                    })
                    .unwrap_or(0);
                if bytes.len() >= end + 4 + len {
                    return String::from_utf8(bytes).unwrap();
                }
            }
        }
    }
    fn respond(stream: &mut TcpStream, code: u16, content: &str) {
        let body = json!({"choices":[{"message":{"content":content}}]}).to_string();
        let _=write!(stream,"HTTP/1.1 {code} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len());
    }
    #[test]
    fn same_endpoint_accounts_are_isolated_and_dtos_never_include_keys() {
        let (root, rt, vault) = runtime();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let mut seen = vec![];
            for _ in 0..2 {
                let (mut s, _) = listener.accept().unwrap();
                seen.push(receive(&mut s));
                respond(
                    &mut s,
                    200,
                    r#"{"summary":[{"text":"连接测试","kind":"discussion","section":"topic","evidence":[{"segment_id":"connection-test","revision":1}]}],"actions":[]}"#,
                );
            }
            seen
        });
        let a = save(&rt, input("账号一", &url, "fake-secret-one")).unwrap();
        let b = save(&rt, input("账号二", &url, "fake-secret-two")).unwrap();
        assert_ne!(a["profile_id"], b["profile_id"]);
        test(&rt, a["profile_id"].as_str().unwrap(), 1).unwrap();
        test(&rt, b["profile_id"].as_str().unwrap(), 1).unwrap();
        let seen = server.join().unwrap();
        assert!(seen[0].contains("Bearer fake-secret-one"));
        assert!(seen[1].contains("Bearer fake-secret-two"));
        let directory = list(&rt).unwrap().to_string();
        assert!(!directory.contains("fake-secret"));
        assert!(!directory.contains("credential_ref"));
        assert_eq!(vault.values.lock().unwrap().len(), 2);
        let configs: String = rt
            .db()
            .unwrap()
            .query_row(
                "SELECT group_concat(config_json) FROM model_configs",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(!configs.contains("fake-secret"));
        // Explicitly reject accidental credential reuse after endpoint edit.
        let mut changed = input("改地址", "https://other.example.test", " ");
        changed.profile_id = a["profile_id"].as_str().map(str::to_string);
        changed.expected_version = Some(1);
        changed.key_action = "keep".into();
        changed.api_key.clear();
        assert!(save(&rt, changed).is_err());
        drop(rt);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn defaults_versions_switching_and_restart_preserve_meeting_binding() {
        let (root, rt, vault) = runtime();
        let a = save(&rt, input("A", "https://a.example.test", "")).unwrap();
        let p = a["profile_id"].as_str().unwrap();
        let m = mid(&rt);
        let mut edit = input("A新版", "https://a.example.test", "");
        edit.profile_id = Some(p.into());
        edit.expected_version = Some(1);
        edit.key_action = "keep".into();
        save(&rt, edit).unwrap();
        assert_eq!(rt.detail(&m).unwrap()["model_profile"]["config_version"], 1);
        assert_eq!(rt.detail(&m).unwrap()["model_profile"]["latest_version"], 2);
        let next = mid(&rt);
        assert_eq!(selected(&rt, &next).unwrap().config_version, 2);
        let b = save(&rt, input("B", "http://127.0.0.1:11434/v1", "")).unwrap();
        set_default(&rt, b["profile_id"].as_str().unwrap()).unwrap();
        assert_eq!(selected(&rt, &m).unwrap().profile_id, p);
        assert_eq!(rt.detail(&mid(&rt)).unwrap()["analysis_mode"], "local");
        let old = version(&rt, &m);
        select(&rt, &m, p, 2, false);
        assert!(authorize(&rt, &m, old, true, 0, Some((p, 1))).is_err());
        assert!(rt
            .db()
            .unwrap()
            .execute("UPDATE model_configs SET config_json='{}' WHERE id=?", [p])
            .is_err());
        drop(rt);
        let rt = Runtime::open_with_vault(root.clone(), vault).unwrap();
        assert_eq!(selected(&rt, &m).unwrap().config_version, 2);
        drop(rt);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn rotation_and_archive_invalidate_only_referencing_meetings() {
        let (root, rt, vault) = runtime();
        let a = save(&rt, input("A", "https://a.example.test", "old-fake")).unwrap();
        let p = a["profile_id"].as_str().unwrap();
        let m = mid(&rt);
        select(&rt, &m, p, 1, true);
        let b = save(&rt, input("B", "https://a.example.test", "other-fake")).unwrap();
        let n = mid(&rt);
        let q = b["profile_id"].as_str().unwrap();
        select(&rt, &n, q, 1, true);
        let unaffected = rt.detail(&n).unwrap()["generation"].clone();
        let mut edit = input("轮换", "https://a.example.test", "new-fake");
        edit.profile_id = Some(p.into());
        edit.expected_version = Some(1);
        save(&rt, edit).unwrap();
        assert!(selected(&rt, &m).is_err());
        assert_eq!(rt.detail(&m).unwrap()["analysis_mode"], "transcript_only");
        assert_eq!(rt.detail(&n).unwrap()["generation"], unaffected);
        assert_eq!(rt.detail(&n).unwrap()["analysis_mode"], "remote");
        assert!(!vault
            .values
            .lock()
            .unwrap()
            .values()
            .any(|v| v == "old-fake"));
        select(&rt, &m, p, 2, true);
        archive(&rt, p, 2).unwrap();
        assert!(selected(&rt, &m).is_err());
        assert!(authorize(&rt, &m, version(&rt, &m), true, 0, None).is_err());
        assert_eq!(rt.detail(&n).unwrap()["generation"], unaffected);
        drop(rt);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn legacy_import_recovers_keychain_failure_and_scrubs_runtime_settings() {
        let root = std::env::temp_dir().join(format!("helmmeet-legacy-{}", id()));
        std::fs::create_dir_all(&root).unwrap();
        let settings = json!({"asr":{"model_path":"test.bin","language":"zh"},"analysis":{"base_url":"https://legacy.example.test","model":"legacy","timeout_seconds":120}});
        std::fs::write(root.join("settings.json"), settings.to_string()).unwrap();
        let vault = Arc::new(MemoryVault::default());
        vault
            .put("https://legacy.example.test", "legacy-fake")
            .unwrap();
        vault.fail.store(true, Ordering::Relaxed);
        let pending = Runtime::open_with_vault(root.clone(), vault.clone()).unwrap();
        assert!(pending.transient.lock().unwrap()["startup_warning"].is_string());
        // Even while credentials are denied, recording metadata remains usable.
        pending.create("仍可创建录音会议", "general").unwrap();
        drop(pending);
        vault.fail.store(false, Ordering::Relaxed);
        let rt = Runtime::open_with_vault(root.clone(), vault.clone()).unwrap();
        let d = list(&rt).unwrap();
        assert_eq!(d["profiles"].as_array().unwrap().len(), 1);
        assert!(vault.get("https://legacy.example.test").unwrap().is_none());
        assert!(!std::fs::read_to_string(root.join("settings.json"))
            .unwrap()
            .contains("legacy.example.test"));
        let c = selected(&rt, &mid(&rt)).unwrap();
        assert_eq!(secret(&rt, &c).unwrap().as_deref(), Some("legacy-fake"));
        drop(rt);
        let rt = Runtime::open_with_vault(root.clone(), vault).unwrap();
        assert_eq!(list(&rt).unwrap()["profiles"].as_array().unwrap().len(), 1);
        drop(rt);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn late_http_result_and_retry_cannot_survive_model_switch() {
        for code in [200, 429] {
            let (root, rt, _) = runtime();
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let a = save(&rt, input("旧模型", &url, "")).unwrap();
            let m = mid(&rt);
            seed(&rt, &m);
            select(&rt, &m, a["profile_id"].as_str().unwrap(), 1, true);
            let (sent, seen) = std::sync::mpsc::channel();
            let (release, wait) = std::sync::mpsc::channel();
            let server = std::thread::spawn(move || {
                let (mut s, _) = listener.accept().unwrap();
                receive(&mut s);
                sent.send(()).unwrap();
                wait.recv_timeout(Duration::from_secs(5)).unwrap();
                respond(&mut s, code, r#"{"summary":[],"actions":[]}"#);
                listener.set_nonblocking(true).unwrap();
                let end = Instant::now() + Duration::from_millis(1500);
                while Instant::now() < end {
                    assert!(
                        listener.accept().is_err(),
                        "old request was retried after switch"
                    );
                    std::thread::sleep(Duration::from_millis(10));
                }
            });
            let job = analysis::submit(rt.clone(), &m, "stage", "", "all").unwrap();
            seen.recv_timeout(Duration::from_secs(5)).unwrap();
            select(&rt, &m, a["profile_id"].as_str().unwrap(), 1, false);
            let until = Instant::now() + Duration::from_secs(2);
            while rt.llm_gate.try_lock().is_err() {
                assert!(
                    Instant::now() < until,
                    "cancelled HTTP kept the model queue blocked"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
            release.send(()).unwrap();
            server.join().unwrap();
            let d = rt.detail(&m).unwrap();
            assert_eq!(d["jobs"][0]["id"], job["job_id"]);
            assert_eq!(d["jobs"][0]["state"], "cancelled");
            assert!(d["artifacts"].as_array().unwrap().is_empty());
            drop(rt);
            std::fs::remove_dir_all(root).unwrap();
        }
    }
    #[test]
    fn queued_generation_is_fenced_before_first_network_dispatch() {
        let (root, rt, _) = runtime();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let a = save(&rt, input("排队模型", &url, "")).unwrap();
        let m = mid(&rt);
        seed(&rt, &m);
        select(&rt, &m, a["profile_id"].as_str().unwrap(), 1, true);
        let gate = rt.llm_gate.lock().unwrap();
        analysis::submit(rt.clone(), &m, "stage", "", "all").unwrap();
        select(&rt, &m, a["profile_id"].as_str().unwrap(), 1, false);
        drop(gate);
        listener.set_nonblocking(true).unwrap();
        let end = Instant::now() + Duration::from_millis(300);
        while Instant::now() < end {
            assert!(listener.accept().is_err());
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(rt.detail(&m).unwrap()["jobs"][0]["state"], "cancelled");
        drop(rt);
        std::fs::remove_dir_all(root).unwrap();
    }
}
