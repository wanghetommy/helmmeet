BEGIN IMMEDIATE;
-- Preserve frozen versions. Merge custom text without truncating or duplicating it.
WITH selected AS (
 SELECT b.value,
 coalesce(json_extract(b.value,'$.persona.personality'),'') AS personality,
 coalesce(json_extract(b.value,'$.persona.expression'),'') AS expression,
 coalesce(json_extract(b.value,'$.persona.catchphrases'),'') AS catchphrases,
 coalesce(json_extract(b.value,'$.persona.thinking_models'),'') AS thinking_models,
 coalesce(json_extract(b.value,'$.persona.expertise'),'') AS expertise,
 coalesce(json_extract(b.value,'$.persona.education'),'') AS education
 FROM assistant_behavior_versions b JOIN app_preferences p
 ON p.key='assistant_behavior_version' AND b.version=CAST(p.value AS INTEGER)
), normalized AS (
 SELECT *,
 CASE WHEN expression='自然口语、短句、少术语；直接回应问题，不机械套用报告结构，不重复自我介绍。' AND catchphrases='我的判断是……；关键在于……；我建议先……' THEN '自然口语、短句、少术语；直接回应问题，不机械套用报告结构，不重复自我介绍。

惯用语（按需使用）：我的判断是……；关键在于……；我建议先……。偶尔自然使用，每条回答至多一次；短答和严肃风险讨论优先省略，避免相邻两轮重复。' ELSE expression END AS style,
 CASE WHEN expertise='会议促进、目标与项目管理、组织协作、经营分析和复盘；跨领域问题先说明关键假设，知识不足时不装懂。' THEN '熟悉会议促进、目标与项目管理、组织协作、经营分析和复盘方法，能结合常见实践提供建议；区分通用经验与本场已确认事实，缺少信息时说明假设。' ELSE expertise END AS experience,
 CASE WHEN education='采用管理学、组织行为学、认知心理学和统计学的知识训练视角，重视证据、假设检验与实践反馈。' THEN '' ELSE education END AS custom_background
 FROM selected
)
INSERT INTO assistant_behavior_versions(version,value,created_at)
SELECT (SELECT coalesce(max(version),0)+1 FROM assistant_behavior_versions),
 json_set(value,'$.persona',json_object(
 'personality',personality,
 'expression',CASE WHEN length(trim(catchphrases))>0 AND instr(style,catchphrases)=0
   THEN style || CASE WHEN length(style)>0 THEN char(10)||char(10) ELSE '' END || '惯用语（按需使用）：' || catchphrases ELSE style END,
 'thinking_models',thinking_models,
 'expertise',CASE WHEN length(trim(custom_background))>0 AND instr(experience,custom_background)=0
   THEN experience || CASE WHEN length(experience)>0 THEN char(10)||char(10) ELSE '' END || '背景与经验：' || custom_background ELSE experience END
 )),strftime('%Y-%m-%dT%H:%M:%SZ','now')
FROM normalized;
UPDATE app_preferences SET value=CAST((SELECT max(version) FROM assistant_behavior_versions) AS TEXT)
 WHERE key='assistant_behavior_version';
PRAGMA user_version=23;
COMMIT;
