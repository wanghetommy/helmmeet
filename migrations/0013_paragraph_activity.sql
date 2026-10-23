BEGIN IMMEDIATE;
CREATE UNIQUE INDEX recording_epochs_meeting_identity ON recording_epochs(meeting_id,id);
CREATE TABLE speech_activity_epochs (
 epoch_id TEXT PRIMARY KEY,
 meeting_id TEXT NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
 UNIQUE(meeting_id,epoch_id),
 FOREIGN KEY(meeting_id,epoch_id) REFERENCES recording_epochs(meeting_id,id) ON DELETE CASCADE
);
CREATE TABLE speech_pauses (
 meeting_id TEXT NOT NULL,
 epoch_id TEXT NOT NULL,
 start_ms INTEGER NOT NULL CHECK(start_ms>=0),
 end_ms INTEGER NOT NULL CHECK(end_ms>start_ms),
 PRIMARY KEY(meeting_id,epoch_id,start_ms),
 FOREIGN KEY(meeting_id,epoch_id) REFERENCES speech_activity_epochs(meeting_id,epoch_id) ON DELETE CASCADE
);
CREATE INDEX speech_pauses_meeting ON speech_pauses(meeting_id,start_ms);
CREATE TABLE model_token_observations (
 model_key TEXT PRIMARY KEY,
 ratio_milli INTEGER NOT NULL CHECK(ratio_milli>=1000 AND ratio_milli<=8000)
);
PRAGMA user_version=13;
COMMIT;
