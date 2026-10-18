-- Pre-release schema foundation. Enable foreign_keys on EVERY connection.
BEGIN IMMEDIATE;

CREATE TABLE meetings (
    id TEXT PRIMARY KEY,
    title TEXT NOT NULL,
    timezone TEXT NOT NULL,
    wall_start TEXT,
    recording_duration_ms INTEGER NOT NULL DEFAULT 0 CHECK (recording_duration_ms >= 0),
    state TEXT NOT NULL DEFAULT 'ready' CHECK (state IN
        ('ready','recording','paused','interrupted','finalizing','completed','needs_attention','deleting')),
    analysis_mode TEXT NOT NULL DEFAULT 'transcript_only' CHECK (analysis_mode IN ('transcript_only','local','remote')),
    generation INTEGER NOT NULL DEFAULT 1 CHECK (generation > 0),
    created_at TEXT NOT NULL
);
CREATE UNIQUE INDEX one_active_meeting ON meetings((1))
    WHERE state IN ('recording','paused','interrupted');

CREATE TABLE recording_epochs (
    id TEXT PRIMARY KEY,
    meeting_id TEXT NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
    recording_start_ms INTEGER NOT NULL CHECK (recording_start_ms >= 0),
    recording_end_ms INTEGER CHECK (recording_end_ms >= recording_start_ms),
    wall_start TEXT NOT NULL,
    wall_end TEXT,
    reason TEXT NOT NULL
);

CREATE TABLE audio_chunks (
    id TEXT PRIMARY KEY,
    meeting_id TEXT NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
    source TEXT NOT NULL CHECK (source IN ('microphone','system')),
    relative_path TEXT NOT NULL UNIQUE,
    start_ms INTEGER NOT NULL CHECK (start_ms >= 0),
    end_ms INTEGER CHECK (end_ms > start_ms),
    sample_count INTEGER NOT NULL DEFAULT 0 CHECK (sample_count >= 0),
    status TEXT NOT NULL CHECK (status IN ('writing','sealed','repair_needed','missing')),
    UNIQUE (meeting_id, source, start_ms)
);

CREATE TABLE coverage_intervals (
    id TEXT PRIMARY KEY,
    meeting_id TEXT NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
    kind TEXT NOT NULL CHECK (kind IN ('capture_microphone','capture_system','asr','analysis')),
    start_ms INTEGER NOT NULL CHECK (start_ms >= 0),
    end_ms INTEGER NOT NULL CHECK (end_ms > start_ms),
    state TEXT NOT NULL CHECK (state IN ('covered','missing','failed','pending')),
    reason TEXT,
    snapshot_id TEXT
);
CREATE INDEX coverage_by_meeting ON coverage_intervals(meeting_id, kind, start_ms);

CREATE TABLE transcript_segments (
    id TEXT PRIMARY KEY,
    meeting_id TEXT NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
    start_ms INTEGER NOT NULL CHECK (start_ms >= 0),
    end_ms INTEGER NOT NULL CHECK (end_ms > start_ms),
    source TEXT NOT NULL CHECK (source IN ('microphone','system','mixed')),
    current_revision INTEGER NOT NULL CHECK (current_revision > 0),
    state TEXT NOT NULL CHECK (state IN ('final','edited')),
    UNIQUE (id, meeting_id)
);
CREATE INDEX transcript_by_time ON transcript_segments(meeting_id, start_ms);

CREATE TABLE transcript_revisions (
    segment_id TEXT NOT NULL,
    revision INTEGER NOT NULL CHECK (revision > 0),
    meeting_id TEXT NOT NULL,
    text TEXT NOT NULL,
    origin TEXT NOT NULL CHECK (origin IN ('asr','human')),
    created_at TEXT NOT NULL,
    PRIMARY KEY (segment_id, revision),
    UNIQUE (segment_id, revision, meeting_id),
    FOREIGN KEY (segment_id, meeting_id) REFERENCES transcript_segments(id, meeting_id) ON DELETE CASCADE
);

