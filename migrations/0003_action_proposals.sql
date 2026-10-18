BEGIN IMMEDIATE;
CREATE TABLE action_proposals (
    id TEXT PRIMARY KEY,
    action_id TEXT NOT NULL REFERENCES action_items(id) ON DELETE CASCADE,
    artifact_id TEXT NOT NULL REFERENCES artifacts(id) ON DELETE CASCADE,
    proposal_hash TEXT NOT NULL,
    proposal_json TEXT NOT NULL CHECK(json_valid(proposal_json)),
    state TEXT NOT NULL DEFAULT 'pending' CHECK(state IN ('pending','accepted','ignored')),
    UNIQUE(action_id,proposal_hash)
);
PRAGMA user_version = 3;
COMMIT;
