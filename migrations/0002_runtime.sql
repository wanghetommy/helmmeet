BEGIN IMMEDIATE;
ALTER TABLE meetings ADD COLUMN version INTEGER NOT NULL DEFAULT 1;
ALTER TABLE meetings ADD COLUMN assistant_id TEXT NOT NULL DEFAULT 'general';
ALTER TABLE meetings ADD COLUMN auto_summary_minutes INTEGER NOT NULL DEFAULT 5;
ALTER TABLE meetings ADD COLUMN consent_target TEXT;
ALTER TABLE meetings ADD COLUMN last_error TEXT;
CREATE TABLE user_notes (
    id TEXT PRIMARY KEY,
    meeting_id TEXT NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
    recording_time_ms INTEGER NOT NULL,
    text TEXT NOT NULL
);
CREATE TABLE assistant_templates (
    id TEXT PRIMARY KEY,
    version INTEGER NOT NULL,
    name TEXT NOT NULL,
    instructions TEXT NOT NULL
);
CREATE TABLE asr_chunks (
    meeting_id TEXT NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
    start_ms INTEGER NOT NULL,
    end_ms INTEGER NOT NULL,
    PRIMARY KEY(meeting_id,start_ms,end_ms)
);
ALTER TABLE meetings ADD COLUMN source_mask INTEGER NOT NULL DEFAULT 1;
PRAGMA user_version = 2;
COMMIT;
