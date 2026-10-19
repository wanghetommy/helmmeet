BEGIN IMMEDIATE;
ALTER TABLE meetings ADD COLUMN parent_meeting_id TEXT REFERENCES meetings(id) ON DELETE SET NULL;
ALTER TABLE meetings ADD COLUMN analysis_choice TEXT NOT NULL DEFAULT 'legacy' CHECK(analysis_choice IN ('legacy','default','enabled','disabled'));
INSERT OR IGNORE INTO app_preferences VALUES ('analysis_paused','false'),('default_source_mask','1'),('default_input_device','0');
CREATE TABLE meeting_end_receipts (
 meeting_id TEXT NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
 epoch_id TEXT NOT NULL,
 expected_version INTEGER NOT NULL,
 created_at TEXT NOT NULL,
 PRIMARY KEY(meeting_id,epoch_id,expected_version)
);
CREATE TABLE knowledge_sets (
 id TEXT PRIMARY KEY,
 name TEXT NOT NULL,
 kind TEXT NOT NULL CHECK(kind IN ('documents','terms')),
 current_version INTEGER NOT NULL CHECK(current_version>0),
 archived INTEGER NOT NULL DEFAULT 0 CHECK(archived IN (0,1)),
 created_at TEXT NOT NULL
);
CREATE TABLE knowledge_versions (
 set_id TEXT NOT NULL REFERENCES knowledge_sets(id),
 version INTEGER NOT NULL CHECK(version>0),
 name TEXT NOT NULL,
 payload_json TEXT NOT NULL CHECK(json_valid(payload_json)),
 created_at TEXT NOT NULL,
 PRIMARY KEY(set_id,version)
);
CREATE TRIGGER knowledge_version_immutable BEFORE UPDATE ON knowledge_versions BEGIN SELECT RAISE(ABORT,'knowledge versions are immutable'); END;
CREATE TRIGGER knowledge_version_retained BEFORE DELETE ON knowledge_versions BEGIN SELECT RAISE(ABORT,'knowledge snapshots must be retained'); END;
CREATE TABLE meeting_knowledge_bindings (
 meeting_id TEXT NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
 set_id TEXT NOT NULL,
 version INTEGER NOT NULL,
 PRIMARY KEY(meeting_id,set_id),
 FOREIGN KEY(set_id,version) REFERENCES knowledge_versions(set_id,version)
);
CREATE TABLE knowledge_chunks (
 id INTEGER PRIMARY KEY,
 set_id TEXT NOT NULL,
 version INTEGER NOT NULL,
 ordinal INTEGER NOT NULL,
 text TEXT NOT NULL,
 UNIQUE(set_id,version,ordinal),
 FOREIGN KEY(set_id,version) REFERENCES knowledge_versions(set_id,version)
);
CREATE VIRTUAL TABLE knowledge_search USING fts5(text,content='knowledge_chunks',content_rowid='id',tokenize='trigram');
CREATE TRIGGER knowledge_search_insert AFTER INSERT ON knowledge_chunks BEGIN INSERT INTO knowledge_search(rowid,text) VALUES(new.id,new.text); END;
CREATE TABLE normalization_events (
 id TEXT PRIMARY KEY,
 meeting_id TEXT NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
 segment_id TEXT NOT NULL,
 before_revision INTEGER NOT NULL,
 after_revision INTEGER NOT NULL,
 bindings_json TEXT NOT NULL CHECK(json_valid(bindings_json)),
 created_at TEXT NOT NULL,
 undone INTEGER NOT NULL DEFAULT 0 CHECK(undone IN(0,1)),
 FOREIGN KEY(segment_id,before_revision,meeting_id) REFERENCES transcript_revisions(segment_id,revision,meeting_id),
 FOREIGN KEY(segment_id,after_revision,meeting_id) REFERENCES transcript_revisions(segment_id,revision,meeting_id)
);
CREATE TABLE asr_lexicon_snapshots (
 segment_id TEXT PRIMARY KEY,
 meeting_id TEXT NOT NULL,
 context_json TEXT NOT NULL CHECK(json_valid(context_json)),
 FOREIGN KEY(segment_id,meeting_id) REFERENCES transcript_segments(id,meeting_id) ON DELETE CASCADE
);
PRAGMA user_version=10;
COMMIT;
