BEGIN IMMEDIATE;
CREATE TABLE assistant_conversations (
 id TEXT PRIMARY KEY,
 meeting_id TEXT UNIQUE REFERENCES meetings(id) ON DELETE CASCADE,
 title TEXT NOT NULL,
 epoch INTEGER NOT NULL DEFAULT 0,
 created_at TEXT NOT NULL
);
CREATE TABLE assistant_turns (
 id TEXT PRIMARY KEY,
 conversation_id TEXT NOT NULL REFERENCES assistant_conversations(id) ON DELETE CASCADE,
 epoch INTEGER NOT NULL,
 question TEXT NOT NULL,
 source TEXT NOT NULL CHECK(source IN ('text','voice')),
 utterance_id TEXT UNIQUE,
 state TEXT NOT NULL CHECK(state IN ('pending','running','succeeded','failed','cancelled')),
 response_json TEXT CHECK(response_json IS NULL OR json_valid(response_json)),
 error TEXT,
 model_json TEXT CHECK(model_json IS NULL OR json_valid(model_json)),
 snapshot_json TEXT CHECK(snapshot_json IS NULL OR json_valid(snapshot_json)),
 legacy_job_id TEXT UNIQUE REFERENCES analysis_jobs(id) ON DELETE SET NULL,
 created_at TEXT NOT NULL
);
CREATE UNIQUE INDEX one_assistant_turn ON assistant_turns(conversation_id) WHERE state IN ('pending','running');
CREATE TABLE assistant_proposals (
 id TEXT PRIMARY KEY,
 turn_id TEXT NOT NULL REFERENCES assistant_turns(id) ON DELETE CASCADE,
 payload_json TEXT NOT NULL CHECK(json_valid(payload_json)),
 state TEXT NOT NULL DEFAULT 'pending' CHECK(state IN ('pending','accepted','dismissed')),
 result_json TEXT CHECK(result_json IS NULL OR json_valid(result_json))
);
CREATE TABLE assistant_playback_events (
 id TEXT PRIMARY KEY, meeting_id TEXT NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
 turn_id TEXT REFERENCES assistant_turns(id) ON DELETE CASCADE,
 text TEXT NOT NULL, start_host_ns INTEGER NOT NULL, end_host_ns INTEGER, start_ms INTEGER NOT NULL DEFAULT 0, end_ms INTEGER
);
INSERT INTO assistant_conversations(id,meeting_id,title,created_at)
 SELECT id,id,title,created_at FROM meetings WHERE EXISTS(SELECT 1 FROM conversation_turns c WHERE c.meeting_id=meetings.id);
INSERT INTO assistant_turns(id,conversation_id,epoch,question,source,state,legacy_job_id,created_at)
 SELECT c.job_id,c.meeting_id,0,c.question,'text',CASE WHEN j.state='succeeded' THEN 'succeeded' WHEN j.state='cancelled' THEN 'cancelled' ELSE 'failed' END,c.job_id,c.created_at
 FROM conversation_turns c JOIN analysis_jobs j ON j.id=c.job_id JOIN meetings m ON m.id=c.meeting_id WHERE c.epoch=m.chat_epoch;
INSERT OR IGNORE INTO app_preferences VALUES ('assistant_settings','{"voice_enabled":false,"wake_word":"小舵同学","silence_ms":1200,"speak_answers":true,"collections":[]}');
PRAGMA user_version=11;
COMMIT;
