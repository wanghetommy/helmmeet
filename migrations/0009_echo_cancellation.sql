BEGIN IMMEDIATE;
ALTER TABLE audio_enhancement ADD COLUMN echo_cancellation INTEGER NOT NULL DEFAULT 0 CHECK(echo_cancellation IN (0,1));
CREATE TRIGGER echo_configuration_frozen BEFORE UPDATE OF echo_cancellation ON audio_enhancement
 WHEN EXISTS(SELECT 1 FROM meetings WHERE id=OLD.meeting_id AND (state!='ready' OR wall_start IS NOT NULL))
 BEGIN SELECT RAISE(ABORT,'AEC frozen after recording starts'); END;
DROP TRIGGER enhanced_source_guard;
CREATE TRIGGER enhanced_source_guard BEFORE INSERT ON enhanced_chunks
 WHEN NOT EXISTS(SELECT 1 FROM audio_chunks c JOIN audio_enhancement e ON e.meeting_id=c.meeting_id WHERE c.id=NEW.chunk_id AND c.meeting_id=NEW.meeting_id AND c.source='microphone' AND c.status='sealed' AND c.sample_count=NEW.sample_count AND (e.mode!='off' OR e.echo_cancellation=1) AND e.engine=NEW.engine)
 BEGIN SELECT RAISE(ABORT,'invalid enhanced audio source'); END;
CREATE UNIQUE INDEX recording_epoch_identity ON recording_epochs(id,meeting_id);
CREATE TABLE audio_chunk_timing (
 chunk_id TEXT PRIMARY KEY,
 meeting_id TEXT NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
 epoch_id TEXT NOT NULL REFERENCES recording_epochs(id) ON DELETE CASCADE,
 start_sample INTEGER NOT NULL CHECK(start_sample>=0),
 host_origin_ns INTEGER NOT NULL CHECK(host_origin_ns>=0),
 FOREIGN KEY(chunk_id,meeting_id) REFERENCES audio_chunks(id,meeting_id) ON DELETE CASCADE,
 FOREIGN KEY(epoch_id,meeting_id) REFERENCES recording_epochs(id,meeting_id) ON DELETE CASCADE
);
CREATE INDEX audio_timing_meeting ON audio_chunk_timing(meeting_id,start_sample);
CREATE TABLE audio_output_routes (
 id TEXT PRIMARY KEY,
 meeting_id TEXT NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
 epoch_id TEXT NOT NULL REFERENCES recording_epochs(id) ON DELETE CASCADE,
 start_sample INTEGER NOT NULL CHECK(start_sample>=0),
 end_sample INTEGER NOT NULL CHECK(end_sample>=start_sample),
 device INTEGER NOT NULL,
 transport INTEGER NOT NULL,
 sample_rate INTEGER NOT NULL,
 supported INTEGER NOT NULL CHECK(supported IN (0,1)),
 FOREIGN KEY(epoch_id,meeting_id) REFERENCES recording_epochs(id,meeting_id) ON DELETE CASCADE
);
CREATE INDEX audio_routes_meeting ON audio_output_routes(meeting_id,start_sample);
CREATE TABLE aec_intervals (
 chunk_id TEXT NOT NULL REFERENCES enhanced_chunks(chunk_id) ON DELETE CASCADE,
 start_sample INTEGER NOT NULL CHECK(start_sample>=0),
 end_sample INTEGER NOT NULL CHECK(end_sample>start_sample),
 state TEXT NOT NULL CHECK(state IN ('applied','warming','silent','bypass')),
 reason TEXT NOT NULL,
 delay_ms INTEGER,
 PRIMARY KEY(chunk_id,start_sample)
);
CREATE TRIGGER aec_interval_bounds BEFORE INSERT ON aec_intervals
 WHEN NOT EXISTS(SELECT 1 FROM audio_chunks c LEFT JOIN audio_chunk_timing t ON t.chunk_id=c.id WHERE c.id=NEW.chunk_id AND NEW.start_sample>=coalesce(t.start_sample,c.start_ms*16) AND NEW.end_sample<=coalesce(t.start_sample,c.start_ms*16)+c.sample_count)
 BEGIN SELECT RAISE(ABORT,'AEC interval outside source'); END;
PRAGMA user_version=9;
COMMIT;
