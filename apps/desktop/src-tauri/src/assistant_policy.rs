//! Versioned user expression preferences. Evidence/IPC rules are never editable.
use crate::db::{err, now, Runtime};
use rusqlite::{params, Transaction};
use serde_json::{json, Value};

pub fn current(rt: &Runtime) -> Result<Value, String> {
    let (version, raw): (u64,String) = rt.db()?.query_row(
        "SELECT b.version,b.value FROM assistant_behavior_versions b JOIN app_preferences p ON p.key='assistant_behavior_version' AND b.version=CAST(p.value AS INTEGER)",[],|r|Ok((r.get(0)?,r.get(1)?))
    ).map_err(err)?;
    let mut value: Value = serde_json::from_str(&raw).map_err(err)?;
    value["version"] = json!(version);
    value["default_persona_prompt"] = json!(meeting_core::dialogue::GENERAL_RULES);
    value["default_persona"] = default_persona();
    Ok(value)
}
pub fn default_persona() -> Value {
    Value::Object(
        meeting_core::dialogue::PERSONA_FIELDS
            .iter()
            .map(|field| (field.key.to_owned(), json!(field.default)))
            .collect(),
    )
}
pub fn validate(input: &Value) -> Result<Value, String> {
    let prompt = input["persona_prompt"]
        .as_str()
        .ok_or("请输入助手人设")?
        .trim();
    if prompt.is_empty() || prompt.chars().count() > 4000 {
        return Err("通用职责与行为规则应为1–4000字".into());
    }
    let fields = input["persona"].as_object().ok_or("人设维度格式无效")?;
    let mut persona = serde_json::Map::new();
    for field in meeting_core::dialogue::PERSONA_FIELDS {
        let text = fields
            .get(field.key)
            .and_then(Value::as_str)
            .ok_or("人设维度必须是文本")?
            .trim();
        if text.chars().count() > field.limit {
            return Err(format!("人设字段{}不能超过{}字", field.label, field.limit));
        }
        persona.insert(field.key.to_owned(), json!(text));
    }
    let simple = input["simple_seconds"].as_u64().ok_or("简单问题时长无效")?;
    let medium = input["medium_seconds"].as_u64().ok_or("中等问题时长无效")?;
    let complex = input["complex_seconds"]
        .as_u64()
        .ok_or("复杂问题时长无效")?;
    if !(3..=15).contains(&simple)
        || !(10..=30).contains(&medium)
        || !(20..=60).contains(&complex)
        || simple > medium
        || medium > complex
    {
        return Err("回答时长应依次递增：简单3–15秒、中等10–30秒、复杂20–60秒".into());
    }
    Ok(
        json!({"persona":persona,"persona_prompt":prompt,"simple_seconds":simple,"medium_seconds":medium,"complex_seconds":complex,"conclusion_first":input["conclusion_first"].as_bool().ok_or("表达偏好无效")?,"one_question":input["one_question"].as_bool().ok_or("追问偏好无效")?}),
    )
}
pub fn save(tx: &Transaction<'_>, input: &Value) -> Result<(), String> {
    let value = validate(input)?;
    let (version,old):(u64,String)=tx.query_row("SELECT b.version,b.value FROM assistant_behavior_versions b JOIN app_preferences p ON p.key='assistant_behavior_version' AND b.version=CAST(p.value AS INTEGER)",[],|r|Ok((r.get(0)?,r.get(1)?))).map_err(err)?;
    if serde_json::from_str::<Value>(&old).map_err(err)? == value {
        return Ok(());
    }
    if input["version"].as_u64() != Some(version) {
        return Err("人设设置已变化，请重新打开设置后保存".into());
    }
    tx.execute(
        "INSERT INTO assistant_behavior_versions VALUES (?,?,?)",
        params![version + 1, value.to_string(), now()],
    )
    .map_err(err)?;
    tx.execute(
        "UPDATE app_preferences SET value=? WHERE key='assistant_behavior_version'",
        [(version + 1).to_string()],
    )
    .map_err(err)?;
    Ok(())
}
#[cfg(test)]
pub fn freeze(rt: &Runtime, context: &Value) -> Result<Value, String> {
    freeze_with_mode(rt, context, "auto")
}
pub fn freeze_with_mode(rt: &Runtime, context: &Value, mode: &str) -> Result<Value, String> {
    let (label, rules) = meeting_core::dialogue::answer_mode(mode).ok_or("问答方式无效")?;
    let mut policy = current(rt)?;
    policy["answer_mode"] = json!(mode);
    policy["mode_label"] = json!(label);
    policy["mode_rules"] = json!(rules);
    for key in ["default_persona_prompt", "default_persona"] {
        policy.as_object_mut().unwrap().remove(key);
    }
    policy["chars_per_second"] = json!(crate::voice::speech_rate(rt));
    policy["assistant_name"] = crate::assistant::settings(rt)?["wake_word"].clone();
    let kind = context["template"]["id"].as_str().unwrap_or("general");
    policy["meeting_type"] = json!(kind);
    let template = &context["template"];
    let rules = if template["policy"].is_object() {
        template["policy"].clone()
    } else {
        crate::templates::defaults(
            kind,
            template["instructions"]
                .as_str()
                .unwrap_or("直接回应当前问题"),
        )
    };
    policy["meeting_strategy"] = rules["qa_rules"].clone();
    policy["meeting_template"] = json!({"id":kind,"version":template["version"],"name":template["name"].as_str().unwrap_or(if context["kind"]=="general" {"通用对话"} else {"通用会议"}),"goal":rules["goal"],"criteria":rules["criteria"],"qa_rules":rules["qa_rules"]});
    policy["template_version"] = context["template"]["version"].clone();
    policy["phase"] = json!(match context["state"].as_str() {
        Some("ready") => "会前",
        Some("recording" | "paused" | "interrupted") => "会中",
        Some(_) => "会后",
        None => "通用对话",
    });
    Ok(policy)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{atomic::Ordering, Arc};
    fn setup() -> (tempfile::TempDir, Arc<Runtime>) {
        let root = tempfile::tempdir().unwrap();
        let rt = Runtime::open_with_vault(
            root.path().into(),
            Arc::new(crate::models::tests::MemoryVault::default()),
        )
        .unwrap();
        (root, rt)
    }
    #[test]
    fn persona_versions_freeze_per_turn_and_settings_save_atomically() {
        let (_root, rt) = setup();
        let initial = current(&rt).unwrap();
        assert_eq!(
            initial["persona_prompt"],
            meeting_core::dialogue::GENERAL_RULES
        );
        assert_eq!(initial["persona"], default_persona());
        let initial_version = initial["version"].as_u64().unwrap();
        let frozen = freeze(
            &rt,
            &json!({"template":{"id":"decision","version":3},"state":"recording"}),
        )
        .unwrap();
        assert_eq!(frozen["phase"], "会中");
        assert_eq!(
            frozen["meeting_strategy"],
            meeting_core::dialogue::meeting_strategy("decision")
        );
        let mut settings = crate::assistant::settings(&rt).unwrap();
        settings["behavior"]["persona_prompt"] = json!("合成测试人设：简短自然");
        settings["behavior"]["persona"]["personality"] = json!("合成性格：坦诚");
        settings["behavior"]["persona"]["expression"] = json!("合成表达：短句，不使用固定口头禅");
        let epoch = rt.voice.epoch.load(Ordering::Acquire);
        crate::assistant::save_settings(&rt, settings.clone()).unwrap();
        assert_eq!(rt.voice.epoch.load(Ordering::Acquire), epoch);
        assert_eq!(current(&rt).unwrap()["version"], initial_version + 1);
        assert_eq!(frozen["version"], initial_version);
        assert_eq!(frozen["persona_prompt"], initial["persona_prompt"]);
        assert_eq!(frozen["persona"], initial["persona"]);
        assert_eq!(
            current(&rt).unwrap()["persona"]["personality"],
            "合成性格：坦诚"
        );
        assert_eq!(
            current(&rt).unwrap()["persona"]["expression"],
            "合成表达：短句，不使用固定口头禅"
        );
        assert!(frozen.get("default_persona").is_none());
        assert!(frozen["persona"].get("education").is_none());
        assert!(frozen["persona"].get("catchphrases").is_none());
        // A stale editor cannot overwrite a newer version or partially save TTS.
        settings["behavior"]["persona_prompt"] = json!("过期编辑");
        settings["tts"]["speaker"] = json!("synthetic-speaker");
        assert!(crate::assistant::save_settings(&rt, settings.clone())
            .unwrap_err()
            .contains("已变化"));
        assert_ne!(
            crate::tts::settings(&rt).unwrap()["speaker"],
            "synthetic-speaker"
        );
        let latest = crate::assistant::settings(&rt).unwrap();
        crate::assistant::save_settings(&rt, latest).unwrap();
        assert_eq!(current(&rt).unwrap()["version"], initial_version + 1);
        assert_eq!(freeze(&rt, &json!({})).unwrap()["phase"], "通用对话");
        assert_eq!(
            freeze(&rt, &json!({"state":"completed"})).unwrap()["phase"],
            "会后"
        );
        let db = rt.db().unwrap();
        assert!(db
            .execute("UPDATE assistant_behavior_versions SET value='{}'", [])
            .is_err());
        assert!(db
            .execute("DELETE FROM assistant_behavior_versions", [])
            .is_err());
    }
    #[test]
    fn one_turn_modes_and_actual_custom_template_are_frozen_without_extra_fields() {
        let (_root, rt) = setup();
        let context = json!({"template":{"id":"copied-custom-id","version":7,"name":"合成模板","policy":{"goal":"合成目标","criteria":"合成判断标准","qa_rules":"合成问答规则","tools":["shell"]}},"state":"recording"});
        for (mode, label, _) in meeting_core::dialogue::ANSWER_MODES {
            let p = freeze_with_mode(&rt, &context, mode).unwrap();
            assert_eq!(p["answer_mode"], mode);
            assert_eq!(p["mode_label"], label);
            assert_eq!(p["meeting_template"]["qa_rules"], "合成问答规则");
            assert_eq!(p["meeting_template"]["version"], 7);
            assert!(p["meeting_template"].get("tools").is_none());
        }
        assert!(freeze_with_mode(&rt, &context, "shell").is_err());
        assert_eq!(freeze(&rt, &context).unwrap()["answer_mode"], "auto");
    }
    #[test]
    fn merged_expression_and_experience_survive_restart_without_new_version() {
        let (root, rt) = setup();
        let mut preferences = crate::assistant::settings(&rt).unwrap();
        preferences["behavior"]["persona"]["expression"] = json!("");
        preferences["behavior"]["persona"]["expertise"] = json!("合成知识与项目实践经验");
        preferences["behavior"]["persona_prompt"] = json!("合成自定义内容原样保留");
        crate::assistant::save_settings(&rt, preferences).unwrap();
        let saved = current(&rt).unwrap();
        drop(rt);
        let reopened = Runtime::open_with_vault(
            root.path().into(),
            Arc::new(crate::models::tests::MemoryVault::default()),
        )
        .unwrap();
        assert_eq!(current(&reopened).unwrap(), saved);
        let policy = freeze(&reopened, &json!({})).unwrap();
        assert_eq!(policy["persona"]["expression"], "");
        assert_eq!(policy["persona"]["expertise"], "合成知识与项目实践经验");
        assert_eq!(policy["persona_prompt"], "合成自定义内容原样保留");
    }
    #[test]
    fn dimension_limits_types_and_supplement_preserve_expression_only_contract() {
        let (_root, rt) = setup();
        let base = current(&rt).unwrap();
        for field in meeting_core::dialogue::PERSONA_FIELDS {
            let mut value = base.clone();
            value["persona"][field.key] = json!("中".repeat(field.limit));
            assert!(validate(&value).is_ok());
            value["persona"][field.key] = json!("中".repeat(field.limit + 1));
            assert!(validate(&value).unwrap_err().contains(field.label));
            value["persona"][field.key] = Value::Null;
            assert!(validate(&value).is_err());
        }
        let mut value = base;
        value["persona"]["tools"] = json!(["shell"]);
        assert!(validate(&value).unwrap()["persona"].get("tools").is_none());
        value["persona_prompt"] = json!("中".repeat(4001));
        assert!(validate(&value).is_err());
        value["persona_prompt"] = json!("  保留我的补充人设  ");
        for field in meeting_core::dialogue::PERSONA_FIELDS {
            value["persona"][field.key] = json!("");
        }
        assert_eq!(
            validate(&value).unwrap()["persona_prompt"],
            "保留我的补充人设"
        );
    }
    #[test]
    fn expression_preferences_cannot_inject_model_or_permission_fields() {
        let (_root, rt) = setup();
        let mut value = current(&rt).unwrap();
        value["max_output_tokens"] = json!(8192);
        value["tools"] = json!(["shell"]);
        let clean = validate(&value).unwrap();
        assert!(clean.get("max_output_tokens").is_none());
        assert!(clean.get("tools").is_none());
        value["simple_seconds"] = json!(31);
        assert!(validate(&value).is_err());
        value["simple_seconds"] = json!(10);
        value["persona_prompt"] = json!(" ");
        assert!(validate(&value).is_err()); // general duties must remain explicit
        for field in meeting_core::dialogue::PERSONA_FIELDS {
            value["persona"][field.key] = json!("");
        }
        assert!(validate(&value).is_err());
    }
}
