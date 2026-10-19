BEGIN IMMEDIATE;
CREATE TABLE audio_enhancement (
 meeting_id TEXT PRIMARY KEY REFERENCES meetings(id) ON DELETE CASCADE,
 mode TEXT NOT NULL DEFAULT 'off' CHECK(mode IN ('off','low','moderate')),
 engine TEXT NOT NULL DEFAULT 'webrtc-apm-2.1/ns16k-delay96-v1',
 generation INTEGER NOT NULL DEFAULT 1 CHECK(generation>0),
 state TEXT NOT NULL DEFAULT 'queued' CHECK(state IN ('queued','processing','complete','failed')),
 error TEXT
);
INSERT INTO audio_enhancement(meeting_id) SELECT id FROM meetings;
INSERT OR IGNORE INTO app_preferences(key,value) VALUES('audio_enhancement_default','off');
CREATE TABLE enhanced_chunks (
 chunk_id TEXT PRIMARY KEY REFERENCES audio_chunks(id) ON DELETE CASCADE,
 meeting_id TEXT NOT NULL REFERENCES audio_enhancement(meeting_id) ON DELETE CASCADE,
 relative_path TEXT NOT NULL UNIQUE,
 sample_count INTEGER NOT NULL CHECK(sample_count>0),
 engine TEXT NOT NULL,
 FOREIGN KEY(chunk_id,meeting_id) REFERENCES audio_chunks(id,meeting_id)
);
CREATE UNIQUE INDEX audio_chunks_identity ON audio_chunks(id,meeting_id);
CREATE TRIGGER enhancement_mode_frozen BEFORE UPDATE OF mode,engine ON audio_enhancement
 WHEN EXISTS(SELECT 1 FROM meetings WHERE id=OLD.meeting_id AND (state!='ready' OR wall_start IS NOT NULL))
 BEGIN SELECT RAISE(ABORT,'audio enhancement frozen after recording starts'); END;
CREATE TRIGGER enhanced_source_guard BEFORE INSERT ON enhanced_chunks
 WHEN NOT EXISTS(SELECT 1 FROM audio_chunks c JOIN audio_enhancement e ON e.meeting_id=c.meeting_id WHERE c.id=NEW.chunk_id AND c.meeting_id=NEW.meeting_id AND c.source='microphone' AND c.status='sealed' AND c.sample_count=NEW.sample_count AND e.mode!='off' AND e.engine=NEW.engine)
 BEGIN SELECT RAISE(ABORT,'invalid enhanced audio source'); END;
PRAGMA user_version=8;
COMMIT;
