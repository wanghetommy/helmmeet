//! Immutable meeting rules shared by dialogue and minutes; no model calls.
use crate::db::{err, id, Runtime};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};

pub fn defaults(kind: &str, instructions: &str) -> Value {
    json!({"goal":meeting_core::dialogue::meeting_goal(kind),"criteria":instructions,
        "qa_rules":meeting_core::dialogue::meeting_strategy(kind)})
}
pub fn validate(policy: &Value) -> Result<Value, String> {
    let mut clean = json!({});
    for (key, label, limit) in [
        ("goal", "会议目标", 1000),
        ("criteria", "分析重点与判断标准", 12000),
        ("qa_rules", "会中问答规则", 2000),
    ] {
        let text = policy[key]
            .as_str()
            .ok_or_else(|| format!("{label}必须是文本"))?
            .trim();
        if text.is_empty() || text.chars().count() > limit {
            return Err(format!("{label}应为1–{limit}字"));
        }
        clean[key] = json!(text);
    }
    Ok(clean)
}
pub fn seed_missing(db: &Connection) -> Result<(), String> {
    let rows=db.prepare("SELECT template_id,version,instructions FROM template_versions WHERE NOT EXISTS(SELECT 1 FROM template_guidance g WHERE g.template_id=template_versions.template_id AND g.version=template_versions.version)").map_err(err)?.query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,u64>(1)?,r.get::<_,String>(2)?))).map_err(err)?.collect::<Result<Vec<_>,_>>().map_err(err)?;
    let tx = db.unchecked_transaction().map_err(err)?;
    for (key, version, instructions) in rows {
        let policy = defaults(&key, &instructions);
        tx.execute(
            "INSERT OR IGNORE INTO template_guidance VALUES (?,?,?)",
            params![key, version, policy.to_string()],
        )
        .map_err(err)?;
    }
    tx.commit().map_err(err)
}
pub fn policy(
    db: &Connection,
    key: &str,
    version: u64,
    instructions: &str,
) -> Result<Value, String> {
    let raw: Option<String> = db
        .query_row(
            "SELECT policy_json FROM template_guidance WHERE template_id=? AND version=?",
            params![key, version],
            |r| r.get(0),
        )
        .optional()
        .map_err(err)?;
    raw.map(|s| serde_json::from_str(&s).map_err(err))
        .unwrap_or_else(|| Ok(defaults(key, instructions)))
}
pub fn bound(db: &Connection, meeting: &str) -> Result<Value, String> {
    let row:Option<(String,u64,String,String)>=db.query_row("SELECT v.template_id,v.version,v.name,v.instructions FROM template_versions v JOIN meetings m ON m.assistant_id=v.template_id AND m.template_version=v.version WHERE m.id=?",[meeting],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional().map_err(err)?;
    if let Some((key, version, name, instructions)) = row {
        let policy = policy(db, &key, version, &instructions)?;
        Ok(
            json!({"id":key,"version":version,"name":name,"instructions":instructions,"policy":policy}),
        )
    } else {
        Ok(Value::Null)
    }
}
pub fn list(rt: &Runtime) -> Result<Value, String> {
    let db = rt.db()?;
    let rows = db
        .prepare("SELECT id,version,name,instructions FROM assistant_templates ORDER BY name")
        .map_err(err)?
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, u64>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })
        .map_err(err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(err)?;
    Ok(json!(rows.into_iter().map(|(key,version,name,instructions)|{
        Ok(json!({"id":key,"version":version,"name":name,"instructions":instructions,"policy":policy(&db,&key,version,&instructions)?}))
    }).collect::<Result<Vec<Value>,String>>()?))
}
pub struct Input {
    pub id: Option<String>,
    pub name: String,
    pub instructions: String,
    pub policy: Option<Value>,
    pub expected_version: Option<u64>,
}
pub fn save(rt: &Runtime, input: Input) -> Result<Value, String> {
    let Input {
        id: key,
        name,
        instructions,
        policy: given,
        expected_version,
    } = input;
    let name = name.trim();
    let instructions = instructions.trim();
    if name.is_empty() || name.len() > 200 || instructions.is_empty() || instructions.len() > 12000
    {
        return Err("模板名称或纪要整理规则为空或超长".into());
    }
    let key = key.unwrap_or_else(id);
    let mut db = rt.db()?;
    let tx = db.transaction().map_err(err)?;
    let old: Option<u64> = tx
        .query_row(
            "SELECT version FROM assistant_templates WHERE id=?",
            [&key],
            |r| r.get(0),
        )
        .optional()
        .map_err(err)?;
    if old.is_some() && expected_version.is_some() && old != expected_version {
        return Err("模板已更新，请刷新后保存".into());
    }
    let rules = match given {
        Some(v) => validate(&v)?,
        None => match old {
            Some(v) => policy(&tx, &key, v, instructions)?,
            None => defaults(&key, instructions),
        },
    };
    tx.execute("INSERT INTO assistant_templates VALUES (?,1,?,?) ON CONFLICT(id) DO UPDATE SET name=excluded.name,instructions=excluded.instructions,version=version+1",params![key,name,instructions]).map_err(err)?;
    tx.execute("INSERT INTO template_versions SELECT id,version,name,instructions FROM assistant_templates WHERE id=?",[&key]).map_err(err)?;
    let version = old.unwrap_or(0) + 1;
    tx.execute(
        "INSERT INTO template_guidance VALUES (?,?,?)",
        params![key, version, rules.to_string()],
    )
    .map_err(err)?;
    tx.commit().map_err(err)?;
    Ok(json!({"id":key,"version":version}))
}
pub fn minutes_instructions(template: &Value) -> String {
    let policy = &template["policy"];
    if policy.is_object() {
        format!(
            "会议目标：{}\n分析重点与判断标准：{}\n纪要整理规则：{}",
            policy["goal"].as_str().unwrap_or(""),
            policy["criteria"].as_str().unwrap_or(""),
            template["instructions"]
                .as_str()
                .unwrap_or("整理讨论、决定、风险与行动，引用原文")
        )
    } else {
        template["instructions"]
            .as_str()
            .unwrap_or("整理讨论、决定、风险与行动，引用原文")
            .into()
    }
}
pub fn adopt(rt: &Runtime, mid: &str, expected: u64, version: u64) -> Result<Value, String> {
    rt.check_id(mid)?;
    let _gate = rt.model_gate.lock().map_err(|_| "模型锁异常")?;
    let mut db = rt.db()?;
    let tx = db.transaction().map_err(err)?;
    let active:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM analysis_jobs WHERE meeting_id=? AND state IN ('pending','running') UNION ALL SELECT 1 FROM assistant_turns t JOIN assistant_conversations c ON c.id=t.conversation_id WHERE c.meeting_id=? AND t.state IN ('pending','running'))",params![mid,mid],|r|r.get(0)).map_err(err)?;
    if active {
        return Err("本场正在生成内容，完成后再采用新版模板".into());
    }
    let changed=tx.execute("UPDATE meetings SET template_version=?,template_source='selected',version=version+1,generation=generation+1 WHERE id=? AND version=? AND state NOT IN ('ready','deleting') AND ?=(SELECT version FROM assistant_templates WHERE id=meetings.assistant_id) AND EXISTS(SELECT 1 FROM template_guidance WHERE template_id=meetings.assistant_id AND version=?)",params![version,mid,expected,version,version]).map_err(err)?;
    if changed != 1 {
        return Err("会议或模板版本已变化，请刷新后重试".into());
    }
    tx.commit().map_err(err)?;
    Ok(json!({"adopted":true}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    #[test]
    fn copied_rules_freeze_by_version_and_require_explicit_idle_adoption() {
        let root = tempfile::tempdir().unwrap();
        let rt = Runtime::open_with_vault(
            root.path().into(),
            Arc::new(crate::models::tests::MemoryVault::default()),
        )
        .unwrap();
        let original = list(&rt)
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["id"] == "review")
            .unwrap()
            .clone();
        let mut custom = original["policy"].clone();
        custom["qa_rules"] = json!("合成自定义规则：先比较成本，再讨论风险");
        let made = save(
            &rt,
            Input {
                id: None,
                name: "合成复制模板".into(),
                instructions: "合成纪要规则".into(),
                policy: Some(custom.clone()),
                expected_version: None,
            },
        )
        .unwrap();
        let key = made["id"].as_str().unwrap();
        let mid = id();
        rt.db().unwrap().execute("INSERT INTO meetings(id,title,timezone,created_at,assistant_id,template_version,state) VALUES (?,'synthetic','Asia/Shanghai','now',?,1,'completed')",params![mid,key]).unwrap();
        let before = bound(&rt.db().unwrap(), &mid).unwrap();
        assert_eq!(before["policy"], custom);
        let frozen = crate::assistant_policy::freeze_with_mode(
            &rt,
            &json!({"template":before,"state":"completed"}),
            "judge",
        )
        .unwrap();
        assert_eq!(frozen["meeting_template"]["qa_rules"], custom["qa_rules"]);
        assert_eq!(frozen["answer_mode"], "judge");
        assert!(!minutes_instructions(&before).contains("先比较成本"));
        assert!(minutes_instructions(&before).contains(custom["goal"].as_str().unwrap()));
        let mut next = custom.clone();
        next["qa_rules"] = json!("新版合成问答规则");
        save(
            &rt,
            Input {
                id: Some(key.into()),
                name: "合成新版本".into(),
                instructions: "新版纪要".into(),
                policy: Some(next.clone()),
                expected_version: Some(1),
            },
        )
        .unwrap();
        assert_eq!(bound(&rt.db().unwrap(), &mid).unwrap(), before);
        assert!(save(
            &rt,
            Input {
                id: Some(key.into()),
                name: "过期".into(),
                instructions: "过期".into(),
                policy: Some(custom),
                expected_version: Some(1)
            }
        )
        .is_err());
        let (version, generation): (u64, u64) = rt
            .db()
            .unwrap()
            .query_row(
                "SELECT version,generation FROM meetings WHERE id=?",
                [&mid],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert!(adopt(&rt, &mid, version + 1, 2).is_err());
        let conversation = crate::assistant::open(&rt, Some(mid.clone()), false).unwrap();
        let tid = id();
        rt.db().unwrap().execute("INSERT INTO assistant_turns(id,conversation_id,epoch,question,source,state,created_at) VALUES (?,?,0,'synthetic','text','running','now')",params![tid,conversation["id"].as_str().unwrap()]).unwrap();
        assert!(adopt(&rt, &mid, version, 2)
            .unwrap_err()
            .contains("正在生成"));
        rt.db()
            .unwrap()
            .execute(
                "UPDATE assistant_turns SET state='succeeded' WHERE id=?",
                [tid],
            )
            .unwrap();
        adopt(&rt, &mid, version, 2).unwrap();
        assert_eq!(bound(&rt.db().unwrap(), &mid).unwrap()["policy"], next);
        assert_eq!(
            frozen["meeting_template"]["qa_rules"],
            before["policy"]["qa_rules"]
        );
        assert_eq!(
            rt.db()
                .unwrap()
                .query_row("SELECT generation FROM meetings WHERE id=?", [&mid], |r| {
                    r.get::<_, u64>(0)
                })
                .unwrap(),
            generation + 1
        );
        assert!(rt
            .db()
            .unwrap()
            .execute("UPDATE template_guidance SET policy_json='{}'", [])
            .is_err());
        assert!(rt
            .db()
            .unwrap()
            .execute("DELETE FROM template_guidance", [])
            .is_err());
    }
    #[test]
    fn policy_fields_are_bounded_and_cannot_add_permissions() {
        let mut v = defaults("review", "原有纪要规则");
        v["tools"] = json!(["shell"]);
        assert!(validate(&v).unwrap().get("tools").is_none());
        for (key, limit) in [("goal", 1000), ("criteria", 12000), ("qa_rules", 2000)] {
            let mut value = v.clone();
            value[key] = json!("中".repeat(limit));
            assert!(validate(&value).is_ok());
            value[key] = json!("中".repeat(limit + 1));
            assert!(validate(&value).is_err());
            value[key] = json!("");
            assert!(validate(&value).is_err());
        }
    }
}
