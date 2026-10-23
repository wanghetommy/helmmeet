BEGIN IMMEDIATE;

ALTER TABLE meetings ADD COLUMN calibration_enabled INTEGER NOT NULL DEFAULT 0;
ALTER TABLE meetings ADD COLUMN calibration_mode TEXT NOT NULL DEFAULT 'standard' CHECK (calibration_mode IN ('low_latency','standard','high_accuracy'));

CREATE TABLE transcript_calibrations (
    id TEXT PRIMARY KEY,
    meeting_id TEXT NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
    paragraph_id TEXT NOT NULL,
    source_manifest_json TEXT NOT NULL CHECK (json_valid(source_manifest_json)),
    source_revision INTEGER NOT NULL CHECK (source_revision > 0),
    raw_text TEXT NOT NULL,
    corrected_text TEXT,
    changes_json TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(changes_json)),
    previous_paragraph_id TEXT,
    dictionary_manifest_json TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(dictionary_manifest_json)),
    status TEXT NOT NULL CHECK (status IN ('pending','succeeded','needs_review','rejected','failed','stale')),
    error TEXT,
    model_json TEXT CHECK (model_json IS NULL OR json_valid(model_json)),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE(meeting_id, paragraph_id, source_revision)
);
CREATE INDEX transcript_calibrations_meeting_status ON transcript_calibrations(meeting_id,status,updated_at);

INSERT OR IGNORE INTO app_preferences VALUES (
    'calibration_defaults',
    '{"enabled":false,"mode":"standard","auto_apply_low_risk":false}'
);

PRAGMA user_version=16;
COMMIT;
