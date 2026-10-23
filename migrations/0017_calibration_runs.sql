BEGIN IMMEDIATE;

CREATE TABLE transcript_calibration_runs (
    id TEXT PRIMARY KEY,
    meeting_id TEXT NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
    profile_id TEXT,
    config_version INTEGER,
    mode TEXT NOT NULL CHECK (mode IN ('low_latency','standard','high_accuracy')),
    state TEXT NOT NULL CHECK (state IN ('queued','running','succeeded','failed','partial')),
    total_paragraphs INTEGER NOT NULL DEFAULT 0 CHECK (total_paragraphs >= 0),
    completed_paragraphs INTEGER NOT NULL DEFAULT 0 CHECK (completed_paragraphs >= 0),
    cursor INTEGER NOT NULL DEFAULT 0 CHECK (cursor >= 0),
    input_budget_bytes INTEGER NOT NULL DEFAULT 0 CHECK (input_budget_bytes >= 0),
    output_budget_tokens INTEGER NOT NULL DEFAULT 0 CHECK (output_budget_tokens >= 0),
    rolling_context_json TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(rolling_context_json)),
    error TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE INDEX transcript_calibration_runs_meeting ON transcript_calibration_runs(meeting_id,created_at);

ALTER TABLE transcript_calibrations ADD COLUMN run_id TEXT REFERENCES transcript_calibration_runs(id) ON DELETE SET NULL;
ALTER TABLE transcript_calibrations ADD COLUMN batch_index INTEGER NOT NULL DEFAULT 0 CHECK (batch_index >= 0);
ALTER TABLE transcript_calibrations ADD COLUMN rolling_context_json TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(rolling_context_json));
CREATE INDEX transcript_calibrations_run ON transcript_calibrations(run_id,batch_index);

PRAGMA user_version=17;
COMMIT;
