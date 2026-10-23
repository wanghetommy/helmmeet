BEGIN IMMEDIATE;
CREATE TABLE assistant_review_parts (
 cache_key TEXT PRIMARY KEY,
 meeting_id TEXT NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
 conversation_id TEXT NOT NULL REFERENCES assistant_conversations(id) ON DELETE CASCADE,
 content_json TEXT NOT NULL CHECK(json_valid(content_json)),
 created_at TEXT NOT NULL
);
CREATE INDEX assistant_review_parts_meeting ON assistant_review_parts(meeting_id);
CREATE TABLE assistant_review_progress (
 turn_id TEXT PRIMARY KEY REFERENCES assistant_turns(id) ON DELETE CASCADE,
 completed INTEGER NOT NULL DEFAULT 0, total INTEGER NOT NULL,
 model_calls INTEGER NOT NULL DEFAULT 0, estimated_calls INTEGER NOT NULL,
 through_ms INTEGER NOT NULL, phase TEXT NOT NULL DEFAULT 'queued'
);
PRAGMA user_version=14;
COMMIT;
