BEGIN IMMEDIATE;
CREATE TABLE recording_sources (
    meeting_id TEXT NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
    source TEXT NOT NULL CHECK(source IN ('microphone','system')),
    state TEXT NOT NULL CHECK(state IN ('active','disabled','failed','stopped')),
    device INTEGER NOT NULL DEFAULT 0,
    gap_start_ms INTEGER,
    reason TEXT,
    PRIMARY KEY(meeting_id,source)
);
PRAGMA user_version = 6;
COMMIT;