CREATE TABLE model_configs (
    id TEXT NOT NULL,
    version INTEGER NOT NULL CHECK (version > 0),
    config_json TEXT NOT NULL CHECK (json_valid(config_json)),
    credential_ref TEXT,
    PRIMARY KEY (id, version)
);

CREATE TABLE meeting_configs (
    meeting_id TEXT NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
    version INTEGER NOT NULL CHECK (version > 0),
    assistant_snapshot TEXT NOT NULL CHECK (json_valid(assistant_snapshot)),
    model_snapshot TEXT NOT NULL CHECK (json_valid(model_snapshot)),
    permission_snapshot TEXT NOT NULL CHECK (json_valid(permission_snapshot)),
    PRIMARY KEY (meeting_id, version)
);

CREATE TABLE analysis_jobs (
    id TEXT PRIMARY KEY,
    meeting_id TEXT NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
    idempotency_key TEXT NOT NULL UNIQUE,
    type TEXT NOT NULL CHECK (type IN ('stage','qa','final_map','final_reduce')),
    input_snapshot TEXT NOT NULL CHECK (json_valid(input_snapshot)),
    generation INTEGER NOT NULL CHECK (generation > 0),
    state TEXT NOT NULL CHECK (state IN ('pending','running','succeeded','failed','cancelled','stale')),
    attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    error_code TEXT
);

CREATE TABLE artifacts (
    id TEXT PRIMARY KEY,
    meeting_id TEXT NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
    type TEXT NOT NULL CHECK (type IN ('stage','minutes','answer')),
    version INTEGER NOT NULL CHECK (version > 0),
    content_json TEXT NOT NULL CHECK (json_valid(content_json)),
    coverage_json TEXT NOT NULL CHECK (json_valid(coverage_json)),
    status TEXT NOT NULL CHECK (status IN ('draft','confirmed','incomplete','stale')),
    generated_by TEXT NOT NULL CHECK (generated_by IN ('model','human')),
    UNIQUE (id, meeting_id),
    UNIQUE (meeting_id, type, version)
);

CREATE TABLE artifact_evidence (
    artifact_id TEXT NOT NULL,
    item_key TEXT NOT NULL,
    meeting_id TEXT NOT NULL,
    segment_id TEXT NOT NULL,
    revision INTEGER NOT NULL,
    PRIMARY KEY (artifact_id, item_key, segment_id, revision),
    FOREIGN KEY (artifact_id, meeting_id) REFERENCES artifacts(id, meeting_id) ON DELETE CASCADE,
    FOREIGN KEY (segment_id, revision, meeting_id) REFERENCES transcript_revisions(segment_id, revision, meeting_id)
);

CREATE TABLE action_items (
    id TEXT PRIMARY KEY,
    meeting_id TEXT NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
    version INTEGER NOT NULL DEFAULT 1 CHECK (version > 0),
    task TEXT NOT NULL,
    owner_text TEXT,
    due_text TEXT,
    due_at TEXT,
    confirmation_state TEXT NOT NULL CHECK (confirmation_state IN ('pending','confirmed','ignored')),
    execution_state TEXT NOT NULL DEFAULT 'todo' CHECK (execution_state IN ('todo','doing','done')),
    locked_fields_json TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(locked_fields_json))
);

CREATE TABLE action_revisions (
    action_id TEXT NOT NULL REFERENCES action_items(id) ON DELETE CASCADE,
    version INTEGER NOT NULL CHECK (version > 0),
    origin TEXT NOT NULL CHECK (origin IN ('model','human')),
    changes_json TEXT NOT NULL CHECK (json_valid(changes_json)),
    evidence_json TEXT NOT NULL CHECK (json_valid(evidence_json)),
    accepted_at TEXT,
    PRIMARY KEY (action_id, version)
);

CREATE TABLE cleanup_queue (
    id TEXT PRIMARY KEY,
    meeting_id TEXT NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
    relative_path TEXT NOT NULL,
    attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    error_code TEXT
);

PRAGMA user_version = 1;
COMMIT;
