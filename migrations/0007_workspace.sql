BEGIN IMMEDIATE;
CREATE TABLE template_versions (
 template_id TEXT NOT NULL, version INTEGER NOT NULL, name TEXT NOT NULL, instructions TEXT NOT NULL,
 PRIMARY KEY(template_id,version)
);
INSERT INTO template_versions SELECT id,version,name,instructions FROM assistant_templates;
CREATE TRIGGER template_version_immutable BEFORE UPDATE ON template_versions BEGIN SELECT RAISE(ABORT,'template versions are immutable'); END;
ALTER TABLE meetings ADD COLUMN template_version INTEGER;
ALTER TABLE meetings ADD COLUMN template_source TEXT NOT NULL DEFAULT 'legacy_unverified';
UPDATE meetings SET template_version=(SELECT version FROM assistant_templates WHERE id=assistant_id);
ALTER TABLE meetings ADD COLUMN context_consent INTEGER NOT NULL DEFAULT 0 CHECK(context_consent IN (0,1));
CREATE TABLE summary_schedules (
 meeting_id TEXT PRIMARY KEY REFERENCES meetings(id) ON DELETE CASCADE,
 enabled INTEGER NOT NULL DEFAULT 0 CHECK(enabled IN(0,1)),
 mode TEXT NOT NULL DEFAULT 'realtime' CHECK(mode IN('realtime','minute','three_minutes','legacy_5','legacy_10')),
 version INTEGER NOT NULL DEFAULT 1,
 failures INTEGER NOT NULL DEFAULT 0,
 retry_after_utc INTEGER NOT NULL DEFAULT 0,
 paused_reason TEXT,
 tail_done INTEGER NOT NULL DEFAULT 1 CHECK(tail_done IN(0,1))
);
INSERT INTO summary_schedules(meeting_id,enabled,mode)
 SELECT id,CASE WHEN auto_summary_minutes>0 AND analysis_mode!='transcript_only' THEN 1 ELSE 0 END,
 CASE auto_summary_minutes WHEN 3 THEN 'three_minutes' WHEN 5 THEN 'legacy_5' WHEN 10 THEN 'legacy_10' ELSE 'realtime' END FROM meetings;
CREATE TABLE summary_coverage (
 meeting_id TEXT NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
 segment_id TEXT NOT NULL, revision INTEGER NOT NULL,
 PRIMARY KEY(meeting_id,segment_id),
 FOREIGN KEY(segment_id,revision,meeting_id) REFERENCES transcript_revisions(segment_id,revision,meeting_id) ON DELETE CASCADE
);
CREATE TABLE analysis_origins (
 job_id TEXT PRIMARY KEY REFERENCES analysis_jobs(id) ON DELETE CASCADE,
 automatic INTEGER NOT NULL CHECK(automatic IN(0,1)), schedule_version INTEGER,
 requested_json TEXT NOT NULL CHECK(json_valid(requested_json))
);
ALTER TABLE meetings ADD COLUMN chat_epoch INTEGER NOT NULL DEFAULT 0;
CREATE TABLE conversation_turns (
 job_id TEXT PRIMARY KEY REFERENCES analysis_jobs(id) ON DELETE CASCADE,
 meeting_id TEXT NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
 epoch INTEGER NOT NULL, question TEXT NOT NULL, scope TEXT NOT NULL,
 artifact_id TEXT REFERENCES artifacts(id) ON DELETE SET NULL,
 created_at TEXT NOT NULL
);
UPDATE analysis_jobs SET state='cancelled',error_code='UPGRADE_INTERRUPTED' WHERE state IN('pending','running');
CREATE UNIQUE INDEX one_active_summary ON analysis_jobs(meeting_id) WHERE type='stage' AND state IN('pending','running');
PRAGMA user_version=7;
COMMIT;
