BEGIN IMMEDIATE;
ALTER TABLE summary_schedules RENAME TO summary_schedules_v11;
CREATE TABLE summary_schedules (
 meeting_id TEXT PRIMARY KEY REFERENCES meetings(id) ON DELETE CASCADE,
 enabled INTEGER NOT NULL DEFAULT 0 CHECK(enabled IN(0,1)),
 mode TEXT NOT NULL DEFAULT 'three_minutes' CHECK(mode IN('realtime','three_minutes','economy','legacy_5','legacy_10')),
 version INTEGER NOT NULL DEFAULT 1, failures INTEGER NOT NULL DEFAULT 0,
 retry_after_utc INTEGER NOT NULL DEFAULT 0, paused_reason TEXT,
 tail_done INTEGER NOT NULL DEFAULT 1 CHECK(tail_done IN(0,1)),
 migration_notice TEXT, completed_asr_ms INTEGER NOT NULL DEFAULT 0
);
INSERT INTO summary_schedules(meeting_id,enabled,mode,version,failures,retry_after_utc,paused_reason,tail_done,migration_notice)
 SELECT meeting_id,enabled,CASE WHEN mode='minute' THEN 'three_minutes' ELSE mode END,
 version+1,failures,retry_after_utc,paused_reason,tail_done,
 CASE WHEN mode='minute' THEN '原1分钟档已调整为3分钟' ELSE NULL END FROM summary_schedules_v11;
DROP TABLE summary_schedules_v11;
UPDATE analysis_jobs SET state='cancelled',error_code='SCHEDULE_UPGRADED'
 WHERE state IN('pending','running') AND id IN (SELECT job_id FROM analysis_origins WHERE automatic=1);
CREATE TABLE meeting_memories (
 artifact_id TEXT PRIMARY KEY REFERENCES artifacts(id) ON DELETE CASCADE,
 meeting_id TEXT NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
 generation INTEGER NOT NULL, consent_version INTEGER NOT NULL,
 source_manifest TEXT NOT NULL CHECK(json_valid(source_manifest)),
 created_at TEXT NOT NULL
);
CREATE INDEX meeting_memory_by_meeting ON meeting_memories(meeting_id,created_at);
CREATE TABLE summary_fragments (
 meeting_id TEXT NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
 segment_id TEXT NOT NULL, revision INTEGER NOT NULL,
 char_start INTEGER NOT NULL, char_end INTEGER NOT NULL CHECK(char_end>char_start),
 artifact_id TEXT NOT NULL REFERENCES artifacts(id) ON DELETE CASCADE,
 PRIMARY KEY(meeting_id,segment_id,revision,char_start,char_end),
 FOREIGN KEY(segment_id,revision,meeting_id) REFERENCES transcript_revisions(segment_id,revision,meeting_id) ON DELETE CASCADE
);
CREATE TABLE summary_model_backoff (model_key TEXT PRIMARY KEY, retry_after_utc INTEGER NOT NULL);
CREATE TABLE assistant_compactions (
 id TEXT PRIMARY KEY, conversation_id TEXT NOT NULL REFERENCES assistant_conversations(id) ON DELETE CASCADE,
 epoch INTEGER NOT NULL, scope_key TEXT NOT NULL, model_key TEXT NOT NULL,
 through_turn TEXT NOT NULL REFERENCES assistant_turns(id) ON DELETE CASCADE,
 summary TEXT NOT NULL, tokens_before INTEGER NOT NULL,
 usage_json TEXT NOT NULL CHECK(json_valid(usage_json)), created_at TEXT NOT NULL
);
CREATE INDEX assistant_compaction_lookup ON assistant_compactions(conversation_id,epoch,scope_key,model_key,created_at);
PRAGMA user_version=12;
COMMIT;
