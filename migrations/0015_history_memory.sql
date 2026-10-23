BEGIN IMMEDIATE;
CREATE TABLE assistant_history_index (
 conversation_id TEXT NOT NULL REFERENCES assistant_conversations(id) ON DELETE CASCADE,
 epoch INTEGER NOT NULL,
 turn_id TEXT PRIMARY KEY REFERENCES assistant_turns(id) ON DELETE CASCADE,
 question TEXT NOT NULL,
 answer TEXT NOT NULL,
 search_text TEXT NOT NULL,
 created_at TEXT NOT NULL
);
CREATE INDEX assistant_history_index_lookup ON assistant_history_index(conversation_id,epoch,created_at);
CREATE TABLE assistant_history_checkpoints (
 conversation_id TEXT NOT NULL REFERENCES assistant_conversations(id) ON DELETE CASCADE,
 epoch INTEGER NOT NULL,
 model_key TEXT NOT NULL,
 through_turn TEXT REFERENCES assistant_turns(id) ON DELETE SET NULL,
 summary TEXT NOT NULL,
 pending_turns INTEGER NOT NULL DEFAULT 0,
 status TEXT NOT NULL DEFAULT 'pending' CHECK(status IN('pending','running','succeeded','failed')),
 error TEXT,
 updated_at TEXT NOT NULL,
 PRIMARY KEY(conversation_id,epoch,model_key)
);
CREATE INDEX assistant_history_checkpoint_lookup ON assistant_history_checkpoints(conversation_id,epoch,updated_at);
INSERT INTO assistant_history_index(conversation_id,epoch,turn_id,question,answer,search_text,created_at)
 SELECT conversation_id,epoch,id,question,coalesce(response_json,''),question||' '||coalesce(response_json,''),created_at
 FROM assistant_turns WHERE state='succeeded';
PRAGMA user_version=15;
COMMIT;
