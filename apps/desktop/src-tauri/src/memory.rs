//! Cumulative local index of validated semantic summaries. Never invents a summary.
use crate::db::{err, Runtime};
use rusqlite::OptionalExtension;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, HashSet};

pub fn context(rt: &Runtime, mid: &str, scope: &str) -> Result<Value, String> {
    if scope != "all" {
        return Ok(Value::Null);
    }
    let db = rt.db()?;
    let binding: Option<(String, u64)> = db
        .query_row(
            "SELECT profile_id,config_version FROM meeting_model_bindings WHERE meeting_id=?",
            [mid],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(err)?;
    let mut q = db.prepare("SELECT s.id,s.current_revision,s.start_ms,s.end_ms,length(r.text),c.revision FROM transcript_segments s JOIN transcript_revisions r ON r.segment_id=s.id AND r.revision=s.current_revision AND r.meeting_id=s.meeting_id LEFT JOIN summary_coverage c ON c.meeting_id=s.meeting_id AND c.segment_id=s.id WHERE s.meeting_id=? AND s.state IN('final','edited') ORDER BY s.start_ms,s.id").map_err(err)?;
    let originals = q
        .query_map([mid], |r| {
            Ok((
                (r.get::<_, String>(0)?, r.get::<_, u64>(1)?),
                (
                    r.get::<_, u64>(2)?,
                    r.get::<_, u64>(3)?,
                    r.get::<_, usize>(4)?,
                    r.get::<_, Option<u64>>(5)?,
                ),
            ))
        })
        .map_err(err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(err)?;
    let current = originals.iter().cloned().collect::<HashMap<_, _>>();
    let mut q = db.prepare("SELECT a.id,a.content_json,a.coverage_json,mm.source_manifest FROM artifacts a JOIN meetings m ON m.id=a.meeting_id LEFT JOIN meeting_memories mm ON mm.artifact_id=a.id WHERE a.meeting_id=? AND a.type='stage' AND a.status!='stale' AND m.context_consent=1 AND json_extract(a.content_json,'$.manual_text') IS NULL ORDER BY a.rowid").map_err(err)?;
    let rows = q
        .query_map([mid], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
            ))
        })
        .map_err(err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(err)?;
    let mut facts = vec![];
    let mut ids = HashSet::new();
    let mut covered = HashSet::new();
    let mut artifact_ids = vec![];
    let mut topics: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    let mut overview = vec![];
    for (aid, body, coverage, manifest) in rows {
        let body: Value = serde_json::from_str(&body).map_err(err)?;
        let coverage: Value = serde_json::from_str(&coverage).map_err(err)?;
        if binding
            .as_ref()
            .is_none_or(|(p, v)| coverage["profile_id"] != *p || coverage["config_version"] != *v)
        {
            continue;
        }
        artifact_ids.push(aid.clone());
        if let Some(manifest) = manifest {
            let manifest: Value = serde_json::from_str(&manifest).map_err(err)?;
            for r in manifest.as_array().into_iter().flatten() {
                if let (Some(s), Some(rev)) = (r["segment_id"].as_str(), r["revision"].as_u64()) {
                    let pair = (s.to_owned(), rev);
                    if current.get(&pair).is_some_and(|v| v.3 == Some(rev)) {
                        covered.insert(pair);
                    }
                }
            }
        }
        let mut latest = vec![];
        for item in body["summary"].as_array().into_iter().flatten() {
            let mut sources = vec![];
            let mut start = u64::MAX;
            let mut end = 0;
            let mut valid = true;
            for e in item["evidence"].as_array().into_iter().flatten() {
                let pair = (
                    e["segment_id"].as_str().unwrap_or("").to_owned(),
                    e["revision"].as_u64().unwrap_or(0),
                );
                if let Some(v) = current.get(&pair) {
                    start = start.min(v.0);
                    end = end.max(v.1);
                    sources.push(format!("meeting:{}:{}", pair.0, pair.1));
                } else {
                    valid = false;
                    break;
                }
            }
            if !valid || sources.is_empty() {
                continue;
            }
            let text = item["text"].as_str().unwrap_or("");
            let topic = text
                .split(['：', ':'])
                .next()
                .filter(|s| s.chars().count() <= 24)
                .unwrap_or(item["section"].as_str().unwrap_or("议题"));
            let fact = json!({"text":text,"kind":item["kind"],"section":item["section"],"topic":topic,"sources":sources,"start_ms":start,"end_ms":end,"artifact_id":aid});
            latest.push(fact.clone());
            // Identical claims at identical sources collapse; changes at later
            // sources remain chronological even if their wording is identical.
            let signature = json!([text, sources]).to_string();
            if !ids.insert(signature) {
                continue;
            }
            topics.entry(topic.into()).or_default().push(facts.len());
            facts.push(fact);
        }
        if !latest.is_empty() {
            overview = latest;
        }
    }
    // This is a selected view of a persistent full index, not a semantic rewrite.
    // Preserve early goals, newest statements, and representatives across time.
    let mut priority = vec![];
    for rows in topics.values() {
        if let Some(i) = rows.first() {
            priority.push(*i);
        }
        if let Some(i) = rows.last() {
            priority.push(*i);
        }
    }
    priority.extend((0..facts.len()).rev());
    let mut selected = HashSet::new();
    let mut bytes = 0;
    for i in priority {
        let n = facts[i].to_string().len();
        if bytes + n <= 24 * 1024 && selected.insert(i) {
            bytes += n;
        }
    }
    let total = facts.len();
    let facts = facts
        .into_iter()
        .enumerate()
        .filter_map(|(i, f)| selected.contains(&i).then_some(f))
        .collect::<Vec<_>>();
    let mut first_gap = None;
    let mut gaps = vec![];
    for (pair, v) in &originals {
        if !covered.contains(pair) {
            first_gap.get_or_insert(v.0);
            if gaps.len() < 64 {
                gaps.push(
                    json!({"segment_id":pair.0,"revision":pair.1,"start_ms":v.0,"end_ms":v.1}),
                );
            }
        }
    }
    let topic_names = topics.keys().cloned().collect::<Vec<_>>();
    let mut view = json!({"artifact_ids":artifact_ids,"facts":facts,"overview":overview,"topics":topic_names,"facts_total":total,"facts_selected":selected.len(),"processed_segments":covered.len(),"segments_total":originals.len(),"pending_segments":originals.len().saturating_sub(covered.len()),"through_ms":first_gap.unwrap_or_else(||originals.last().map_or(0,|(_,v)|v.1)),"missing_ranges":gaps,"partial":selected.len()<total || covered.len()<originals.len(),"policy":"累计会议记忆是语义索引，处理覆盖不等于细节完整；较新原文优先，重要事实需核对原文；摘要没写不代表会议没提到"});
    // The start protocol itself is bounded, independently of model requests.
    while view.to_string().len() > 40 * 1024 {
        if view["overview"]
            .as_array_mut()
            .is_some_and(|a| a.pop().is_some())
        {
            view["partial"] = json!(true);
            continue;
        }
        if view["artifact_ids"]
            .as_array_mut()
            .is_some_and(|a| a.pop().is_some())
        {
            continue;
        }
        if view["topics"]
            .as_array_mut()
            .is_some_and(|a| a.pop().is_some())
        {
            continue;
        }
        break;
    }
    Ok(view)
}
