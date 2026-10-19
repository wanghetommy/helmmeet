//! Local, versioned documents/lexicons. Only explicitly bound snapshots enter a meeting.
use crate::db::{err, id, now, Runtime};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
};

#[derive(Clone, Deserialize, Serialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Term {
    #[serde(default)]
    pub id: String,
    pub canonical: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub category: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Save {
    pub id: Option<String>,
    pub expected_version: Option<u32>,
    pub name: String,
    pub kind: String,
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub terms: Vec<Term>,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub set_id: String,
    pub version: u32,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Correction {
    pub segment_id: String,
    pub expected_revision: u32,
    pub hash: String,
}

pub fn list(rt: &Runtime, include_archived: bool) -> Result<Value, String> {
    let db = rt.db()?;
    let mut q=db.prepare("SELECT s.id,s.name,s.kind,s.current_version,s.archived,v.payload_json,(SELECT count(*) FROM meeting_knowledge_bindings b WHERE b.set_id=s.id) FROM knowledge_sets s JOIN knowledge_versions v ON v.set_id=s.id AND v.version=s.current_version WHERE s.archived=0 OR ? ORDER BY s.created_at DESC").map_err(err)?;
    let rows=q.query_map([include_archived],|r|{let p:Value=serde_json::from_str(&r.get::<_,String>(5)?).unwrap_or(json!({}));Ok(json!({"id":r.get::<_,String>(0)?,"name":r.get::<_,String>(1)?,"kind":r.get::<_,String>(2)?,"version":r.get::<_,u32>(3)?,"archived":r.get::<_,bool>(4)?,"term_count":p["terms"].as_array().map_or(0,Vec::len),"characters":p["content"].as_str().map_or(0,|s|s.chars().count()),"bound_meetings":r.get::<_,u32>(6)?}))}).map_err(err)?.collect::<Result<Vec<_>,_>>().map_err(err)?;
    Ok(json!(rows))
}
pub fn get(rt: &Runtime, set: &str, version: Option<u32>) -> Result<Value, String> {
    rt.db()?.query_row("SELECT s.id,v.name,s.kind,v.version,s.archived,v.payload_json FROM knowledge_sets s JOIN knowledge_versions v ON v.set_id=s.id AND v.version=coalesce(?,s.current_version) WHERE s.id=?",params![version,set],|r|{let mut p:Value=serde_json::from_str(&r.get::<_,String>(5)?).unwrap_or(json!({}));p["id"]=json!(r.get::<_,String>(0)?);p["name"]=json!(r.get::<_,String>(1)?);p["kind"]=json!(r.get::<_,String>(2)?);p["version"]=json!(r.get::<_,u32>(3)?);p["archived"]=json!(r.get::<_,bool>(4)?);Ok(p)}).map_err(|_|"资料版本不存在".into())
}
fn validate(input: &mut Save) -> Result<(), String> {
    if input.name.trim().is_empty()
        || input.name.len() > 300
        || !matches!(input.kind.as_str(), "documents" | "terms")
        || input.content.chars().count() > 120000
        || input.terms.len() > 500
    {
        return Err("请检查集合名称、类型及长度（资料最多12万字，词库最多500项）".into());
    }
    if input.kind == "documents" && input.content.trim().is_empty() {
        return Err("资料正文不能为空".into());
    }
    let mut identities = BTreeSet::new();
    for term in &mut input.terms {
        term.canonical = term.canonical.trim().to_string();
        term.aliases = term
            .aliases
            .iter()
            .map(|a| a.trim().to_string())
            .filter(|a| !a.is_empty())
            .collect();
        if term.id.is_empty() {
            term.id = id();
        }
        if term.canonical.is_empty()
            || term.canonical.chars().count() > 80
            || term.aliases.len() > 12
            || term
                .aliases
                .iter()
                .any(|a| a.trim().is_empty() || a.chars().count() > 80)
            || term.category.len() > 80
            || !identities.insert(term.id.clone())
        {
            return Err("请检查标准名、实体ID和别名（每项最多12个别名）".into());
        }
        term.aliases = term
            .aliases
            .iter()
            .map(|a| a.trim().to_string())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
    }
    if input.kind == "documents" {
        input.terms.clear();
    } else {
        input.content.clear();
    }
    Ok(())
}
pub fn save(rt: &Runtime, mut input: Save) -> Result<Value, String> {
    validate(&mut input)?;
    let set = input.id.clone().unwrap_or_else(id);
    let mut db = rt.db()?;
    let tx = db.transaction().map_err(err)?;
    let old: Option<(u32, bool, String)> = tx
        .query_row(
            "SELECT current_version,archived,kind FROM knowledge_sets WHERE id=?",
            [&set],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(err)?;
    let version = match old {
        Some((v, archived, kind)) => {
            if input.expected_version != Some(v) || archived || kind != input.kind {
                return Err("集合版本已变化、已归档或类型冲突，请刷新后重试".into());
            }
            v + 1
        }
        None => {
            if input.id.is_some() || input.expected_version.is_some() {
                return Err("集合不存在".into());
            }
            tx.execute(
                "INSERT INTO knowledge_sets VALUES (?,?,?,1,0,?)",
                params![set, input.name.trim(), input.kind, now()],
            )
            .map_err(err)?;
            1
        }
    };
    let payload = json!({"content":input.content,"terms":input.terms});
    tx.execute(
        "INSERT INTO knowledge_versions VALUES (?,?,?,?,?)",
        params![set, version, input.name.trim(), payload.to_string(), now()],
    )
    .map_err(err)?;
    tx.execute(
        "UPDATE knowledge_sets SET current_version=?,name=? WHERE id=?",
        params![version, input.name.trim(), set],
    )
    .map_err(err)?;
    let chars = input.content.chars().collect::<Vec<_>>();
    for (ordinal, start) in (0..chars.len()).step_by(700).enumerate() {
        let text = chars[start..(start + 800).min(chars.len())]
            .iter()
            .collect::<String>();
        tx.execute(
            "INSERT INTO knowledge_chunks(set_id,version,ordinal,text) VALUES (?,?,?,?)",
            params![set, version, ordinal, text],
        )
        .map_err(err)?;
    }
    tx.commit().map_err(err)?;
    drop(db);
    get(rt, &set, Some(version))
}
pub fn archive(rt: &Runtime, set: &str, expected: u32, archived: bool) -> Result<Value, String> {
    if rt
        .db()?
        .execute(
            "UPDATE knowledge_sets SET archived=? WHERE id=? AND current_version=?",
            params![archived, set, expected],
        )
        .map_err(err)?
        != 1
    {
        return Err("集合版本已变化".into());
    }
    Ok(json!({"saved":true}))
}
pub fn import() -> Result<Value, String> {
    let Some(path) = rfd::FileDialog::new()
        .add_filter("本地文本资料与词表", &["txt", "md", "csv"])
        .pick_file()
    else {
        return Ok(Value::Null);
    };
    let extension = path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if !matches!(extension.as_str(), "txt" | "md" | "csv") {
        return Err("首版支持TXT、Markdown和词库CSV".into());
    }
    let file = std::fs::File::open(&path).map_err(err)?;
    let mut bytes = Vec::new();
    file.take(2_000_001).read_to_end(&mut bytes).map_err(err)?;
    if bytes.len() > 2_000_000 {
        return Err("导入文件超过2MB，请拆分资料".into());
    }
    let content = String::from_utf8(bytes)
        .map_err(|_| "请选择UTF-8编码文本文件")?
        .trim_start_matches('\u{feff}')
        .to_string();
    let name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("导入资料")
        .to_string();
    if extension == "csv" {
        let terms = parse_csv(&content)?;
        return Ok(json!({"name":name,"kind":"terms","content":"","terms":terms}));
    }
    if content.chars().count() > 120000 {
        return Err("正文超过12万字，请拆分资料".into());
    }
    Ok(json!({"name":name,"kind":"documents","content":content,"terms":[]}))
}
pub fn parse_csv(text: &str) -> Result<Vec<Term>, String> {
    let mut rows = vec![];
    let (mut row, mut field, mut quoted) = (vec![], String::new(), false);
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                if quoted && chars.peek() == Some(&'"') {
                    field.push('"');
                    chars.next();
                } else {
                    quoted = !quoted;
                }
            }
            ',' if !quoted => {
                row.push(std::mem::take(&mut field));
            }
            '\n' if !quoted => {
                row.push(std::mem::take(&mut field));
                rows.push(std::mem::take(&mut row));
            }
            '\r' if !quoted => {}
            _ => field.push(c),
        }
    }
    if quoted {
        return Err("CSV引号不完整".into());
    }
    if !field.is_empty() || !row.is_empty() {
        row.push(field);
        rows.push(row);
    }
    if rows.is_empty()
        || !matches!(
            rows[0].first().map(|s| s.trim()),
            Some("标准名" | "canonical")
        )
    {
        return Err("CSV首行应为：标准名,别名,类型；多个别名用 | 分隔".into());
    }
    let mut terms = vec![];
    for r in rows
        .into_iter()
        .skip(1)
        .filter(|r| r.iter().any(|f| !f.trim().is_empty()))
    {
        if r.len() > 3 {
            return Err("CSV仅支持标准名、别名、类型三列".into());
        }
        terms.push(Term {
            id: id(),
            canonical: r[0].trim().to_string(),
            aliases: r
                .get(1)
                .map(|s| {
                    s.split('|')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default(),
            category: r.get(2).cloned().unwrap_or_default(),
        });
    }
    let mut candidate = Save {
        id: None,
        expected_version: None,
        name: "CSV预览".into(),
        kind: "terms".into(),
        content: String::new(),
        terms,
    };
    validate(&mut candidate)?;
    Ok(candidate.terms)
}
fn bound_db(db: &Connection, mid: &str) -> Result<Vec<Value>, String> {
    let mut q=db.prepare("SELECT s.id,v.name,s.kind,v.version,s.current_version,s.archived,v.payload_json FROM meeting_knowledge_bindings b JOIN knowledge_sets s ON s.id=b.set_id JOIN knowledge_versions v ON v.set_id=b.set_id AND v.version=b.version WHERE b.meeting_id=? ORDER BY s.id").map_err(err)?;
    let result=q.query_map([mid],|r|Ok(json!({"set_id":r.get::<_,String>(0)?,"name":r.get::<_,String>(1)?,"kind":r.get::<_,String>(2)?,"version":r.get::<_,u32>(3)?,"latest_version":r.get::<_,u32>(4)?,"archived":r.get::<_,bool>(5)?,"payload":serde_json::from_str::<Value>(&r.get::<_,String>(6)?).unwrap_or(json!({}))}))).map_err(err)?.collect::<Result<Vec<_>,_>>().map_err(err)?;
    Ok(result)
}
pub fn bound(rt: &Runtime, mid: &str) -> Result<Value, String> {
    let db = rt.db()?;
    // Polling needs metadata, never allocate full documents in each meeting snapshot.
    let mut q=db.prepare("SELECT s.id,v.name,s.kind,v.version,s.current_version,s.archived,coalesce(json_array_length(v.payload_json,'$.terms'),0) FROM meeting_knowledge_bindings b JOIN knowledge_sets s ON s.id=b.set_id JOIN knowledge_versions v ON v.set_id=b.set_id AND v.version=b.version WHERE b.meeting_id=? ORDER BY s.id").map_err(err)?;
    let rows=q.query_map([mid],|r|Ok(json!({"set_id":r.get::<_,String>(0)?,"name":r.get::<_,String>(1)?,"kind":r.get::<_,String>(2)?,"version":r.get::<_,u32>(3)?,"latest_version":r.get::<_,u32>(4)?,"archived":r.get::<_,bool>(5)?,"term_count":r.get::<_,u32>(6)?}))).map_err(err)?.collect::<Result<Vec<_>,_>>().map_err(err)?;
    Ok(json!(rows))
}

pub fn bind(
    rt: &Runtime,
    mid: &str,
    expected: u64,
    bindings: Vec<Binding>,
) -> Result<Value, String> {
    rt.check_id(mid)?;
    if bindings.len() > 20 {
        return Err("本场最多选用20个集合".into());
    }
    let mut db = rt.db()?;
    let tx = db.transaction().map_err(err)?;
    let mut seen = BTreeSet::new();
    for b in &bindings {
        if !seen.insert(&b.set_id) {
            return Err("不能重复选用同一集合".into());
        }
        let valid:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM knowledge_versions v JOIN knowledge_sets s ON s.id=v.set_id WHERE v.set_id=? AND v.version=? AND (s.archived=0 OR EXISTS(SELECT 1 FROM meeting_knowledge_bindings b WHERE b.meeting_id=? AND b.set_id=v.set_id AND b.version=v.version)))",params![b.set_id,b.version,mid],|r|r.get(0)).map_err(err)?;
        if !valid {
            return Err("所选资料不存在或已归档，请刷新选择".into());
        }
    }
    if tx.execute("UPDATE meetings SET version=version+1,generation=generation+1 WHERE id=? AND version=? AND state!='deleting'",params![mid,expected]).map_err(err)?!=1{return Err("会议版本已变化，请刷新后选用".into());}
    tx.execute(
        "DELETE FROM meeting_knowledge_bindings WHERE meeting_id=?",
        [mid],
    )
    .map_err(err)?;
    for b in bindings {
        tx.execute(
            "INSERT INTO meeting_knowledge_bindings VALUES (?,?,?)",
            params![mid, b.set_id, b.version],
        )
        .map_err(err)?;
    }
    tx.execute("UPDATE analysis_jobs SET state='cancelled' WHERE meeting_id=? AND state IN ('pending','running')",[mid]).map_err(err)?;
    tx.execute(
        "UPDATE artifacts SET status='stale' WHERE meeting_id=?",
        [mid],
    )
    .map_err(err)?;
    let snapshots = bound_db(&tx, mid)?;
    let (_, conflicts) = rules(&snapshots);
    tx.commit().map_err(err)?;
    Ok(json!({"saved":true,"conflicts":conflicts}))
}
fn rules(snapshots: &[Value]) -> (Vec<(String, String)>, Vec<String>) {
    let mut aliases: BTreeMap<String, BTreeSet<(String, String)>> = BTreeMap::new();
    let mut standards: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for s in snapshots {
        for term in s["payload"]["terms"].as_array().into_iter().flatten() {
            let canonical = term["canonical"].as_str().unwrap_or("").to_string();
            let entity = format!(
                "{}:{}",
                s["set_id"].as_str().unwrap_or(""),
                term["id"].as_str().unwrap_or("")
            );
            standards
                .entry(canonical.clone())
                .or_default()
                .insert(entity.clone());
            for alias in term["aliases"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
            {
                aliases
                    .entry(alias.to_string())
                    .or_default()
                    .insert((entity.clone(), canonical.clone()));
            }
        }
    }
    let (mut mappings, mut conflicts) = (vec![], vec![]);
    for (alias, entities) in aliases {
        if entities.len() != 1 {
            conflicts.push(format!("{alias} 对应多个实体，保留原文待核对"));
            continue;
        }
        let (entity, canonical) = entities.into_iter().next().unwrap();
        if alias == canonical {
            continue;
        }
        if standards
            .get(&alias)
            .is_some_and(|ids| ids.iter().any(|id| id != &entity))
        {
            conflicts.push(format!("{alias} 也是其他实体的标准名，不自动合并"));
            continue;
        }
        mappings.push((alias, canonical));
    }
    mappings.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then(a.0.cmp(&b.0)));
    (mappings, conflicts)
}
fn normalized(text: &str, mappings: &[(String, String)]) -> String {
    let mut out = String::new();
    let mut at = 0;
    while at < text.len() {
        let rest = &text[at..];
        let found = mappings.iter().find(|(alias, _)| {
            if !rest.starts_with(alias) {
                return false;
            }
            if alias.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                let before = text[..at].chars().next_back();
                let after = rest[alias.len()..].chars().next();
                !before.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
                    && !after.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
            } else {
                true
            }
        });
        if let Some((alias, canonical)) = found {
            out.push_str(canonical);
            at += alias.len();
        } else {
            let c = rest.chars().next().unwrap();
            out.push(c);
            at += c.len_utf8();
        }
    }
    out
}
fn manifest(snapshot: &[Value]) -> Vec<Value> {
    snapshot
        .iter()
        .map(|s| json!({"set_id":s["set_id"],"version":s["version"]}))
        .collect()
}
fn fingerprint(mid: &str, segment: &str, rev: u32, text: &str, snapshot: &[Value]) -> String {
    format!(
        "{:x}",
        Sha256::digest(json!([mid, segment, rev, text, manifest(snapshot)]).to_string())
    )
}
pub fn preview_normalization(rt: &Runtime, mid: &str) -> Result<Value, String> {
    rt.check_id(mid)?;
    let db = rt.db()?;
    let snapshots = bound_db(&db, mid)?;
    let (mappings, conflicts) = rules(&snapshots);
    let mut q=db.prepare("SELECT s.id,s.current_revision,r.text,s.start_ms FROM transcript_segments s JOIN transcript_revisions r ON r.segment_id=s.id AND r.revision=s.current_revision WHERE s.meeting_id=? AND s.state='final' ORDER BY s.start_ms LIMIT 2000").map_err(err)?;
    let mut changes = vec![];
    for row in q
        .query_map([mid], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, u32>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, u64>(3)?,
            ))
        })
        .map_err(err)?
    {
        let (s, rev, before, time) = row.map_err(err)?;
        let after = normalized(&before, &mappings);
        if after != before {
            changes.push(json!({"segment_id":s,"expected_revision":rev,"before":before,"after":after,"start_ms":time,"hash":fingerprint(mid,&s,rev,&after,&snapshots)}));
        }
    }
    let mut q=db.prepare("SELECT id,segment_id,before_revision,after_revision,undone FROM normalization_events WHERE meeting_id=? ORDER BY rowid DESC LIMIT 50").map_err(err)?;
    let events=q.query_map([mid],|r|Ok(json!({"id":r.get::<_,String>(0)?,"segment_id":r.get::<_,String>(1)?,"before_revision":r.get::<_,u32>(2)?,"after_revision":r.get::<_,u32>(3)?,"undone":r.get::<_,bool>(4)?}))).map_err(err)?.collect::<Result<Vec<_>,_>>().map_err(err)?;
    Ok(
        json!({"changes":changes,"conflicts":conflicts,"events":events,"notice":"只核对明确别名；人工修订不批量覆盖。人名冲突保留原文。"}),
    )
}
pub fn apply_normalization(
    rt: &Runtime,
    mid: &str,
    changes: Vec<Correction>,
) -> Result<Value, String> {
    rt.check_id(mid)?;
    if changes.is_empty() || changes.len() > 2000 {
        return Err("请选择需要统一的原文".into());
    }
    let mut db = rt.db()?;
    let tx = db.transaction().map_err(err)?;
    let snapshots = bound_db(&tx, mid)?;
    let (mappings, _) = rules(&snapshots);
    for change in &changes {
        let before:String=tx.query_row("SELECT r.text FROM transcript_segments s JOIN transcript_revisions r ON r.segment_id=s.id AND r.revision=s.current_revision WHERE s.id=? AND s.meeting_id=? AND s.current_revision=? AND s.state='final'",params![change.segment_id,mid,change.expected_revision],|r|r.get(0)).map_err(|_|"原文已被修订，请重新预览")?;
        let after = normalized(&before, &mappings);
        if before == after
            || fingerprint(
                mid,
                &change.segment_id,
                change.expected_revision,
                &after,
                &snapshots,
            ) != change.hash
        {
            return Err("词库版本或原文已变化，请重新预览".into());
        }
        tx.execute(
            "INSERT INTO transcript_revisions VALUES (?,?,?,?,'human',?)",
            params![
                change.segment_id,
                change.expected_revision + 1,
                mid,
                after,
                now()
            ],
        )
        .map_err(err)?;
        tx.execute("UPDATE transcript_segments SET current_revision=current_revision+1,state='edited' WHERE id=? AND meeting_id=?",params![change.segment_id,mid]).map_err(err)?;
        tx.execute(
            "INSERT INTO normalization_events VALUES (?,?,?,?,?,?,?,0)",
            params![
                id(),
                mid,
                change.segment_id,
                change.expected_revision,
                change.expected_revision + 1,
                json!(manifest(&snapshots)).to_string(),
                now()
            ],
        )
        .map_err(err)?;
    }
    invalidate(&tx, mid)?;
    tx.commit().map_err(err)?;
    Ok(json!({"applied":changes.len()}))
}
fn invalidate(db: &Connection, mid: &str) -> Result<(), String> {
    db.execute(
        "UPDATE meetings SET generation=generation+1,version=version+1 WHERE id=?",
        [mid],
    )
    .map_err(err)?;
    db.execute(
        "UPDATE artifacts SET status='stale' WHERE meeting_id=?",
        [mid],
    )
    .map_err(err)?;
    db.execute("UPDATE analysis_jobs SET state='cancelled' WHERE meeting_id=? AND state IN('pending','running')",[mid]).map_err(err)?;
    Ok(())
}
pub fn undo_normalization(rt: &Runtime, mid: &str, event: &str) -> Result<Value, String> {
    rt.check_id(mid)?;
    let mut db = rt.db()?;
    let tx = db.transaction().map_err(err)?;
    let(segment,before,after):(String,u32,u32)=tx.query_row("SELECT segment_id,before_revision,after_revision FROM normalization_events WHERE id=? AND meeting_id=? AND undone=0",params![event,mid],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).map_err(|_|"该统一操作不存在或已撤销")?;
    if tx.execute("UPDATE transcript_segments SET current_revision=current_revision+1,state='edited' WHERE id=? AND meeting_id=? AND current_revision=?",params![segment,mid,after]).map_err(err)?!=1{return Err("此后已有人工修订，不能覆盖；请手动核对原稿".into());}
    tx.execute("INSERT INTO transcript_revisions SELECT segment_id,?,meeting_id,text,'human',? FROM transcript_revisions WHERE segment_id=? AND meeting_id=? AND revision=?",params![after+1,now(),segment,mid,before]).map_err(err)?;
    tx.execute(
        "UPDATE normalization_events SET undone=1 WHERE id=?",
        [event],
    )
    .map_err(err)?;
    invalidate(&tx, mid)?;
    tx.commit().map_err(err)?;
    Ok(json!({"undone":true}))
}
pub fn asr_context(rt: &Runtime, mid: &str) -> Result<Value, String> {
    let db = rt.db()?;
    let snapshots = bound_db(&db, mid)?;
    let mut words = BTreeSet::new();
    for s in &snapshots {
        for term in s["payload"]["terms"].as_array().into_iter().flatten() {
            if let Some(name) = term["canonical"].as_str() {
                words.insert(name.to_string());
            }
        }
    }
    let context: String = db
        .query_row("SELECT context_json FROM meetings WHERE id=?", [mid], |r| {
            r.get(0)
        })
        .map_err(err)?;
    let context: Value = serde_json::from_str(&context).map_err(err)?;
    let names = words.into_iter().take(64).collect::<Vec<_>>().join("、");
    let prompt = if names.is_empty() && context["terms"].as_str().unwrap_or("").trim().is_empty() {
        String::new()
    } else {
        format!(
            "术语参考：{names}。{}",
            context["terms"].as_str().unwrap_or("")
        )
        .replace('\0', "")
        .chars()
        .take(768)
        .collect()
    };
    Ok(
        json!({"prompt":prompt,"bindings":snapshots.iter().map(|s|json!({"set_id":s["set_id"],"version":s["version"]})).collect::<Vec<_>>()}),
    )
}
pub fn retrieve(rt: &Runtime, mid: &str, query: &str) -> Result<Value, String> {
    let db = rt.db()?;
    let snapshots = bound_db(&db, mid)?;
    let (mut terms, mut excerpts) = (vec![], vec![]);
    for s in &snapshots {
        for t in s["payload"]["terms"].as_array().into_iter().flatten() {
            let name = t["canonical"].as_str().unwrap_or("");
            if query.contains(name)
                || t["aliases"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .any(|a| query.contains(a))
            {
                terms.push(json!({"set_id":s["set_id"],"version":s["version"],"term":t}));
            }
        }
    }
    let chars = query
        .chars()
        .filter(|c| c.is_alphanumeric())
        .take(180)
        .collect::<Vec<_>>();
    if chars.len() >= 3 {
        let expression = chars
            .windows(3)
            .step_by(2)
            .take(30)
            .map(|v| format!("\"{}\"", v.iter().collect::<String>()))
            .collect::<Vec<_>>()
            .join(" OR ");
        let mut q=db.prepare("SELECT c.id,c.set_id,c.version,c.text,v.name FROM knowledge_search JOIN knowledge_chunks c ON c.id=knowledge_search.rowid JOIN meeting_knowledge_bindings b ON b.set_id=c.set_id AND b.version=c.version JOIN knowledge_versions v ON v.set_id=c.set_id AND v.version=c.version WHERE knowledge_search MATCH ? AND b.meeting_id=? ORDER BY bm25(knowledge_search),c.id LIMIT 5").map_err(err)?;
        excerpts=q.query_map(params![expression,mid],|r|Ok(json!({"chunk_id":r.get::<_,i64>(0)?,"set_id":r.get::<_,String>(1)?,"version":r.get::<_,u32>(2)?,"excerpt":r.get::<_,String>(3)?,"name":r.get::<_,String>(4)?,"source":"background_not_meeting_evidence"}))).map_err(err)?.collect::<Result<Vec<_>,_>>().map_err(err)?;
    } else if chars.len() == 2 {
        let mut q=db.prepare("SELECT c.id,c.set_id,c.version,c.text,v.name FROM knowledge_chunks c JOIN meeting_knowledge_bindings b ON b.set_id=c.set_id AND b.version=c.version JOIN knowledge_versions v ON v.set_id=c.set_id AND v.version=c.version WHERE b.meeting_id=? AND instr(c.text,?)>0 ORDER BY c.id LIMIT 5").map_err(err)?;
        excerpts=q.query_map(params![mid,chars.iter().collect::<String>()],|r|Ok(json!({"chunk_id":r.get::<_,i64>(0)?,"set_id":r.get::<_,String>(1)?,"version":r.get::<_,u32>(2)?,"excerpt":r.get::<_,String>(3)?,"name":r.get::<_,String>(4)?,"source":"background_not_meeting_evidence"}))).map_err(err)?.collect::<Result<Vec<_>,_>>().map_err(err)?;
    }
    Ok(
        json!({"terms":terms.into_iter().take(40).collect::<Vec<_>>(),"excerpts":excerpts,"bindings":snapshots.iter().map(|s|json!({"set_id":s["set_id"],"version":s["version"],"name":s["name"]})).collect::<Vec<_>>(),"rule":"背景资料不是会议共识；事实结论必须引用本场原文，不补造负责人、日期或决定"}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::tests::MemoryVault;
    use std::sync::Arc;
    fn runtime() -> (std::path::PathBuf, Arc<Runtime>, String) {
        let root = std::env::temp_dir().join(format!("helmmeet-knowledge-{}", id()));
        let rt = Runtime::open_with_vault(root.clone(), Arc::new(MemoryVault::default())).unwrap();
        let mid = rt.create("合成词库会议", "general").unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        (root, rt, mid)
    }
    fn input(kind: &str, content: &str, terms: Vec<Term>) -> Save {
        Save {
            id: None,
            expected_version: None,
            name: "合成资料".into(),
            kind: kind.into(),
            content: content.into(),
            terms,
        }
    }
    fn term(entity: &str, name: &str, aliases: &[&str]) -> Term {
        Term {
            id: entity.into(),
            canonical: name.into(),
            aliases: aliases.iter().map(|s| s.to_string()).collect(),
            category: "人名".into(),
        }
    }
    fn attach(rt: &Runtime, mid: &str, set: &str, version: u32) {
        let v = rt.detail(mid).unwrap()["version"].as_u64().unwrap();
        bind(
            rt,
            mid,
            v,
            vec![Binding {
                set_id: set.into(),
                version,
            }],
        )
        .unwrap();
    }
    fn corrections_from(p: &Value) -> Vec<Correction> {
        p["changes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| Correction {
                segment_id: c["segment_id"].as_str().unwrap().into(),
                expected_revision: c["expected_revision"].as_u64().unwrap() as u32,
                hash: c["hash"].as_str().unwrap().into(),
            })
            .collect()
    }
    fn segment(rt: &Runtime, mid: &str, text: &str, state: &str) -> String {
        let sid = id();
        let db = rt.db().unwrap();
        db.execute(
            "INSERT INTO transcript_segments VALUES (?,?,0,1000,'mixed',1,?)",
            params![sid, mid, state],
        )
        .unwrap();
        db.execute(
            "INSERT INTO transcript_revisions VALUES (?,1,?,?,'asr',?)",
            params![sid, mid, text, now()],
        )
        .unwrap();
        sid
    }
    #[test]
    fn bound_versions_are_immutable_and_retrieval_never_leaks_other_collections() {
        let (root, rt, mid) = runtime();
        let a = save(
            &rt,
            input("documents", "音频采集的延迟目标是十毫秒。", vec![]),
        )
        .unwrap();
        let aid = a["id"].as_str().unwrap();
        let private = save(
            &rt,
            input("documents", "音频采集绝密资料不应跨会议出现。", vec![]),
        )
        .unwrap();
        attach(&rt, &mid, aid, 1);
        let mut next = input("documents", "音频采集目标已经变成一百毫秒。", vec![]);
        next.id = Some(aid.into());
        next.expected_version = Some(1);
        save(&rt, next).unwrap();
        assert!(!retrieve(&rt, &mid, "延迟").unwrap()["excerpts"]
            .as_array()
            .unwrap()
            .is_empty());
        let result = retrieve(&rt, &mid, "音频采集延迟").unwrap();
        assert_eq!(result["bindings"][0]["version"], 1);
        assert!(!result["excerpts"].as_array().unwrap().is_empty());
        assert!(result["excerpts"]
            .as_array()
            .unwrap()
            .iter()
            .all(|e| e["set_id"] == aid && e["version"] == 1));
        assert!(!result.to_string().contains("绝密"));
        assert!(!result.to_string().contains("一百"));
        archive(&rt, aid, 2, true).unwrap();
        assert_eq!(
            get(&rt, aid, Some(1)).unwrap()["content"],
            "音频采集的延迟目标是十毫秒。"
        );
        let other = rt.create("另一个合成会议", "general").unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(bind(
            &rt,
            &other,
            1,
            vec![Binding {
                set_id: aid.into(),
                version: 1
            }]
        )
        .is_err());
        assert!(retrieve(&rt, &other, "音频采集").unwrap()["excerpts"]
            .as_array()
            .unwrap()
            .is_empty());
        assert_ne!(private["id"], a["id"]);
        crate::commands::run(rt.clone(), crate::commands::Request::KnowledgeReindex).unwrap();
        assert!(!retrieve(&rt, &mid, "音频采集").unwrap()["excerpts"]
            .as_array()
            .unwrap()
            .is_empty());
        assert!(rt
            .db()
            .unwrap()
            .execute("UPDATE knowledge_versions SET name='overwrite'", [])
            .is_err());
        drop(rt);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn normalization_preview_is_bound_to_versions_preserves_original_and_undo_protects_edits() {
        let (root, rt, mid) = runtime();
        let set = save(
            &rt,
            input("terms", "", vec![term("person", "王鹤", &["王贺", " "])]),
        )
        .unwrap();
        let set = set["id"].as_str().unwrap();
        attach(&rt, &mid, set, 1);
        let sid = segment(&rt, &mid, "王贺确认方案。", "final");
        let human = segment(&rt, &mid, "王贺人工核对保留。", "edited");
        let preview = preview_normalization(&rt, &mid).unwrap();
        assert_eq!(preview["changes"].as_array().unwrap().len(), 1);
        assert_eq!(preview["changes"][0]["after"], "王鹤确认方案。");
        let corrections = || {
            vec![Correction {
                segment_id: sid.clone(),
                expected_revision: 1,
                hash: preview["changes"][0]["hash"].as_str().unwrap().into(),
            }]
        };
        // Changing the binding invalidates a previously reviewed batch even when text matches.
        let mut updated = input("terms", "", vec![term("person", "王鹤", &["王贺"])]);
        updated.id = Some(set.into());
        updated.expected_version = Some(1);
        save(&rt, updated).unwrap();
        attach(&rt, &mid, set, 2);
        assert!(apply_normalization(&rt, &mid, corrections()).is_err());
        let preview = preview_normalization(&rt, &mid).unwrap();
        let changes = corrections_from(&preview);
        apply_normalization(&rt, &mid, changes).unwrap();
        let d = rt.detail(&mid).unwrap();
        assert_eq!(
            d["segments"]
                .as_array()
                .unwrap()
                .iter()
                .find(|s| s["id"] == human)
                .unwrap()["revision"],
            1
        );
        let original: String = rt
            .db()
            .unwrap()
            .query_row(
                "SELECT text FROM transcript_revisions WHERE segment_id=? AND revision=1",
                [&sid],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(original, "王贺确认方案。");
        let event = preview_normalization(&rt, &mid).unwrap()["events"][0]["id"]
            .as_str()
            .unwrap()
            .to_string();
        undo_normalization(&rt, &mid, &event).unwrap();
        assert!(undo_normalization(&rt, &mid, &event).is_err());
        // A later human correction fences undo rather than silently replacing their text.
        let sid2 = segment(&rt, &mid, "王贺再确认。", "final");
        let p = preview_normalization(&rt, &mid).unwrap();
        apply_normalization(&rt, &mid, corrections_from(&p)).unwrap();
        let event = preview_normalization(&rt, &mid).unwrap()["events"][0]["id"]
            .as_str()
            .unwrap()
            .to_string();
        crate::commands::run(
            rt.clone(),
            crate::commands::Request::Revise {
                meeting_id: mid.clone(),
                segment_id: sid2,
                expected_revision: 2,
                text: "人工最终确认".into(),
            },
        )
        .unwrap();
        assert!(undo_normalization(&rt, &mid, &event).is_err());
        assert!(asr_context(&rt, &mid).unwrap()["prompt"]
            .as_str()
            .unwrap()
            .contains("王鹤"));
        crate::commands::run(
            rt.clone(),
            crate::commands::Request::Delete {
                meeting_id: mid.clone(),
            },
        )
        .unwrap();
        assert!(rt.detail(&mid).is_err());
        let broken: u32 = rt
            .db()
            .unwrap()
            .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(broken, 0);
        drop(rt);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn alias_conflicts_and_word_boundaries_are_conservative_and_csv_is_validated() {
        let snapshots = vec![
            json!({"payload":{"terms":[term("a","王鹤",&["王贺"]),term("b","王贺",&[]),term("c","Alice",&["Al"])]}}),
        ];
        let (mappings, conflicts) = rules(&snapshots);
        assert!(!conflicts.is_empty());
        assert_eq!(normalized("王贺与Al；Also", &mappings), "王贺与Alice；Also");
        let csv = "标准名,别名,类型\n王鹤,王贺|老王,人名\n\"产品,平台\",平台,术语";
        let parsed = parse_csv(csv).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[1].canonical, "产品,平台");
        assert!(parse_csv("标准名,别名,类型\n\"未闭合,x,y").is_err());
    }
}
