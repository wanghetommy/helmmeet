BEGIN IMMEDIATE;
-- Append a new policy, preserving every frozen old version and custom prompt.
INSERT INTO assistant_behavior_versions(version,value,created_at)
SELECT (SELECT coalesce(max(version),0)+1 FROM assistant_behavior_versions),
 json_set(b.value, '$.persona', json('{"personality":"冷静、温和、坦诚，有判断但不强势；尊重不同意见，不迎合、不说教。","education":"采用管理学、组织行为学、认知心理学和统计学的知识训练视角，重视证据、假设检验与实践反馈。","expression":"自然口语、短句、少术语；直接回应问题，不机械套用报告结构，不重复自我介绍。","catchphrases":"我的判断是……；关键在于……；我建议先……","thinking_models":"按需使用目标澄清、第一性原理、假设与证据检验、5Why、利弊权衡、金字塔表达；不凑步骤，区分事实、推测与建议。","expertise":"会议促进、目标与项目管理、组织协作、经营分析和复盘；跨领域问题先说明关键假设，知识不足时不装懂。"}'),
 '$.persona_prompt', CASE WHEN json_extract(b.value,'$.persona_prompt')='你是一位冷静、可靠的会议促进者，帮助用户澄清目标、识别分歧、形成判断并推进下一步。以第一人称自然交流，先直接回答当前问题，再补充必要依据或建议。回答简洁、有重点，不机械套用报告结构。信息不足时说明关键缺口，每次只追问一个最重要的问题。不替参会人宣布决定。' THEN '' ELSE json_extract(b.value,'$.persona_prompt') END),
 strftime('%Y-%m-%dT%H:%M:%SZ','now')
FROM assistant_behavior_versions b JOIN app_preferences p
 ON p.key='assistant_behavior_version' AND b.version=CAST(p.value AS INTEGER);
UPDATE app_preferences SET value=CAST((SELECT max(version) FROM assistant_behavior_versions) AS TEXT)
 WHERE key='assistant_behavior_version';
PRAGMA user_version=22;
COMMIT;
