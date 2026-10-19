use crate::{
    config::ModelConfig,
    db::{err, id, now, Runtime},
    models,
};
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{collections::HashSet, sync::Arc, thread, time::Duration};

pub fn target(config: &ModelConfig) -> String {
    format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(config).unwrap_or_default())
    )
}
pub fn request(
    rt: &Runtime,
    config: &ModelConfig,
    messages: Value,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<String, String> {
    request_inner(rt, config, messages, check, true)
}
fn request_body(config: &ModelConfig, messages: &Value, json_output: bool) -> Value {
    let mut body = json!({"model":config.model,"messages":messages,"max_tokens":config.max_output_tokens,"stream":false,"temperature":0.1});
    if json_output {
        body["response_format"] = json!({"type":"json_object"});
    }
    // Scope provider-specific options to the actual official endpoint, not model name.
    if config
        .endpoint()
        .ok()
        .is_some_and(|(u, _)| u.host_str() == Some("api.deepseek.com"))
    {
        body["thinking"] = json!({"type":"disabled"});
    }
    body
}
fn format_unsupported(code: u16, body: &Value) -> bool {
    if !matches!(code, 400 | 422) {
        return false;
    }
    let text = body["error"]["message"]
        .as_str()
        .unwrap_or("")
        .to_ascii_lowercase();
    (text.contains("response_format") || text.contains("json_object"))
        && [
            "unsupported",
            "not support",
            "unknown",
            "not allowed",
            "unrecognized",
        ]
        .iter()
        .any(|s| text.contains(s))
}
fn completion(data: &Value) -> Result<String, String> {
    let choice = &data["choices"][0];
    match choice["finish_reason"].as_str() {
        Some("length") => {
            return Err(
                "模型输出被截断：已达到输出上限；请提高模型配置中的输出上限，或缩小分析范围后重试"
                    .into(),
            )
        }
        Some("content_filter") => return Err("模型服务拦截了本次输出，未生成可用结果".into()),
        Some("tool_calls" | "function_call") => {
            return Err("模型返回了工具调用，本任务需要正文结果".into())
        }
        Some("stop") | None => {}
        _ => return Err("模型服务未正常完成输出，请重试".into()),
    }
    let text = choice["message"]["content"]
        .as_str()
        .ok_or("服务未返回文本正文；推理内容不能作为会议结果")?;
    if text.trim().is_empty() {
        return Err("模型返回空正文，请重试或检查模型输出上限".into());
    }
    Ok(text.into())
}
fn request_inner(
    rt: &Runtime,
    config: &ModelConfig,
    messages: Value,
    check: &dyn Fn() -> Result<(), String>,
    mut json_output: bool,
) -> Result<String, String> {
    let (endpoint, _) = config.endpoint()?;
    // Conservative byte budget; an unavailable tokenizer must not imply unlimited context.
    if messages.to_string().len()
        > config
            .context_tokens
            .checked_sub(config.max_output_tokens)
            .ok_or("模型上下文预算无效")? as usize
    {
        return Err("输入超过此配置的保守上下文预算，请缩小范围、模板或会议信息".into());
    }
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(config.timeout_seconds.clamp(10, 180)))
        .build()
        .map_err(|_| "HTTP 初始化失败")?;
    for attempt in 0..3 {
        check()?;
        let key = models::secret(rt, config)?;
        check()?;
        let mut request =
            client
                .post(endpoint.clone())
                .json(&request_body(config, &messages, json_output));
        if let Some(key) = key {
            request = request.bearer_auth(key);
        }
        let response = tauri::async_runtime::block_on(async {
            let exchange = async {
                let response = request.send().await.map_err(|_| "模型服务连接失败或超时")?;
                let code = response.status().as_u16();
                let wait = response
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse::<u64>().ok());
                let data = if (200..300).contains(&code)
                    || (json_output && matches!(code, 400 | 422))
                {
                    let mut response = response;
                    let mut bytes = Vec::new();
                    while let Some(chunk) =
                        response.chunk().await.map_err(|_| "模型响应读取失败")?
                    {
                        if bytes.len() + chunk.len() > 4 * 1024 * 1024 {
                            return Err("模型响应超过大小限制");
                        }
                        bytes.extend_from_slice(&chunk);
                    }
                    Some(serde_json::from_slice::<Value>(&bytes).map_err(|_| "服务返回无效 JSON"))
                } else {
                    None
                };
                Ok::<_, &str>((code, wait, data))
            };
            let mut exchange = Box::pin(exchange);
            loop {
                // Dropping the HTTP future cancels connection/body work promptly.
                // Already transmitted bytes cannot be recalled from the provider.
                check()?;
                match tokio::time::timeout(Duration::from_millis(100), exchange.as_mut()).await {
                    Ok(result) => return Ok::<_, String>(result),
                    Err(_) => continue,
                }
            }
        })?;
        match response {
            Ok((code, _, Some(data))) if (200..300).contains(&code) => {
                let data = data.map_err(str::to_string)?;
                return completion(&data);
            }
            Ok((code, _, Some(Ok(body))))
                if json_output && format_unsupported(code, &body) && attempt < 2 =>
            {
                json_output = false;
                continue;
            }

            Ok((code, retry, _)) => {
                if (code == 429 || code >= 500)
                    && attempt < 2
                    && retry.is_none_or(|seconds| seconds <= 20)
                {
                    cancellable_wait(
                        Duration::from_secs(retry.unwrap_or(1 << attempt).min(20)),
                        check,
                    )?;
                    continue;
                }
                return Err(format!(
                    "模型服务返回 HTTP {code}，请检查模型、凭据或额度；retry_after_seconds={}",
                    retry.unwrap_or(0)
                ));
            }
            Err(_) if attempt < 2 => cancellable_wait(Duration::from_secs(1 << attempt), check)?,
            Err(e) => return Err(e.into()),
        }
    }
    Err("请求失败".into())
}
fn cancellable_wait(
    duration: Duration,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<(), String> {
    let until = std::time::Instant::now() + duration;
    while std::time::Instant::now() < until {
        check()?;
        thread::sleep(
            Duration::from_millis(100)
                .min(until.saturating_duration_since(std::time::Instant::now())),
        );
    }
    check()
}

use crate::meeting_prompts::RULES;

fn parse(text: &str) -> Result<Value, String> {
    let mut text = text.trim().trim_start_matches('\u{feff}').trim();
    if text.starts_with("<think>") {
        text = text
            .split_once("</think>")
            .ok_or("模型只返回了未完成的推理，未生成JSON正文")?
            .1
            .trim();
    }
    if let Ok(value) = serde_json::from_str::<Value>(text) {
        if value.is_object() {
            return Ok(value);
        }
        return Err("模型结果必须是JSON对象".into());
    }
    // Extract only the first complete object. Never salvage a nested fragment of truncated JSON.
    let start = text
        .find('{')
        .ok_or("模型未返回JSON对象，请检查此模型是否支持JSON输出")?;
    let (mut depth, mut quoted, mut escaped) = (0usize, false, false);
    for (offset, c) in text[start..].char_indices() {
        if quoted {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                quoted = false;
            }
            continue;
        }
        match c {
            '"' => quoted = true,
            '{' => depth += 1,
            '}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    let end = start + offset + 1;
                    if text[end..].contains('{') {
                        return Err("模型返回了多个JSON对象，无法确定完整结果".into());
                    }
                    return serde_json::from_str(&text[start..end]).map_err(|e| {
                        format!(
                            "模型JSON格式无效（第{}行，第{}列），请重试",
                            e.line(),
                            e.column()
                        )
                    });
                }
            }
            _ => {}
        }
    }
    Err("模型JSON未完整闭合，可能已截断；请检查输出上限后重试".into())
}

