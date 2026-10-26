BEGIN IMMEDIATE;
CREATE TABLE template_guidance (
 template_id TEXT NOT NULL,
 version INTEGER NOT NULL,
 policy_json TEXT NOT NULL CHECK(json_valid(policy_json)),
 PRIMARY KEY(template_id,version),
 FOREIGN KEY(template_id,version) REFERENCES template_versions(template_id,version)
);
INSERT INTO template_guidance SELECT template_id,version,json_object('goal',CASE template_id WHEN 'weekly' THEN '对齐目标和进展，识别阻塞与依赖，明确下一步。' WHEN 'project' THEN '对齐目标和进展，识别阻塞与依赖，明确下一步。' WHEN 'review' THEN '评估方案是否满足目标与约束，明确取舍及验证条件。' WHEN 'decision' THEN '比较选项与判断标准，帮助参会人形成有依据的决定。' WHEN 'retro' THEN '依据实际结果识别差距与原因，形成可验证的改进。' WHEN 'brainstorm' THEN '围绕问题边界探索有差异的想法和待验证方向。' ELSE '澄清当前讨论目标，形成有依据的判断和下一步建议。' END,'criteria',instructions,'qa_rules',CASE template_id WHEN 'weekly' THEN '优先检查目标、进展、阻塞、依赖和下一步，不把进度汇报当成承诺。' WHEN 'project' THEN '优先检查目标、进展、阻塞、依赖和下一步，不把进度汇报当成承诺。' WHEN 'review' THEN '优先检查问题边界、约束、替代方案、关键取舍和验证条件，不把假设当结论。' WHEN 'decision' THEN '优先检查选项、判断标准、利弊、决策权限及待确认决定，不代替主持人决策。' WHEN 'retro' THEN '区分预期与实际、差距、原因证据及改进行动，不把归因猜测当成根因。' WHEN 'brainstorm' THEN '先澄清探索方向，提供少量有差异的想法，不急于收敛，不把想法当已采纳方案。' ELSE '直接回答当前问题，必要时澄清目的，不强行套用完整会议分析框架。' END) FROM template_versions;
CREATE TRIGGER template_guidance_immutable BEFORE UPDATE ON template_guidance BEGIN SELECT RAISE(ABORT,'template guidance is immutable'); END;
CREATE TRIGGER template_guidance_retained BEFORE DELETE ON template_guidance BEGIN SELECT RAISE(ABORT,'template guidance must be retained'); END;
INSERT INTO assistant_behavior_versions(version,value,created_at)
SELECT (SELECT coalesce(max(version),0)+1 FROM assistant_behavior_versions),
 json_set(b.value,'$.persona_prompt',
 '你是一位帮助讨论形成清晰判断、推进下一步的会议伙伴。先理解当前问题，再直接回应。结合本场信息和必要的通用知识，区分事实、推测与建议。发现关键缺口时，优先澄清最影响判断的问题；出现分歧时，帮助明确判断标准；需要收敛时，整理已确认内容、未决问题和下一步建议。不替参会人宣布决定。'
 || CASE WHEN length(trim(coalesce(json_extract(b.value,'$.persona_prompt'),'')))>0 THEN char(10)||char(10)||'原有自定义要求：'||json_extract(b.value,'$.persona_prompt') ELSE '' END),
 strftime('%Y-%m-%dT%H:%M:%SZ','now')
FROM assistant_behavior_versions b JOIN app_preferences p ON p.key='assistant_behavior_version' AND b.version=CAST(p.value AS INTEGER);
UPDATE app_preferences SET value=CAST((SELECT max(version) FROM assistant_behavior_versions) AS TEXT) WHERE key='assistant_behavior_version';
PRAGMA user_version=24;
COMMIT;
