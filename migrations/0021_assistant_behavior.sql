BEGIN IMMEDIATE;
CREATE TABLE assistant_behavior_versions (
 version INTEGER PRIMARY KEY,
 value TEXT NOT NULL CHECK(json_valid(value)),
 created_at TEXT NOT NULL
);
CREATE TRIGGER assistant_behavior_immutable_update BEFORE UPDATE ON assistant_behavior_versions
BEGIN SELECT RAISE(ABORT,'assistant behavior versions are immutable'); END;
CREATE TRIGGER assistant_behavior_immutable_delete BEFORE DELETE ON assistant_behavior_versions
BEGIN SELECT RAISE(ABORT,'assistant behavior versions are immutable'); END;
INSERT INTO assistant_behavior_versions VALUES (1,
 '{"persona_prompt":"你是一位冷静、可靠的会议促进者，帮助用户澄清目标、识别分歧、形成判断并推进下一步。以第一人称自然交流，先直接回答当前问题，再补充必要依据或建议。回答简洁、有重点，不机械套用报告结构。信息不足时说明关键缺口，每次只追问一个最重要的问题。不替参会人宣布决定。","simple_seconds":10,"medium_seconds":20,"complex_seconds":30,"conclusion_first":true,"one_question":true}',
 strftime('%Y-%m-%dT%H:%M:%SZ','now'));
INSERT INTO app_preferences VALUES ('assistant_behavior_version','1');
ALTER TABLE assistant_turns ADD COLUMN speech_interrupted INTEGER NOT NULL DEFAULT 0 CHECK(speech_interrupted IN(0,1));
PRAGMA user_version=21;
COMMIT;
