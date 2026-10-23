//! Recoverable projection; original evidence and its revisions are never merged/deleted.
use crate::db::err;
use meeting_core::paragraph::{self, Pause, Segment};
use rusqlite::Connection;
use serde_json::{json, Value};
pub fn join(parts: impl IntoIterator<Item = String>) -> String {
    let mut text = String::new();
    for part in parts {
        if text.chars().last().is_some_and(|c| {
            c.is_ascii_alphanumeric() || ['.', '!', '?', ',', ';', ':'].contains(&c)
        }) && part
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric())
        {
            text.push(' ');
        }
        text.push_str(&part);
    }
    text
}
pub fn project(
    db: &Connection,
    mid: &str,
    rows: &[Value],
    active: bool,
) -> Result<Vec<Value>, String> {
    let mut statement = db
        .prepare("SELECT start_ms,end_ms FROM speech_pauses WHERE meeting_id=? ORDER BY start_ms")
        .map_err(err)?;
    let mut pauses = statement
        .query_map([mid], |r| {
            Ok(Pause {
                start: r.get(0)?,
                end: r.get(1)?,
            })
        })
        .map_err(err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(err)?;
    let mut q = db
        .prepare("SELECT start_ms,end_ms FROM asr_chunks WHERE meeting_id=? ORDER BY start_ms")
        .map_err(err)?;
    let intervals = q
        .query_map([mid], |r| Ok((r.get::<_, u64>(0)?, r.get::<_, u64>(1)?)))
        .map_err(err)?;
    let mut processed = 0;
    for interval in intervals {
        let (start, end) = interval.map_err(err)?;
        if start > processed {
            break;
        }
        processed = processed.max(end);
    }
    pauses.retain(|p| p.start <= processed);
    let recorded: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM speech_activity_epochs WHERE meeting_id=?)",
            [mid],
            |r| r.get(0),
        )
        .map_err(err)?;
    let mut statement=db.prepare("SELECT recording_start_ms FROM recording_epochs WHERE meeting_id=? UNION SELECT recording_end_ms FROM recording_epochs WHERE meeting_id=? AND recording_end_ms IS NOT NULL UNION SELECT start_ms FROM coverage_intervals WHERE meeting_id=? AND kind LIKE 'capture_%' AND state='missing' ORDER BY 1").map_err(err)?;
    let hard = statement
        .query_map([mid, mid, mid], |r| r.get::<_, u64>(0))
        .map_err(err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(err)?;
    let segments = rows
        .iter()
        .map(|r| Segment {
            start: r["start_ms"].as_u64().unwrap_or(0),
            end: r["end_ms"].as_u64().unwrap_or(0),
            text: r["text"].as_str().unwrap_or("").into(),
            speaker: r["speaker_id"].as_str().map(str::to_owned),
        })
        .collect::<Vec<_>>();
    Ok(paragraph::project(&segments,&pauses,&hard,!recorded && !active,active).iter().map(|g|{
        let members=&rows[g.first..g.end];let first=&members[0];
        json!({"id":format!("p-{}",first["id"].as_str().or_else(||first["segment_id"].as_str()).unwrap_or("")),
            "start_ms":first["start_ms"],"end_ms":members.last().unwrap()["end_ms"],
            "text":join(members.iter().map(|r|r["text"].as_str().unwrap_or("").to_owned())),
            "segment_ids":members.iter().map(|r|r["id"].as_str().or_else(||r["segment_id"].as_str()).unwrap_or("")).collect::<Vec<_>>(),
            "members":members.iter().map(|r|json!({"segment_id":r["id"].as_str().or_else(||r["segment_id"].as_str()),"revision":r["revision"]})).collect::<Vec<_>>(),
            "open":g.open,"reason":g.reason,"boundary_source":if recorded{"vad"}else if active{"unknown"}else{"historical_gaps"}})
    }).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    #[test]
    fn vad_boundaries_wait_for_asr_and_recover_without_rewriting_evidence() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Arc::new(crate::models::tests::MemoryVault::default());
        let rt = crate::db::Runtime::open_with_vault(dir.path().into(), vault.clone()).unwrap();
        let mid = rt.create("合成发言段恢复", "general").unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned();
        let ids = (0..3).map(|_| crate::db::id()).collect::<Vec<_>>();
        {
            let db = rt.db().unwrap();
            db.execute(
                "UPDATE meetings SET state='recording',recording_duration_ms=20000 WHERE id=?",
                [&mid],
            )
            .unwrap();
            db.execute("INSERT INTO recording_epochs(id,meeting_id,recording_start_ms,wall_start,reason) VALUES ('e',?,0,'synthetic','recording')",[&mid]).unwrap();
            db.execute("INSERT INTO speech_activity_epochs VALUES ('e',?)", [&mid])
                .unwrap();
            db.execute(
                "INSERT INTO speech_pauses VALUES (?,'e',16000,18000)",
                [&mid],
            )
            .unwrap();
            for (i, (a, b)) in [(0, 8000), (8000, 16000), (18000, 20000)]
                .iter()
                .enumerate()
            {
                db.execute(
                    "INSERT INTO transcript_segments VALUES (?,?,?,?,'mixed',1,'final')",
                    rusqlite::params![ids[i], mid, a, b],
                )
                .unwrap();
                db.execute("INSERT INTO transcript_revisions VALUES (?,1,?,'这是合成连续发言。','asr','synthetic')",rusqlite::params![ids[i],mid]).unwrap();
            }
        }
        assert_eq!(
            rt.detail(&mid).unwrap()["paragraphs"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        rt.db()
            .unwrap()
            .execute("INSERT INTO asr_chunks VALUES (?,0,20000)", [&mid])
            .unwrap();
        let detail = rt.detail(&mid).unwrap();
        assert_eq!(detail["paragraphs"].as_array().unwrap().len(), 2);
        assert_eq!(
            detail["paragraphs"][0]["segment_ids"],
            json!([ids[0], ids[1]])
        );
        let snapshot = crate::workspace::transcript(&rt, &mid, "current", false).unwrap();
        assert_eq!(snapshot["segments"].as_array().unwrap().len(), 3);
        assert_eq!(snapshot["paragraphs"].as_array().unwrap().len(), 2);
        assert_eq!(
            snapshot["text"].as_str().unwrap().matches("\n\n").count(),
            1
        );
        drop(rt);
        let rt = crate::db::Runtime::open_with_vault(dir.path().into(), vault).unwrap();
        let recovered = rt.detail(&mid).unwrap();
        assert_eq!(
            detail["paragraphs"][0]["members"],
            recovered["paragraphs"][0]["members"]
        );
        assert_eq!(
            detail["paragraphs"][0]["id"],
            recovered["paragraphs"][0]["id"]
        );
        assert_eq!(recovered["segments"].as_array().unwrap().len(), 3);
    }
}