pub fn validate(value: &Value, known: &HashSet<(String, u64)>) -> Result<(), String> {
    for key in ["summary", "actions"] {
        let items = value[key].as_array().ok_or("模型缺少结果列表")?;
        if items.len() > 200 {
            return Err("模型结果条数超限".into());
        }
        for item in items {
            let field = if key == "actions" { "task" } else { "text" };
            let text = item[field].as_str().ok_or("模型文本字段无效")?;
            if text.is_empty() || text.len() > 12000 {
                return Err("模型文本长度无效".into());
            }
            if let Some(section) = item.get("section") {
                if !matches!(
                    section.as_str(),
                    Some("topic" | "outline" | "progress" | "open" | "minutes" | "answer")
                ) {
                    return Err("结果章节无效".into());
                }
            }
            let evidence = item["evidence"].as_array().ok_or("模型缺少引用")?;
            if key == "summary"
                && !matches!(
                    item["kind"].as_str(),
                    Some(
                        "discussion"
                            | "decision"
                            | "risk"
                            | "question"
                            | "suggestion"
                            | "unanswered"
                    )
                )
            {
                return Err("模型分类字段无效".into());
            }
            let nonfact = key == "summary"
                && (item["kind"] == "suggestion"
                    || (item["kind"] == "unanswered" && text == "会议中未明确"));
            if evidence.len() > 32 || (evidence.is_empty() && !nonfact) {
                return Err("模型事实缺少依据".into());
            }
            for reference in evidence {
                let pair = (
                    reference["segment_id"]
                        .as_str()
                        .ok_or("引用 ID 无效")?
                        .to_string(),
                    reference["revision"].as_u64().ok_or("引用修订无效")?,
                );
                if !known.contains(&pair) {
                    return Err("模型引用了输入范围以外的原文".into());
                }
            }
            if key == "actions" {
                for field in ["owner_text", "due_text"] {
                    if !item[field].is_null() && !item[field].is_string() {
                        return Err("待办字段类型无效".into());
                    }
                }
            }
        }
    }
    Ok(())
}
fn sections_valid(value: &Value) -> bool {
    value["summary"]
        .as_array()
        .is_some_and(|items| items.iter().all(|i| i["section"].is_string()))
}
fn checked_request(
    rt: &Runtime,
    check: &dyn Fn() -> Result<(), String>,
    config: &ModelConfig,
    input: Value,
    known: &HashSet<(String, u64)>,
) -> Result<Value, String> {
    let messages =
        json!([{"role":"system","content":RULES},{"role":"user","content":input.to_string()}]);
    let output = request(rt, config, messages.clone(), check)?;
    let first = parse(&output).and_then(|v| {
        validate(&v, known)?;
        if !sections_valid(&v) {
            return Err("模型缺少有效结果章节".into());
        }
        Ok(v)
    });
    if let Ok(value) = first {
        return Ok(value);
    }
    let reason = first.unwrap_err();
    check()?;
    // Regenerate from the original evidence instead of doubling context with a malformed reply.
    let mut repair = messages;
    repair.as_array_mut().unwrap().push(json!({"role":"user","content":format!("上次结果未通过校验：{reason}。请依据同一输入重新生成一个完整JSON对象，必须含summary和actions数组。summary每项须有text、kind、section、evidence，引用只能来自输入。没有依据不增加事实，缺失的行动负责人/日期用null。不要Markdown或说明文字。") }));
    let value = parse(&request(rt, config, repair, check)?)
        .map_err(|e| format!("重新生成后仍未得到有效结构：{e}"))?;
    validate(&value, known)?;
    if !sections_valid(&value) {
        return Err("模型缺少有效结果章节".into());
    }
    Ok(value)
}
pub fn test_structure(
    rt: &Runtime,
    config: &ModelConfig,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<(), String> {
    let known = HashSet::from([("connection-test".into(), 1)]);
    let result = checked_request(
        rt,
        check,
        config,
        json!({"kind":"stage","question":"固定结构化连接测试：输出至少一条测试目标的summary，引用connection-test修订1。没有行动事项。","transcript":[{"segment_id":"connection-test","revision":1,"text":"本次任务为连接测试，目标是验证服务的JSON输出，不含真实会议内容。"}]}),
        &known,
    )?;
    if !result["summary"].as_array().is_some_and(|items| {
        items
            .iter()
            .any(|s| s["evidence"].as_array().is_some_and(|r| !r.is_empty()))
    }) {
        return Err(
            "连接已建立，但模型没有返回引用合成原文的有效结果，请检查此模型的结构化输出能力".into(),
        );
    }
    Ok(())
}
pub fn submit(
    rt: Arc<Runtime>,
    meeting: &str,
    kind: &str,
    question: &str,
    scope: &str,
) -> Result<Value, String> {
    submit_inner(rt, meeting, kind, question, scope, None)
}
pub fn submit_auto(
    rt: Arc<Runtime>,
    meeting: &str,
    schedule_version: u64,
) -> Result<Value, String> {
    submit_inner(rt, meeting, "stage", "", "all", Some(schedule_version))
}
fn submit_inner(
    rt: Arc<Runtime>,
    meeting: &str,
    kind: &str,
    question: &str,
    scope: &str,
    automatic: Option<u64>,
) -> Result<Value, String> {
    rt.ensure_running()?;
    rt.check_id(meeting)?;
    if crate::product::globally_paused(&rt)? {
        return Err("全局分析已暂停，请在设置中恢复".into());
    }
    if !matches!(kind, "stage" | "minutes" | "answer") {
        return Err("分析类型无效".into());
    }
    if question.len() > 12000 || (kind == "answer" && question.trim().is_empty()) {
        return Err("请输入不超过12000字节的问题".into());
    }
    if !matches!(scope, "" | "all" | "recent") && !scope.starts_with("selection:") {
        return Err("问答范围无效".into());
    }
    let config = models::selected(&rt, meeting)?;
    let (_, local) = config.endpoint()?;
    let detail = rt.detail(meeting)?;
    if detail["state"] == "ready" {
        return Err("会议尚未开始，没有可分析内容".into());
    }
    let mode = detail["analysis_mode"]
        .as_str()
        .unwrap_or("transcript_only");
    if mode == "transcript_only" || (mode == "local" && !local) {
        return Err("请先为本场会议启用并授权模型分析".into());
    }
    let consent: Option<String> = rt
        .db()?
        .query_row(
            "SELECT consent_target FROM meetings WHERE id=?",
            [meeting],
            |r| r.get(0),
        )
        .map_err(err)?;
    if consent.as_deref() != Some(&target(&config)) {
        return Err("服务或模型已变化，请重新授权本场分析".into());
    }
    if detail["segments"].as_array().is_none_or(|s| {
        s.iter()
            .all(|s| s["text"].as_str().unwrap_or("").trim().is_empty())
    }) {
        return Err("尚无已确认转写，请稍后重试".into());
    }
    let generation = detail["generation"].as_u64().unwrap_or(0);
    if kind == "stage" {
        let existing=rt.db()?.query_row("SELECT id FROM analysis_jobs WHERE meeting_id=? AND type='stage' AND state IN('pending','running') ORDER BY rowid LIMIT 1",[meeting],|r|r.get::<_,String>(0)).optional().map_err(err)?;
        if let Some(job) = existing {
            return Ok(json!({"job_id":job}));
        }
    }
    let key = id();
    let input = json!({"kind":kind,"question":question,"scope":scope});
    let job = id();
    let jobtype = if kind == "answer" {
        "qa"
    } else if kind == "minutes" {
        "final_map"
    } else {
        "stage"
    };
    {
        let mut db = rt.db()?;
        let tx = db.transaction().map_err(err)?;
        if let Err(e)=tx.execute("INSERT INTO analysis_jobs(id,meeting_id,idempotency_key,type,input_snapshot,generation,state) VALUES (?,?,?,?,?,?,'pending')",params![job,meeting,key,jobtype,input.to_string(),generation]) {
            if kind=="stage" { if let Ok(existing)=tx.query_row("SELECT id FROM analysis_jobs WHERE meeting_id=? AND type='stage' AND state IN('pending','running')",[meeting],|r|r.get::<_,String>(0)) {return Ok(json!({"job_id":existing}));} }
            return Err(err(e));
        }
        tx.execute(
            "INSERT INTO analysis_origins VALUES (?,?,?,?)",
            params![job, automatic.is_some(), automatic, input.to_string()],
        )
        .map_err(err)?;
        if kind == "answer" {
            tx.execute("INSERT INTO conversation_turns(job_id,meeting_id,epoch,question,scope,created_at) SELECT ?,id,chat_epoch,?,?,? FROM meetings WHERE id=?",params![job,question,scope,now(),meeting]).map_err(err)?;
        }
        tx.commit().map_err(err)?;
    }
    let mid = meeting.to_string();
    let task = job.clone();
    let output_kind = kind.to_string();
    let priority = if automatic.is_some() {
        2
    } else if kind == "answer" {
        0
    } else {
        1
    };
    rt.analysis_waiting
        .lock()
        .map_err(|_| "分析队列异常")?
        .push((task.clone(), priority));
    let mut handles = rt.analysis_threads.lock().map_err(|_| "分析线程锁异常")?;
    rt.ensure_running()?;
    let worker_rt = rt.clone();
    let handle = thread::spawn(move || {
        let rt = worker_rt;
        let result = (|| -> Result<(), String> {
            // Priority is checked while holding the queue lock. Only the selected head may take the global permit.
            let _gate = loop {
                check_job(&rt, &mid, generation, &task, automatic)?;
                if automatic.is_some()
                    && rt
                        .db()?
                        .query_row(
                            "SELECT state='paused' FROM meetings WHERE id=?",
                            [&mid],
                            |r| r.get::<_, bool>(0),
                        )
                        .map_err(err)?
                {
                    thread::sleep(Duration::from_millis(25));
                    continue;
                }
                let mut queue = rt.analysis_waiting.lock().map_err(|_| "分析队列异常")?;
                let next = queue
                    .iter()
                    .enumerate()
                    .min_by_key(|(i, (_, p))| (*p, *i))
                    .map(|(_, (id, _))| id);
                if next == Some(&task) {
                    if let Ok(permit) = rt.llm_gate.try_lock() {
                        queue.retain(|(id, _)| id != &task);
                        break permit;
                    }
                }
                drop(queue);
                thread::sleep(Duration::from_millis(25));
            };
            check_job(&rt, &mid, generation, &task, automatic)?;
            let detail = rt.detail(&mid)?;
            let config = models::selected(&rt, &mid)?;
            let scope = input["scope"].as_str().unwrap_or("all");
            let mut segments = detail["segments"].as_array().ok_or("缺少原文")?.clone();
            segments.retain(|s| !s["text"].as_str().unwrap_or("").trim().is_empty());
            if scope == "recent" {
                let cutoff = detail["duration_ms"]
                    .as_u64()
                    .unwrap_or(0)
                    .saturating_sub(600000);
                segments.retain(|s| s["end_ms"].as_u64().unwrap_or(0) > cutoff);
            } else if let Some(id) = scope.strip_prefix("selection:") {
                segments.retain(|s| s["id"].as_str() == Some(id));
            }
            let context_allowed = detail["context_consent"] == true;
            let previous = if output_kind == "stage" && context_allowed {
                detail["artifacts"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .find(|a| {
                        a["type"] == "stage"
                            && a["status"] != "stale"
                            && a["content"]["manual_text"].is_null()
                    })
                    .cloned()
            } else {
                None
            };
            let mut new_manifest = segments
                .iter()
                .map(|s| json!({"segment_id":s["id"],"revision":s["revision"]}))
                .collect::<Vec<_>>();
            if let Some(previous) = previous.as_ref().filter(|_| output_kind == "stage") {
                let pending = crate::workspace::pending(&rt, &mid)?;
                new_manifest
                    .retain(|s| pending.contains_key(s["segment_id"].as_str().unwrap_or("")));
                let old_refs = previous["content"]["summary"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .flat_map(|s| s["evidence"].as_array().into_iter().flatten())
                    .filter_map(|r| r["segment_id"].as_str())
                    .collect::<HashSet<_>>();
                segments.retain(|s| {
                    pending.contains_key(s["id"].as_str().unwrap_or(""))
                        || old_refs.contains(s["id"].as_str().unwrap_or(""))
                });
            }
            if segments.is_empty() {
                return Err("尚无可分析原文".into());
            }
            let assistant_id = detail["assistant_id"]
                .as_str()
                .unwrap_or("general")
                .to_string();
            let instructions = detail["template"]["instructions"]
                .as_str()
                .unwrap_or("整理本场讨论、决定、风险与行动，引用原文")
                .to_string();
            let history = if output_kind == "answer" && context_allowed {
                let db = rt.db()?;
                let mut stmt=db.prepare("SELECT t.question,a.content_json FROM conversation_turns t JOIN meetings m ON m.id=t.meeting_id JOIN artifacts a ON a.id=t.artifact_id WHERE t.meeting_id=? AND t.epoch=m.chat_epoch ORDER BY t.rowid DESC LIMIT 8").map_err(err)?;
                let mut rows=stmt.query_map([&mid],|r|Ok(json!({"question":r.get::<_,String>(0)?,"answer":serde_json::from_str::<Value>(&r.get::<_,String>(1)?).unwrap_or(Value::Null)}))).map_err(err)?.collect::<Result<Vec<_>,_>>().map_err(err)?;
                rows.reverse();
                json!(rows)
            } else {
                json!([])
            };
            let retrieval_query = format!(
                "{} {}",
                input["question"].as_str().unwrap_or(""),
                segments
                    .iter()
                    .rev()
                    .take(3)
                    .filter_map(|s| s["text"].as_str())
                    .collect::<Vec<_>>()
                    .join(" ")
            );
            let knowledge = crate::knowledge::retrieve(&rt, &mid, &retrieval_query)?;
            let task_context = json!({"kind":output_kind,"template":instructions,"question":input["question"],"meeting_context":detail["context"],"existing_actions":detail["actions"],"previous_summary":previous.as_ref().map(|a|&a["content"]),"chat_history":history,"selected_background":knowledge});
            let manifest = segments
                .iter()
                .map(|s| json!({"segment_id":s["id"],"revision":s["revision"]}))
                .collect::<Vec<_>>();
            let snapshot = json!({"request":input,"manifest":manifest,"new_manifest":new_manifest,"template":detail["template"],"knowledge_manifest":knowledge["bindings"],"config":config,"generation":generation,"consent_version":detail["consent_version"],"previous_artifact_id":previous.as_ref().map(|a|&a["id"]),"through_ms":detail["duration_ms"]});
            let frozen_key = format!("{:x}", Sha256::digest(format!("{task}:{snapshot}")));
            rt.db()?.execute("UPDATE analysis_jobs SET idempotency_key=?,input_snapshot=?,state='running',attempts=attempts+1 WHERE id=?",params![frozen_key,snapshot.to_string(),task]).map_err(err)?;
            let known: HashSet<_> = segments
                .iter()
                .map(|s| {
                    (
                        s["id"].as_str().unwrap_or("").into(),
                        s["revision"].as_u64().unwrap_or(0),
                    )
                })
                .collect();
            let deadline = std::time::Instant::now()
                + Duration::from_secs(if automatic.is_some() { 180 } else { 900 });
            let check = || {
                if std::time::Instant::now() >= deadline {
                    return Err("本轮分析超过总时限，请缩小范围后重试".into());
                }
                check_job(&rt, &mid, generation, &task, automatic)
            };
            let overhead = RULES.len() + task_context.to_string().len() + 1024;
            let batch_limit = (config.context_tokens - config.max_output_tokens) as usize;
            let batch_limit = batch_limit.saturating_sub(overhead).min(18000);
            if batch_limit < 256 {
                return Err("模板和会议背景超过上下文预算，请精简或提高配置容量".into());
            }
            let mut batches = vec![];
            let mut current = vec![];
            let mut size = 0;
            for seg in &segments {
                let n = seg.to_string().len();
                if size + n > batch_limit && !current.is_empty() {
                    batches.push(std::mem::take(&mut current));
                    size = 0;
                }
                current.push(json!({"segment_id":seg["id"],"revision":seg["revision"],"text":seg["text"],"start_ms":seg["start_ms"]}));
                size += n;
            }
            if !current.is_empty() {
                batches.push(current);
            }
            let mut outputs = vec![];
            let mut groups = 0usize;
            let max_groups = if automatic.is_some() { 4 } else { 24 };
            for batch in batches {
                check()?;
                groups += 1;
                if groups > max_groups {
                    return Err("本轮分析超过分批预算，请手动缩小范围".into());
                }
                outputs.push(checked_request(
                    &rt,
                    &check,
                    &config,
                    {
                        let mut v = task_context.clone();
                        v["transcript"] = json!(batch);
                        v
                    },
                    &known,
                )?);
            }
            // Bounded fan-in reduction retains original evidence at every level.
            while outputs.len() > 1 {
                let mut reduced = vec![];
                for batch in outputs.chunks(2) {
                    check()?;
                    if batch.len() == 1 {
                        reduced.push(batch[0].clone());
                    } else {
                        groups += 1;
                        if groups > max_groups {
                            return Err("本轮分析超过归并预算，请手动缩小范围".into());
                        }
                        reduced.push(checked_request(&rt,&check,&config,json!({"kind":output_kind,"template":instructions,"question":input["question"],"instruction":"合并去重这些已提取结果，保留全部关键决定和未决问题，原始引用不可改写","parts":batch}),&known)?);
                    }
                }
                outputs = reduced;
            }
            let value = outputs.pop().ok_or("分析未产生内容")?;
            check()?;
            let mut db = rt.db()?;
            let tx = db.transaction().map_err(err)?;
            let (state, current): (String, u64) = tx
                .query_row(
                    "SELECT state,generation FROM meetings WHERE id=?",
                    [&mid],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .map_err(err)?;
            let active: bool = tx
                .query_row(
                    "SELECT state='running' FROM analysis_jobs WHERE id=?",
                    [&task],
                    |r| r.get(0),
                )
                .map_err(err)?;
            let schedule_valid = if let Some(v) = automatic {
                tx.query_row(
                    "SELECT enabled=1 AND version=? FROM summary_schedules WHERE meeting_id=?",
                    params![v, mid],
                    |r| r.get::<_, bool>(0),
                )
                .map_err(err)?
            } else {
                true
            };
            if !active || !schedule_valid || state == "deleting" || current != generation {
                return Err("输入或授权已变化，旧结果不写回".into());
            }
            let covered = detail["asr_processed_ms"].as_u64().unwrap_or(0);
            let duration = detail["duration_ms"].as_u64().unwrap_or(0);
            let gaps = detail["coverage"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|c| c["state"] == "missing" || c["state"] == "failed")
                .count();
            let incomplete = covered < duration || gaps > 0;
            let version:u32=tx.query_row("SELECT coalesce(max(version),0)+1 FROM artifacts WHERE meeting_id=? AND type=?",params![mid,output_kind],|r|r.get(0)).map_err(err)?;
            let artifact = id();
            let coverage = json!({"from_ms":0,"to_ms":duration,"processed_ms":covered,"incomplete":incomplete,"model":config.model,"profile_id":config.profile_id,"config_version":config.config_version,"profile_name":config.display_name,"base_url":config.base_url,"question":input["question"],"generated_at":now(),"assistant_id":assistant_id,"template_version":detail["template"]["version"],"manifest":manifest,"knowledge_manifest":knowledge["bindings"],"background_excerpts":knowledge["excerpts"],"incremental":previous.is_some()});
            tx.execute(
                "INSERT INTO artifacts VALUES (?,?,?,?,?,?,?, 'model')",
                params![
                    artifact,
                    mid,
                    output_kind,
                    version,
                    value.to_string(),
                    coverage.to_string(),
                    if incomplete { "incomplete" } else { "draft" }
                ],
            )
            .map_err(err)?;
            for (key, list) in [
                ("summary", &value["summary"]),
                ("actions", &value["actions"]),
            ] {
                for (index, item) in list.as_array().unwrap().iter().enumerate() {
                    for reference in item["evidence"].as_array().unwrap() {
                        tx.execute(
                            "INSERT OR IGNORE INTO artifact_evidence VALUES (?,?,?,?,?)",
                            params![
                                artifact,
                                format!("{key}/{index}"),
                                mid,
                                reference["segment_id"].as_str(),
                                reference["revision"].as_u64()
                            ],
                        )
                        .map_err(err)?;
                    }
                }
            }
            for action in value["actions"].as_array().unwrap() {
                let task = action["task"].as_str().unwrap();
                let found: Option<(String, u32)> = tx
                    .query_row(
                        "SELECT id,version FROM action_items WHERE meeting_id=? AND task=?",
                        params![mid, task],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .optional()
                    .map_err(err)?;
                if let Some((existing, _)) = &found {
                    let current: (Option<String>, Option<String>) = tx
                        .query_row(
                            "SELECT owner_text,due_text FROM action_items WHERE id=?",
                            [existing],
                            |r| Ok((r.get(0)?, r.get(1)?)),
                        )
                        .map_err(err)?;
                    if current.0.as_deref() != action["owner_text"].as_str()
                        || current.1.as_deref() != action["due_text"].as_str()
                    {
                        let hash = format!("{:x}", Sha256::digest(action.to_string()));
                        tx.execute("INSERT OR IGNORE INTO action_proposals(id,action_id,artifact_id,proposal_hash,proposal_json) VALUES (?,?,?,?,?)",params![id(),existing,artifact,hash,action.to_string()]).map_err(err)?;
                    }
                }
                if found.is_none() {
                    let aid = id();
                    tx.execute("INSERT INTO action_items(id,meeting_id,task,owner_text,due_text,confirmation_state) VALUES (?,?,?,?,?,'pending')",params![aid,mid,task,action["owner_text"].as_str(),action["due_text"].as_str()]).map_err(err)?;
                    tx.execute(
                        "INSERT INTO action_revisions VALUES (?,1,'model',?,?,NULL)",
                        params![aid, action.to_string(), action["evidence"].to_string()],
                    )
                    .map_err(err)?;
                }
            }
            if output_kind == "stage" && matches!(scope, "" | "all") {
                for r in &new_manifest {
                    tx.execute("INSERT INTO summary_coverage VALUES (?,?,?) ON CONFLICT(meeting_id,segment_id) DO UPDATE SET revision=excluded.revision",params![mid,r["segment_id"].as_str(),r["revision"].as_u64()]).map_err(err)?;
                }
                tx.execute(
                    "UPDATE summary_schedules SET failures=0,paused_reason=NULL WHERE meeting_id=?",
                    [&mid],
                )
                .map_err(err)?;
            }
            if output_kind == "answer" {
                tx.execute(
                    "UPDATE conversation_turns SET artifact_id=? WHERE job_id=?",
                    params![artifact, task],
                )
                .map_err(err)?;
            }
            tx.execute(
                "UPDATE analysis_jobs SET state='succeeded' WHERE id=?",
                [&task],
            )
            .map_err(err)?;
            tx.commit().map_err(err)?;
            Ok(())
        })();
        if let Ok(mut queue) = rt.analysis_waiting.lock() {
            queue.retain(|(id, _)| id != &task);
        }
        if let Err(e) = result {
            if let Ok(db) = rt.db() {
                let invalid = db
                    .query_row(
                        "SELECT generation!=? OR state='deleting' FROM meetings WHERE id=?",
                        params![generation, mid],
                        |r| r.get::<_, bool>(0),
                    )
                    .unwrap_or(true)
                    || rt.shutting_down.load(std::sync::atomic::Ordering::Acquire);
                let changed=db.execute("UPDATE analysis_jobs SET state=?,error_code=? WHERE id=? AND state IN('pending','running')",params![if invalid {"cancelled"}else{"failed"},e,task]).unwrap_or(0);
                let changed = if invalid { 0 } else { changed };
                if changed > 0 && automatic.is_some() {
                    let permanent = e.contains("HTTP 400")
                        || e.contains("HTTP 422")
                        || e.contains("HTTP 401")
                        || e.contains("HTTP 403")
                        || e.contains("HTTP 404")
                        || (!e.contains("HTTP") && (e.contains("凭据") || e.contains("授权")));
                    let retry = e
                        .split("retry_after_seconds=")
                        .nth(1)
                        .and_then(|n| n.parse::<u64>().ok())
                        .unwrap_or(0);
                    let retry_at = chrono::Utc::now()
                        .timestamp()
                        .saturating_add(i64::try_from(retry).unwrap_or(i64::MAX));
                    let _ = db.execute(
                        "UPDATE summary_schedules SET retry_after_utc=? WHERE meeting_id=?",
                        params![retry_at, mid],
                    );
                    let _=db.execute("UPDATE summary_schedules SET failures=failures+1,paused_reason=CASE WHEN ? OR failures+1>=3 THEN ? ELSE NULL END WHERE meeting_id=?",params![permanent,e,mid]);
                }
            }
        }
    });
    // Reap finished tasks without retaining unbounded handles for long meetings.
    let mut i = 0;
    while i < handles.len() {
        if handles[i].is_finished() {
            let old = handles.swap_remove(i);
            let _ = old.join();
        } else {
            i += 1;
        }
    }
    handles.push(handle);
    Ok(json!({"job_id":job}))
}
fn check_job(
    rt: &Runtime,
    meeting: &str,
    generation: u64,
    job: &str,
    automatic: Option<u64>,
) -> Result<(), String> {
    check_current(rt, meeting, generation)?;
    let db = rt.db()?;
    let active: bool = db
        .query_row(
            "SELECT state IN('pending','running') FROM analysis_jobs WHERE id=?",
            [job],
            |r| r.get(0),
        )
        .map_err(err)?;
    if !active {
        return Err("任务已取消".into());
    }
    if let Some(version) = automatic {
        let valid: bool = db
            .query_row(
                "SELECT enabled=1 AND version=? FROM summary_schedules WHERE meeting_id=?",
                params![version, meeting],
                |r| r.get(0),
            )
            .map_err(err)?;
        if !valid {
            return Err("自动总结设置已变化".into());
        }
    }
    Ok(())
}

fn check_current(rt: &Runtime, meeting: &str, generation: u64) -> Result<(), String> {
    rt.ensure_running()?;
    if crate::product::globally_paused(rt)? {
        return Err("全局分析已暂停".into());
    }
    let (current, state): (u64, String) = rt
        .db()?
        .query_row(
            "SELECT generation,state FROM meetings WHERE id=?",
            [meeting],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(err)?;
    if current != generation || state == "deleting" {
        return Err("任务已取消或输入发生变化".into());
    }
    models::selected(rt, meeting)?;
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;

    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    fn receive(s: &mut TcpStream) -> Value {
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut bytes = vec![];
        let mut buf = [0u8; 4096];
        loop {
            let n = s.read(&mut buf).unwrap();
            assert!(n > 0);
            bytes.extend_from_slice(&buf[..n]);
            if let Some(end) = bytes.windows(4).position(|v| v == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&bytes[..end]).to_ascii_lowercase();
                let len = headers
                    .lines()
                    .find_map(|l| {
                        l.strip_prefix("content-length:")
                            .and_then(|v| v.trim().parse::<usize>().ok())
                    })
                    .unwrap();
                if bytes.len() >= end + 4 + len {
                    return serde_json::from_slice(&bytes[end + 4..end + 4 + len]).unwrap();
                }
            }
        }
    }
    fn respond(s: &mut TcpStream, code: u16, body: Value) {
        let body = body.to_string();
        write!(s,"HTTP/1.1 {code} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
    }
    fn reply(text: &str) -> Value {
        json!({"choices":[{"finish_reason":"stop","message":{"content":text}}]})
    }
    fn fixture(url: &str) -> (tempfile::TempDir, Arc<Runtime>, ModelConfig) {
        let root = tempfile::tempdir().unwrap();
        let rt = Runtime::open_with_vault(
            root.path().into(),
            Arc::new(crate::models::tests::MemoryVault::default()),
        )
        .unwrap();
        let p = models::save(
            &rt,
            models::ProfileInput {
                profile_id: None,
                expected_version: None,
                display_name: "合成结构化测试".into(),
                base_url: url.into(),
                model: "fixture".into(),
                context_tokens: 32768,
                max_output_tokens: 4096,
                timeout_seconds: 10,
                key_action: "clear".into(),
                api_key: String::new(),
            },
        )
        .unwrap();
        let config = models::load(&rt, p["profile_id"].as_str().unwrap(), 1).unwrap();
        (root, rt, config)
    }
    fn fact() -> Value {
        json!({"summary":[{"text":"合成结论中有 {括号} 与引号\"，保持原文依据","kind":"discussion","section":"topic","evidence":[{"segment_id":"s1","revision":1}]}],"actions":[]})
    }
    #[test]
    fn accepts_wrapped_complete_json_but_never_salvages_truncation_or_multiple_results() {
        let json = fact().to_string();
        for text in [
            json.clone(),
            format!("\u{feff}```JSON\n{json}\n```"),
            format!("结果如下：\n{json}\n已完成。"),
            format!("<think>合成推理占位</think>\n```json\n{json}\n```"),
        ] {
            assert_eq!(parse(&text).unwrap(), fact());
        }
        assert!(parse("{\"summary\":[{\"text\":\"nested but outer not closed\"}").is_err());
        assert!(parse(&format!("{json}\n{json}")).is_err());
        assert!(parse("<think>incomplete").is_err());
        assert!(parse("[]").is_err());
    }
    #[test]
    fn provider_options_are_scoped_and_truncation_is_not_treated_as_success() {
        let mut c = ModelConfig {
            base_url: "https://api.deepseek.com".into(),
            model: "deepseek-flash".into(),
            ..ModelConfig::default()
        };
        let b = request_body(&c, &json!([]), true);
        assert_eq!(b["response_format"]["type"], "json_object");
        assert_eq!(b["thinking"]["type"], "disabled");
        c.base_url = "https://elsewhere.example.test".into();
        assert!(request_body(&c, &json!([]), true)["thinking"].is_null());
        assert!(request_body(&c, &json!([]), false)["response_format"].is_null());
        let mut d = reply("{\"summary\":[],\"actions\":[]}");
        d["choices"][0]["finish_reason"] = json!("length");
        assert!(completion(&d).unwrap_err().contains("截断"));
        assert!(completion(
            &json!({"choices":[{"message":{"reasoning_content":"synthetic reasoning only"}}]})
        )
        .is_err());
    }
    #[test]
    fn structured_http_repair_reuses_original_input_without_echoing_malformed_output() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let (_root, rt, c) = fixture(&format!("http://{}", listener.local_addr().unwrap()));
        let server = thread::spawn(move || {
            let mut seen = vec![];
            for text in [
                "MALFORMED-REPLY".repeat(1200),
                format!("```json\n{}\n```", fact()),
            ] {
                let (mut s, _) = listener.accept().unwrap();
                seen.push(receive(&mut s));
                respond(&mut s, 200, reply(&text));
            }
            seen
        });
        let v=checked_request(&rt,&||Ok(()),&c,json!({"kind":"stage","transcript":[{"segment_id":"s1","revision":1,"text":"合成原文"}]}),&HashSet::from([("s1".into(),1)])).unwrap();
        assert_eq!(v, fact());
        let seen = server.join().unwrap();
        assert!(seen
            .iter()
            .all(|r| r["response_format"]["type"] == "json_object"));
        assert!(!seen[1].to_string().contains("MALFORMED-REPLY"));
        assert!(seen[1].to_string().contains("合成原文"));
    }
    #[test]
    fn compatibility_fallback_only_follows_explicit_unsupported_json_error() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let (_root, rt, c) = fixture(&format!("http://{}", listener.local_addr().unwrap()));
        let server = thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let first = receive(&mut s);
            respond(
                &mut s,
                400,
                json!({"error":{"message":"response_format json_object is not supported"}}),
            );
            let (mut s, _) = listener.accept().unwrap();
            let second = receive(&mut s);
            respond(&mut s, 200, reply(&fact().to_string()));
            (first, second)
        });
        request(
            &rt,
            &c,
            json!([{"role":"user","content":"synthetic JSON test"}]),
            &|| Ok(()),
        )
        .unwrap();
        let (a, b) = server.join().unwrap();
        assert_eq!(a["response_format"]["type"], "json_object");
        assert!(b["response_format"].is_null());
        assert_eq!(a["messages"], b["messages"]);
        assert!(!format_unsupported(
            400,
            &json!({"error":{"message":"Invalid API key"}})
        ));
        assert!(!format_unsupported(
            401,
            &json!({"error":{"message":"unsupported response_format"}})
        ));
    }
    #[test]
    fn both_analysis_types_persist_valid_results_after_a_rejected_attempt() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let (_root, rt, _c) = fixture(&format!("http://{}", listener.local_addr().unwrap()));
        let mid = rt.create("合成分析回归", "general").unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        {
            let db = rt.db().unwrap();
            db.execute(
                "UPDATE meetings SET state='completed',recording_duration_ms=1000 WHERE id=?",
                [&mid],
            )
            .unwrap();
            db.execute(
                "INSERT INTO transcript_segments VALUES ('s1',?,0,1000,'mixed',1,'final')",
                [&mid],
            )
            .unwrap();
            db.execute(
                "INSERT INTO transcript_revisions VALUES ('s1',1,?,'合成原文','asr','now')",
                [&mid],
            )
            .unwrap();
        }
        let server = thread::spawn(move || {
            for response in [
                "not JSON".to_string(),
                "still not JSON".into(),
                fact().to_string(),
                fact().to_string(),
            ] {
                let (mut s, _) = listener.accept().unwrap();
                let _ = receive(&mut s);
                respond(&mut s, 200, reply(&response));
            }
        });
        let wait = |job: &Value| {
            let until = std::time::Instant::now() + Duration::from_secs(5);
            loop {
                let state: String = rt
                    .db()
                    .unwrap()
                    .query_row(
                        "SELECT state FROM analysis_jobs WHERE id=?",
                        [job["job_id"].as_str().unwrap()],
                        |r| r.get(0),
                    )
                    .unwrap();
                if matches!(state.as_str(), "succeeded" | "failed") {
                    break state;
                }
                assert!(std::time::Instant::now() < until);
                thread::sleep(Duration::from_millis(10));
            }
        };
        let failed = submit(rt.clone(), &mid, "stage", "", "all").unwrap();
        assert_eq!(wait(&failed), "failed");
        assert!(rt.detail(&mid).unwrap()["artifacts"]
            .as_array()
            .unwrap()
            .is_empty());
        for kind in ["stage", "minutes"] {
            let job = submit(rt.clone(), &mid, kind, "", "all").unwrap();
            assert_eq!(wait(&job), "succeeded");
        }
        server.join().unwrap();
        let d = rt.detail(&mid).unwrap();
        assert_eq!(d["artifacts"].as_array().unwrap().len(), 2);
        assert_eq!(d["jobs"][0]["state"], "succeeded");
    }
    #[test]
    fn rejects_fabricated_and_empty_evidence() {
        let known = HashSet::from([("s1".into(), 1)]);
        assert!(validate(
            &json!({"summary":[{"text":"决定","evidence":[]}],"actions":[]}),
            &known
        )
        .is_err());
        assert!(validate(&json!({"summary":[{"text":"决定","evidence":[{"segment_id":"other","revision":1}]}],"actions":[]}),&known).is_err());
        assert!(validate(&json!({"summary":[{"text":"会议中未明确","kind":"unanswered","evidence":[]}],"actions":[]}),&known).is_ok());
    }
}
