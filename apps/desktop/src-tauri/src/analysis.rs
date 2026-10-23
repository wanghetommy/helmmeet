use crate::{
    config::ModelConfig,
    db::{err, id, now, Runtime},
    models,
};
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    thread,
    time::Duration,
};

pub fn target(config: &ModelConfig) -> String {
    format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(config).unwrap_or_default())
    )
}
pub const MAX_MODEL_OUTPUT_TOKENS: u32 = 131072;
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
    json_output: bool,
) -> Result<String, String> {
    completion(&request_with_capacity_retry(
        rt,
        config,
        messages,
        check,
        json_output,
        None,
        None,
    )?)
}
fn request_with_capacity_retry(
    rt: &Runtime,
    config: &ModelConfig,
    messages: Value,
    check: &dyn Fn() -> Result<(), String>,
    json_output: bool,
    tools: Option<&Value>,
    tool_choice: Option<&Value>,
) -> Result<Value, String> {
    let data = request_exchange(
        rt,
        config,
        messages.clone(),
        check,
        json_output,
        tools,
        tool_choice,
    )?;
    if data["choices"][0]["finish_reason"] == "length" {
        if let Some(next) = larger_output_config(config) {
            return request_exchange(rt, &next, messages, check, json_output, tools, tool_choice);
        }
    }
    Ok(data)
}
/// Exact serialized body size, including tools and provider-specific options.
pub fn agent_request_bytes(config: &ModelConfig, messages: &Value, tools: &Value) -> usize {
    let mut body = request_body(config, messages, false);
    if tools.as_array().is_some_and(|rows| !rows.is_empty()) {
        body["tools"] = tools.clone();
    }
    body.to_string().len()
}
pub fn request_agent(
    rt: &Runtime,
    config: &ModelConfig,
    messages: Value,
    tools: &Value,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<Value, String> {
    let data = request_with_capacity_retry(
        rt,
        config,
        messages,
        check,
        false,
        if tools.as_array().is_some_and(|t| t.is_empty()) {
            None
        } else {
            Some(tools)
        },
        None,
    )?;
    let choice = &data["choices"][0];
    match choice["finish_reason"].as_str() {
        Some("tool_calls") => {
            if choice["message"]["tool_calls"]
                .as_array()
                .is_none_or(|a| a.is_empty() || a.len() > 5)
            {
                return Err("模型工具调用格式无效".into());
            }
        }
        Some("stop") | None => {
            completion(&data)?;
        }
        _ => {
            completion(&data)?;
        }
    }
    let mut message = choice["message"].clone();
    message["_usage"] = data["usage"].clone();
    message["_finish_reason"] = choice["finish_reason"].clone();
    Ok(message)
}
pub fn token_budget(config: &ModelConfig) -> Result<usize, String> {
    meeting_core::budget::input_tokens(config.context_tokens, config.max_output_tokens)
        .ok_or_else(|| "模型上下文预算不足，请提高上下文窗口或减少输出预留".into())
}
pub fn input_budget(config: &ModelConfig) -> Result<usize, String> {
    meeting_core::budget::request_bytes(config.context_tokens, config.max_output_tokens)
        .ok_or_else(|| "模型上下文预算不足，请提高上下文窗口或减少输出预留".into())
}
pub fn soft_input_budget(config: &ModelConfig) -> Result<usize, String> {
    Ok(meeting_core::budget::soft_request_bytes(input_budget(
        config,
    )?))
}
fn larger_output_config(config: &ModelConfig) -> Option<ModelConfig> {
    if config.max_output_tokens >= MAX_MODEL_OUTPUT_TOKENS {
        return None;
    }
    let mut next = config.clone();
    next.max_output_tokens = config
        .max_output_tokens
        .saturating_mul(2)
        .max(config.max_output_tokens.saturating_add(1024))
        .min(MAX_MODEL_OUTPUT_TOKENS)
        .min(config.context_tokens.saturating_sub(1));
    (next.max_output_tokens > config.max_output_tokens).then_some(next)
}
pub(crate) fn estimated_tokens(rt: &Runtime, config: &ModelConfig, body: &Value) -> usize {
    let ratio = rt
        .db()
        .ok()
        .and_then(|db| {
            db.query_row(
                "SELECT ratio_milli FROM model_token_observations WHERE model_key=?",
                [target(config)],
                |r| r.get::<_, usize>(0),
            )
            .ok()
        })
        .unwrap_or(1000);
    (meeting_core::budget::estimate(&body.to_string()) * ratio).div_ceil(1000)
}
pub(crate) fn agent_fits(
    rt: &Runtime,
    config: &ModelConfig,
    messages: &Value,
    tools: &Value,
    bytes: usize,
    reserve: usize,
) -> Result<bool, String> {
    let mut body = request_body(config, messages, false);
    if tools.as_array().is_some_and(|a| !a.is_empty()) {
        body["tools"] = tools.clone();
    }
    Ok(body.to_string().len() <= bytes
        && estimated_tokens(rt, config, &body) <= token_budget(config)?.saturating_sub(reserve))
}
pub fn agent_remaining_bytes(
    rt: &Runtime,
    config: &ModelConfig,
    messages: &Value,
    tools: &Value,
) -> Result<usize, String> {
    let mut body = request_body(config, messages, false);
    if tools.as_array().is_some_and(|v| !v.is_empty()) {
        body["tools"] = tools.clone();
    }
    let current = agent_request_bytes(config, messages, tools);
    let byte_room = input_budget(config)?.saturating_sub(current);
    let byte_room = if current >= soft_input_budget(config)? {
        byte_room.min(2048)
    } else {
        byte_room
    };
    Ok(
        byte_room
            .min(token_budget(config)?.saturating_sub(estimated_tokens(rt, config, &body)) * 2),
    )
}
fn observe_usage(rt: &Runtime, config: &ModelConfig, body: &Value, data: &Value) {
    if let Some(actual) = data["usage"]["prompt_tokens"].as_u64() {
        let baseline = meeting_core::budget::estimate(&body.to_string()).max(1) as u64;
        let ratio = actual
            .saturating_mul(1000)
            .div_ceil(baseline)
            .clamp(1000, 8000);
        if let Ok(db) = rt.db() {
            let _=db.execute("INSERT INTO model_token_observations VALUES (?,?) ON CONFLICT(model_key) DO UPDATE SET ratio_milli=max(ratio_milli,excluded.ratio_milli)",params![target(config),ratio]);
        }
    }
}
fn context_error(code: u16, data: &Value) -> bool {
    if !matches!(code, 400 | 413 | 422) {
        return false;
    }
    let e = data["error"].to_string().to_lowercase();
    [
        "context_length_exceeded",
        "maximum context length",
        "context window",
        "too many tokens",
        "context length",
        "上下文长度",
    ]
    .iter()
    .any(|v| e.contains(v))
}
fn request_exchange(
    rt: &Runtime,
    config: &ModelConfig,
    messages: Value,
    check: &dyn Fn() -> Result<(), String>,
    mut json_output: bool,
    tools: Option<&Value>,
    tool_choice: Option<&Value>,
) -> Result<Value, String> {
    let (endpoint, _) = config.endpoint()?;
    let budget = input_budget(config)?;
    if messages.to_string().len() > budget {
        return Err("请求体超过当前模型输入安全预算，请压缩或减少本轮取证".into());
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
        let mut body = request_body(config, &messages, json_output);
        if let Some(tools) = tools {
            body["tools"] = tools.clone();
        }
        if let Some(tool_choice) = tool_choice {
            body["tool_choice"] = tool_choice.clone();
        }
        if body.to_string().len() > budget {
            return Err("工具与会话超过当前模型输入安全预算，请压缩或减少本轮取证".into());
        }
        if estimated_tokens(rt, config, &body) > token_budget(config)? {
            return Err(
                "模型上下文容量不足：估算输入tokens超过该配置余量，请压缩或减少本轮取证".into(),
            );
        }
        let mut request = client.post(endpoint.clone()).json(&body);
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
                let data = if (200..300).contains(&code) || matches!(code, 400 | 413 | 422) {
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
                observe_usage(rt, config, &body, &data);
                return Ok(data);
            }
            Ok((code, _, Some(Ok(data)))) if context_error(code, &data) => {
                return Err("模型上下文容量不足：服务拒绝了本轮输入长度".into());
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

/// Sends a fixed, synthetic tool call to verify that a model profile supports
/// the protocol required by the meeting assistant. No meeting data is sent.
pub fn test_tool_call(
    rt: &Runtime,
    config: &ModelConfig,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<(), String> {
    let tools = json!([{
        "type": "function",
        "function": {
            "name": "helmmeet_tool_probe",
            "description": "固定连接测试工具，不执行任何外部操作。",
            "parameters": {"type":"object","properties":{},"additionalProperties":false}
        }
    }]);
    let messages = json!([
        {"role":"system","content":"这是模型能力连接测试。只能调用指定工具，不要输出正文。"},
        {"role":"user","content":"请调用 helmmeet_tool_probe。"}
    ]);
    let choice = json!({"type":"function","function":{"name":"helmmeet_tool_probe"}});
    let data = request_exchange(
        rt,
        config,
        messages,
        check,
        false,
        Some(&tools),
        Some(&choice),
    )?;
    let item = &data["choices"][0];
    if item["finish_reason"] != "tool_calls" {
        return Err("模型未返回工具调用，当前配置不能完成助手取证".into());
    }
    let calls = item["message"]["tool_calls"]
        .as_array()
        .ok_or("模型工具调用格式无效")?;
    let valid = calls.iter().any(|call| {
        call["type"] == "function"
            && call["function"]["name"] == "helmmeet_tool_probe"
            && call["function"]["arguments"].as_str().is_some_and(|args| {
                serde_json::from_str::<Value>(args).is_ok_and(|v| v == json!({}))
            })
    });
    if !valid {
        return Err("模型返回了无法解析的工具调用参数".into());
    }
    Ok(())
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

pub(crate) fn parse(text: &str) -> Result<Value, String> {
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
        for (index, item) in items.iter().enumerate() {
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
                    return Err(format!("{key}[{index}].section无效：只能是topic、outline、progress、open、minutes、answer之一；中文章节名写入text，不能把task_type或kind填入section"));
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
                return Err(format!("summary[{index}].kind无效：只能是discussion、decision、risk、question、suggestion、unanswered之一；不能使用task_type、section或中文分类名"));
            }
            let nonfact = key == "summary"
                && (item["kind"] == "suggestion"
                    || (item["kind"] == "unanswered" && text == "会议中未明确"));
            if evidence.len() > 32 {
                return Err(format!(
                    "{key}[{index}]引用过多：{}条，上限32条；请按独立事实拆分条目并保留各自引用",
                    evidence.len()
                ));
            }
            if evidence.is_empty() && !nonfact {
                return Err(format!("{key}[{index}]事实或行动缺少依据：evidence为空；事实须补充原文引用，无依据则删除该条；未明确事项只能返回kind=unanswered、text=会议中未明确、evidence=[]"));
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
pub(crate) fn split_segment(seg: &Value, limit: usize) -> Vec<Value> {
    let text = seg["text"].as_str().unwrap_or("");
    let mut out = vec![];
    let mut start = 0usize;
    let mut offset = 0usize;
    let mut chunk = String::new();
    for c in text.chars() {
        if chunk.len() + c.len_utf8() > limit && !chunk.is_empty() {
            out.push(json!({"segment_id":seg["id"],"revision":seg["revision"],"text":chunk,"start_ms":seg["start_ms"],"fragment_start":start,"fragment_end":offset}));
            chunk = String::new();
            start = offset;
        }
        chunk.push(c);
        offset += 1;
    }
    if !chunk.is_empty() {
        out.push(json!({"segment_id":seg["id"],"revision":seg["revision"],"text":chunk,"start_ms":seg["start_ms"],"fragment_start":start,"fragment_end":offset}));
    }
    out
}
fn sections_valid(value: &Value) -> bool {
    value["summary"]
        .as_array()
        .is_some_and(|items| items.iter().all(|i| i["section"].is_string()))
}
// Per-call citation IDs reduce long UUID copying errors. The host owns the exact
// original (segment, revision) mapping; neither a batch nor a reducer can widen it.
#[derive(Debug)]
struct CitationScope {
    aliases: HashMap<String, (String, u64)>,
    known: HashSet<(String, u64)>,
}
fn reference_pair(v: &Value) -> Option<(String, u64)> {
    Some((v["segment_id"].as_str()?.into(), v["revision"].as_u64()?))
}
fn prepare_citations(
    input: &mut Value,
    known: &HashSet<(String, u64)>,
) -> Result<CitationScope, String> {
    let mut pairs = vec![];
    if let Some(rows) = input.get_mut("transcript").and_then(Value::as_array_mut) {
        rows.retain(|r| {
            r["assistant_playback_overlap"] != true
                && reference_pair(r).is_some_and(|p| known.contains(&p))
        });
        if rows.is_empty() {
            return Err("本轮没有可自动引用的原文；与小舵播报重叠的段落需先人工核对".into());
        }
        pairs.extend(rows.iter().filter_map(reference_pair));
    } else if let Some(parts) = input["parts"].as_array() {
        for part in parts {
            for key in ["summary", "actions"] {
                for item in part[key].as_array().into_iter().flatten() {
                    pairs.extend(
                        item["evidence"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(reference_pair)
                            .filter(|p| known.contains(p)),
                    );
                }
            }
        }
    }
    let mut unique = HashSet::new();
    pairs.retain(|p| unique.insert(p.clone()));
    let aliases: HashMap<_, _> = pairs
        .iter()
        .enumerate()
        .map(|(i, p)| (format!("ref_{:03}", i + 1), p.clone()))
        .collect();
    let reverse: HashMap<_, _> = aliases
        .iter()
        .map(|(alias, p)| (p.clone(), alias.clone()))
        .collect();
    fn rewrite(v: &mut Value, map: &HashMap<(String, u64), String>) {
        match v {
            Value::Array(rows) => {
                for row in rows {
                    rewrite(row, map);
                }
            }
            Value::Object(object) => {
                // Prior summaries/history are indexes, not an extra source scope.
                if let Some(refs) = object.get_mut("evidence").and_then(Value::as_array_mut) {
                    refs.retain(|r| reference_pair(r).is_some_and(|p| map.contains_key(&p)));
                }
                if let (Some(sid), Some(rev)) = (
                    object.get("segment_id").and_then(Value::as_str),
                    object.get("revision").and_then(Value::as_u64),
                ) {
                    if let Some(alias) = map.get(&(sid.into(), rev)) {
                        object.insert("segment_id".into(), json!(alias));
                    }
                }
                for value in object.values_mut() {
                    rewrite(value, map);
                }
            }
            _ => {}
        }
    }
    rewrite(input, &reverse);
    input["allowed_evidence"] = json!(pairs
        .iter()
        .map(|p| json!({"segment_id":reverse[p],"revision":p.1}))
        .collect::<Vec<_>>());
    Ok(CitationScope {
        aliases,
        known: unique,
    })
}
impl CitationScope {
    fn restore_and_validate(&self, mut value: Value) -> Result<Value, String> {
        for key in ["summary", "actions"] {
            for item in value
                .get_mut(key)
                .and_then(Value::as_array_mut)
                .into_iter()
                .flatten()
            {
                if key == "summary" {
                    if let Some(kind) = item["kind"]
                        .as_str()
                        .and_then(meeting_core::summary::canonical_kind)
                    {
                        item["kind"] = json!(kind);
                    }
                }
                // A model's unsupported "unanswered" wording is not a meeting
                // fact. Drop its unverified wording rather than rejecting the
                // whole minutes draft or promoting it to an uncited fact.
                if key == "summary"
                    && item["kind"] == "unanswered"
                    && item["evidence"].as_array().is_some_and(Vec::is_empty)
                    && item["text"]
                        .as_str()
                        .is_some_and(|t| !t.trim().is_empty() && t.len() <= 12000)
                {
                    item["text"] = json!("会议中未明确");
                }
                for r in item
                    .get_mut("evidence")
                    .and_then(Value::as_array_mut)
                    .into_iter()
                    .flatten()
                {
                    if let Some((sid, rev)) = r.as_str().and_then(|alias| self.aliases.get(alias)) {
                        // A short alias already freezes one exact revision. This
                        // expands known data; it never guesses an ID or revision.
                        *r = json!({"segment_id":sid,"revision":rev});
                    }
                    if let Some((sid, rev)) =
                        r["segment_id"].as_str().and_then(|s| self.aliases.get(s))
                    {
                        if r["revision"].as_u64() == Some(*rev) {
                            r["segment_id"] = json!(sid);
                        }
                    }
                }
            }
        }
        validate(&value, &self.known)?;
        if !sections_valid(&value) {
            return Err("模型缺少有效结果章节".into());
        }
        Ok(value)
    }
}
#[cfg(test)]
static MEETING_DIAGNOSTICS: std::sync::Mutex<Vec<Value>> = std::sync::Mutex::new(Vec::new());
#[cfg(test)]
fn diagnostic_shape(input: &Value, parsed: &Result<Value, String>, result: &Result<Value, String>) {
    if std::env::var("HELMMEET_ALLOW_MEETING_MODEL").as_deref() != Ok("1") {
        return;
    }
    let mut rows = Vec::new();
    if let Ok(value) = parsed {
        for key in ["summary", "actions"] {
            for (index, item) in value[key].as_array().into_iter().flatten().enumerate() {
                // Only known classifications are safe to log; arbitrary model
                // strings may contain private meeting content.
                let kind = item["kind"]
                    .as_str()
                    .filter(|k| {
                        matches!(
                            *k,
                            "discussion"
                                | "decision"
                                | "risk"
                                | "question"
                                | "suggestion"
                                | "unanswered"
                        )
                    })
                    .unwrap_or("other");
                let section = item["section"]
                    .as_str()
                    .filter(|s| {
                        matches!(
                            *s,
                            "topic" | "outline" | "progress" | "open" | "minutes" | "answer"
                        )
                    })
                    .unwrap_or("other");
                let refs = item["evidence"].as_array();
                rows.push(json!({
                    "list": key, "index": index, "kind": kind, "section": section,
                    "text_chars": item["text"].as_str().map(|s| s.chars().count()),
                    "canonical_unanswered": item["text"] == "会议中未明确",
                    "references": refs.map(Vec::len),
                    "reference_strings": refs.map(|r| r.iter().filter(|v| v.is_string()).count()),
                    "reference_objects_with_id": refs.map(|r| r.iter().filter(|v| v["segment_id"].is_string()).count())
                }));
            }
        }
    }
    MEETING_DIAGNOSTICS.lock().unwrap().push(json!({
        "phase": if input["transcript"].is_array() { "map" } else { "reduce" },
        "input_segments": input["transcript"].as_array().map(Vec::len),
        "allowed_references": input["allowed_evidence"].as_array().map(Vec::len),
        "items": rows, "validation_error": result.as_ref().err()
    }));
}
fn checked_request(
    rt: &Runtime,
    check: &dyn Fn() -> Result<(), String>,
    config: &ModelConfig,
    input: Value,
    known: &HashSet<(String, u64)>,
) -> Result<Value, String> {
    grounded_request(rt, check, config, input, known, &|| Ok(()))
}
pub(crate) fn grounded_request(
    rt: &Runtime,
    check: &dyn Fn() -> Result<(), String>,
    config: &ModelConfig,
    mut input: Value,
    known: &HashSet<(String, u64)>,
    before: &dyn Fn() -> Result<(), String>,
) -> Result<Value, String> {
    // Keep request task type distinct from summary item classification, including
    // legacy callers and reduce requests. Never reinterpret invalid output as fact.
    if let Some(object) = input.as_object_mut() {
        if let Some(kind) = object.remove("kind") {
            object.insert("task_type".into(), kind);
        }
    }
    input["output_contract"] = json!({
        "summary": {"type":"array","item":{
            "required":["text","kind","section","evidence"],
            "kind":{"enum":["discussion","decision","risk","question","suggestion","unanswered"]},
            "section":{"enum":["topic","outline","progress","open","minutes","answer"]}
        }},
        "actions":{"type":"array"}
    });
    let citations = prepare_citations(&mut input, known)?;
    let messages =
        json!([{"role":"system","content":RULES},{"role":"user","content":input.to_string()}]);
    before()?;
    let output = request(rt, config, messages.clone(), check)?;
    let parsed = parse(&output);
    let first = parsed
        .clone()
        .and_then(|v| citations.restore_and_validate(v));
    #[cfg(test)]
    diagnostic_shape(&input, &parsed, &first);
    if let Ok(value) = first {
        return Ok(value);
    }
    let reason = first.unwrap_err();
    check()?;
    // Regenerate from the original evidence instead of doubling context with a malformed reply.
    let mut repair = messages;
    repair.as_array_mut().unwrap().push(json!({"role":"user","content":format!("上次结果未通过校验：{reason}。请依据同一输入重新生成一个完整JSON对象，必须含summary和actions数组。summary每项须有text、kind、section、evidence。kind只能是discussion、decision、risk、question、suggestion、unanswered之一，不能使用任务类型stage/minutes/answer或章节名；section只能是topic、outline、progress、open、minutes、answer之一。evidence可以直接使用短编号字符串数组，如[\"ref_001\"]；只能逐字复制本次allowed_evidence中的编号，不能使用上个批次或历史结果的编号。单条事实最多32个引用，超过则拆成多条。没有依据不增加事实，缺失的行动负责人/日期用null。不要Markdown或说明文字。") }));
    before()?;
    let parsed = parse(&request(rt, config, repair, check)?);
    let result = parsed
        .clone()
        .and_then(|v| citations.restore_and_validate(v));
    #[cfg(test)]
    diagnostic_shape(&input, &parsed, &result);
    result.map_err(|e| format!("模型输出校验失败：{e}"))
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
    if kind == "stage"
        && matches!(scope, "" | "all")
        && crate::workspace::pending(&rt, meeting)?.is_empty()
    {
        if let Some(artifact) = detail["artifacts"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|a| {
                a["type"] == "stage"
                    && a["status"] != "stale"
                    && a["content"]["manual_text"].is_null()
            })
        {
            let previous:Option<String>=rt.db()?.query_row("SELECT id FROM analysis_jobs WHERE meeting_id=? AND type='stage' AND state='succeeded' ORDER BY rowid DESC LIMIT 1",[meeting],|r|r.get(0)).optional().map_err(err)?;
            if let Some(job) = previous {
                return Ok(json!({"job_id":job,"artifact_id":artifact["id"],"reused":true}));
            }
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
            for segment in &mut segments {
                let overlap = segment["state"] != "edited"
                    && crate::voice::playback_overlap(
                        &rt,
                        &mid,
                        segment["start_ms"].as_u64().unwrap_or(0),
                        segment["end_ms"].as_u64().unwrap_or(0),
                    )?;
                segment["assistant_playback_overlap"] = json!(overlap);
            }
            let assistant_overlap = segments
                .iter()
                .filter(|s| s["assistant_playback_overlap"] == true)
                .count();
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
            let pending_before = crate::workspace::pending(&rt, &mid)?;
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
            let knowledge = if context_allowed {
                crate::knowledge::retrieve(&rt, &mid, &retrieval_query)?
            } else {
                json!({"bindings":[],"excerpts":[],"terms":[]})
            };
            if output_kind == "stage" {
                segments
                    .sort_by_key(|s| !pending_before.contains_key(s["id"].as_str().unwrap_or("")));
            }
            let task_context = json!({"kind":output_kind,"template":instructions,"question":input["question"],"meeting_context":if context_allowed {detail["context"].clone()} else {Value::Null},"existing_actions":detail["actions"],"previous_summary":previous.as_ref().map(|a|&a["content"]),"chat_history":history,"selected_background":knowledge});
            let mut manifest = segments
                .iter()
                .map(|s| json!({"segment_id":s["id"],"revision":s["revision"]}))
                .collect::<Vec<_>>();
            let excluded_manifest = segments.iter().filter(|s|s["assistant_playback_overlap"]==true).map(|s|json!({"segment_id":s["id"],"revision":s["revision"],"reason":"assistant_playback_overlap"})).collect::<Vec<_>>();
            let mut snapshot = json!({"excluded_manifest":excluded_manifest,"citation_policy":"batch_aliases_no_assistant_overlap_v1","request":input,"manifest":manifest,"new_manifest":new_manifest,"template":detail["template"],"knowledge_manifest":knowledge["bindings"],"config":config,"generation":generation,"consent_version":detail["consent_version"],"previous_artifact_id":previous.as_ref().map(|a|&a["id"]),"through_ms":detail["duration_ms"]});
            let frozen_key = format!("{:x}", Sha256::digest(format!("{task}:{snapshot}")));
            rt.db()?.execute("UPDATE analysis_jobs SET idempotency_key=?,input_snapshot=?,state='running',attempts=attempts+1 WHERE id=?",params![frozen_key,snapshot.to_string(),task]).map_err(err)?;
            let known: HashSet<_> = segments
                .iter()
                .filter(|s| s["assistant_playback_overlap"] != true)
                .map(|s| {
                    (
                        s["id"].as_str().unwrap_or("").into(),
                        s["revision"].as_u64().unwrap_or(0),
                    )
                })
                .collect();
            if known.is_empty() {
                return Err("本轮原文均与小舵播报重叠，请先核对或修订原稿后再生成".into());
            }
            let deadline = std::time::Instant::now()
                + Duration::from_secs(if automatic.is_some() { 180 } else { 900 });
            let check = || {
                if std::time::Instant::now() >= deadline {
                    return Err("本轮分析超过总时限，请缩小范围后重试".into());
                }
                check_job(&rt, &mid, generation, &task, automatic)
            };
            let overhead = RULES.len() + task_context.to_string().len() + 1024;
            let batch_limit = input_budget(&config)?;
            let batch_limit = batch_limit
                .saturating_sub(overhead)
                .min(input_budget(&config)? * 2 / 5)
                .min(10 * 1024);
            if batch_limit < 256 {
                return Err("模板和会议背景超过上下文预算，请精简或提高配置容量".into());
            }
            let mut batches = vec![];
            let mut current = vec![];
            let mut size = 0;
            'segments: for seg in segments
                .iter()
                .filter(|s| s["assistant_playback_overlap"] != true)
            {
                let pieces = split_segment(seg, batch_limit.saturating_sub(512).max(64));
                for piece in pieces {
                    if output_kind == "stage"
                        && pending_before.contains_key(seg["id"].as_str().unwrap_or(""))
                    {
                        let exists:bool=rt.db()?.query_row("SELECT EXISTS(SELECT 1 FROM summary_fragments f JOIN artifacts a ON a.id=f.artifact_id WHERE f.meeting_id=? AND f.segment_id=? AND f.revision=? AND f.char_start<=? AND f.char_end>=? AND a.status!='stale')",params![mid,piece["segment_id"].as_str(),piece["revision"].as_u64(),piece["fragment_start"].as_u64(),piece["fragment_end"].as_u64()],|r|r.get(0)).map_err(err)?;
                        if exists {
                            continue;
                        }
                    }
                    let n = piece.to_string().len() + 96;
                    if size + n > batch_limit && !current.is_empty() {
                        batches.push(std::mem::take(&mut current));
                        if automatic.is_some() && batches.len() >= 2 {
                            break 'segments;
                        }
                        size = 0;
                    }
                    current.push(piece);
                    size += n;
                }
            }
            if !current.is_empty() {
                batches.push(current);
            }
            if automatic.is_some() {
                batches.truncate(2);
            }
            let fragments = batches.iter().flatten().cloned().collect::<Vec<_>>();
            let selected = fragments
                .iter()
                .filter_map(reference_pair)
                .collect::<HashSet<_>>();
            if output_kind == "stage" {
                new_manifest.retain(|r| reference_pair(r).is_some_and(|p| selected.contains(&p)));
                manifest.retain(|r| reference_pair(r).is_some_and(|p| selected.contains(&p)));
                snapshot["manifest"] = json!(manifest);
                snapshot["new_manifest"] = json!(new_manifest);
                rt.db()?
                    .execute(
                        "UPDATE analysis_jobs SET input_snapshot=? WHERE id=?",
                        params![snapshot.to_string(), task],
                    )
                    .map_err(err)?;
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
            let mut incomplete = covered < duration || gaps > 0 || assistant_overlap > 0;
            let version:u32=tx.query_row("SELECT coalesce(max(version),0)+1 FROM artifacts WHERE meeting_id=? AND type=?",params![mid,output_kind],|r|r.get(0)).map_err(err)?;
            if output_kind == "stage"
                && (pending_before.len() > new_manifest.len()
                    || fragments.iter().any(|f| {
                        f["fragment_start"].as_u64().unwrap_or(0) > 0
                            || segments
                                .iter()
                                .find(|s| s["id"] == f["segment_id"])
                                .is_some_and(|s| {
                                    f["fragment_end"].as_u64().unwrap_or(0)
                                        < s["text"].as_str().unwrap_or("").chars().count() as u64
                                })
                    }))
            {
                incomplete = true;
            }
            let artifact = id();
            let coverage = json!({"from_ms":0,"to_ms":duration,"processed_ms":covered,"assistant_playback_overlap_segments":assistant_overlap,"incomplete":incomplete,"model":config.model,"profile_id":config.profile_id,"config_version":config.config_version,"profile_name":config.display_name,"base_url":config.base_url,"question":input["question"],"generated_at":now(),"assistant_id":assistant_id,"template_version":detail["template"]["version"],"manifest":manifest,"knowledge_manifest":knowledge["bindings"],"background_excerpts":knowledge["excerpts"],"incremental":previous.is_some()});
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
            if output_kind == "stage" && matches!(scope, "" | "all") {
                tx.execute(
                    "INSERT INTO meeting_memories VALUES (?,?,?,?,?,?)",
                    params![
                        artifact,
                        mid,
                        generation,
                        detail["consent_version"].as_u64().unwrap_or(0),
                        serde_json::to_string(&manifest).map_err(err)?,
                        now()
                    ],
                )
                .map_err(err)?;
                tx.execute(
                    "UPDATE summary_schedules SET completed_asr_ms=(SELECT coalesce(sum(end_ms-start_ms),0) FROM asr_chunks WHERE meeting_id=?) WHERE meeting_id=?",
                    params![mid, mid],
                )
                .map_err(err)?;
            }
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
                for f in &fragments {
                    if pending_before.contains_key(f["segment_id"].as_str().unwrap_or("")) {
                        tx.execute(
                            "INSERT OR REPLACE INTO summary_fragments VALUES (?,?,?,?,?,?)",
                            params![
                                mid,
                                f["segment_id"].as_str(),
                                f["revision"].as_u64(),
                                f["fragment_start"].as_u64(),
                                f["fragment_end"].as_u64(),
                                artifact
                            ],
                        )
                        .map_err(err)?;
                    }
                }
                for r in &new_manifest {
                    let text:String=tx.query_row("SELECT text FROM transcript_revisions WHERE meeting_id=? AND segment_id=? AND revision=?",params![mid,r["segment_id"].as_str(),r["revision"].as_u64()],|row|row.get(0)).map_err(err)?;
                    let mut q=tx.prepare("SELECT f.char_start,f.char_end FROM summary_fragments f JOIN artifacts a ON a.id=f.artifact_id WHERE f.meeting_id=? AND f.segment_id=? AND f.revision=? AND a.status!='stale' ORDER BY f.char_start").map_err(err)?;
                    let spans = q
                        .query_map(
                            params![mid, r["segment_id"].as_str(), r["revision"].as_u64()],
                            |row| Ok((row.get::<_, u64>(0)?, row.get::<_, u64>(1)?)),
                        )
                        .map_err(err)?
                        .collect::<Result<Vec<_>, _>>()
                        .map_err(err)?;
                    let mut through = 0;
                    for (start, end) in spans {
                        if start > through {
                            break;
                        }
                        through = through.max(end);
                    }
                    if through >= text.chars().count() as u64 {
                        tx.execute("INSERT INTO summary_coverage VALUES (?,?,?) ON CONFLICT(meeting_id,segment_id) DO UPDATE SET revision=excluded.revision",params![mid,r["segment_id"].as_str(),r["revision"].as_u64()]).map_err(err)?;
                    }
                }
                let first_gap:Option<u64>=tx.query_row("SELECT min(s.start_ms) FROM transcript_segments s LEFT JOIN summary_coverage c ON c.segment_id=s.id AND c.meeting_id=s.meeting_id WHERE s.meeting_id=? AND s.state IN('final','edited') AND (c.revision IS NULL OR c.revision!=s.current_revision)",[&mid],|r|r.get(0)).map_err(err)?;
                let mut current_coverage = coverage.clone();
                current_coverage["to_ms"] =
                    json!(first_gap
                        .unwrap_or_else(|| detail["asr_processed_ms"].as_u64().unwrap_or(0)));
                current_coverage["asr_through_ms"] = detail["asr_processed_ms"].clone();
                tx.execute(
                    "UPDATE artifacts SET coverage_json=? WHERE id=?",
                    params![current_coverage.to_string(), artifact],
                )
                .map_err(err)?;
                tx.execute(
                    "DELETE FROM summary_model_backoff WHERE model_key=?",
                    [target(&config)],
                )
                .map_err(err)?;
                tx.execute(
                    "UPDATE summary_schedules SET failures=0,retry_after_utc=0,paused_reason=NULL WHERE meeting_id=?",
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
                    let transient = e.contains("HTTP 429")
                        || e.contains("HTTP 5")
                        || e.contains("连接失败")
                        || e.contains("超时")
                        || e.contains("响应读取失败");
                    let retry = e
                        .split("retry_after_seconds=")
                        .nth(1)
                        .and_then(|n| n.parse::<u64>().ok())
                        .unwrap_or(0);
                    let attempts: u64 = db
                        .query_row(
                            "SELECT failures FROM summary_schedules WHERE meeting_id=?",
                            [&mid],
                            |r| r.get(0),
                        )
                        .unwrap_or(0);
                    // Invalid output is recoverable, but cannot create an
                    // unlimited automatic model loop. Each job already has one
                    // repair; stop after three consecutive failed jobs.
                    let output_failure = e.starts_with("模型输出校验失败：");
                    let permanent = if output_failure {
                        attempts >= 2
                    } else {
                        !transient
                    };
                    let delay = match attempts {
                        0 => 30,
                        1 => 120,
                        _ => 300,
                    };
                    let retry_at = chrono::Utc::now()
                        .timestamp()
                        .saturating_add(i64::try_from(retry.max(delay)).unwrap_or(i64::MAX));
                    let _ = db.execute(
                        "UPDATE summary_schedules SET retry_after_utc=? WHERE meeting_id=?",
                        params![retry_at, mid],
                    );
                    if transient {
                        if let Ok(config) = serde_json::from_value::<ModelConfig>(
                            db.query_row(
                                "SELECT input_snapshot FROM analysis_jobs WHERE id=?",
                                [&task],
                                |r| r.get::<_, String>(0),
                            )
                            .ok()
                            .and_then(|s| serde_json::from_str::<Value>(&s).ok())
                            .unwrap_or(Value::Null)["config"]
                                .clone(),
                        ) {
                            let probe_after =
                                retry_at.max(chrono::Utc::now().timestamp().saturating_add(300));
                            let _=db.execute("INSERT INTO summary_model_backoff VALUES (?,?) ON CONFLICT(model_key) DO UPDATE SET retry_after_utc=max(retry_after_utc,excluded.retry_after_utc)",params![target(&config),probe_after]);
                        }
                    }
                    let _=db.execute("UPDATE summary_schedules SET failures=failures+1,paused_reason=CASE WHEN ? THEN ? ELSE NULL END WHERE meeting_id=?",params![permanent,e,mid]);
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
    fn output_capacity_grows_only_within_the_configured_capability() {
        let mut config = ModelConfig {
            context_tokens: 524288,
            max_output_tokens: 4096,
            ..ModelConfig::default()
        };
        assert_eq!(
            larger_output_config(&config).unwrap().max_output_tokens,
            8192
        );
        config.max_output_tokens = MAX_MODEL_OUTPUT_TOKENS;
        assert!(larger_output_config(&config).is_none());
        config.context_tokens = 8192;
        config.max_output_tokens = 8191;
        assert!(larger_output_config(&config).is_none());
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
    fn full_request_dynamic_boundary_is_independent_from_tokens_and_usage_is_aggregate() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let (_dir, rt, mut config) = fixture(&format!("http://{}", listener.local_addr().unwrap()));
        config.context_tokens = 131072;
        let limit = input_budget(&config).unwrap();
        let make = |n| json!([{"role":"user","content":"中".repeat(n)}]);
        let mut lo = 0usize;
        let mut hi = 200000usize;
        while lo < hi {
            let n = lo + (hi - lo).div_ceil(2);
            if request_body(&config, &make(n), true).to_string().len() <= limit {
                lo = n;
            } else {
                hi = n - 1;
            }
        }
        let messages = make(lo);
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let input = receive(&mut stream);
            assert!(
                input.to_string().len() > limit.saturating_sub(1024)
                    && input.to_string().len() <= limit
            );
            let mut result = reply("合成成功");
            result["usage"] = json!({"prompt_tokens":70000,"completion_tokens":5});
            respond(&mut stream, 200, result);
        });
        assert_eq!(
            request(&rt, &config, messages.clone(), &|| Ok(())).unwrap(),
            "合成成功"
        );
        server.join().unwrap();
        assert!(request(&rt, &config, make(lo + 1), &|| Ok(()))
            .unwrap_err()
            .contains("输入安全预算"));
        let row: usize = rt
            .db()
            .unwrap()
            .query_row(
                "SELECT ratio_milli FROM model_token_observations WHERE model_key=?",
                [target(&config)],
                |r| r.get(0),
            )
            .unwrap();
        assert!(row > 1000);
        let mut small = config;
        small.context_tokens = 32768;
        assert!(request(&rt, &small, messages, &|| Ok(()))
            .unwrap_err()
            .contains("模型上下文容量不足"));
        assert!(context_error(
            400,
            &json!({"error":{"code":"context_length_exceeded"}})
        ));
        assert!(!context_error(
            401,
            &json!({"error":{"message":"context window"}})
        ));
        assert!(!context_error(
            400,
            &json!({"error":{"message":"invalid model"}})
        ));
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
    fn minutes_task_type_is_separate_from_item_kind_and_invalid_kind_is_repaired() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let (_root, rt, c) = fixture(&format!("http://{}", listener.local_addr().unwrap()));
        let mut invalid = fact();
        invalid["summary"][0]["kind"] = json!("minutes");
        invalid["summary"][0]["section"] = json!("minutes");
        let known = HashSet::from([("s1".into(), 1)]);
        assert!(validate(&invalid, &known).unwrap_err().contains("kind"));
        let mut valid = invalid.clone();
        valid["summary"][0]["kind"] = json!("discussion");
        let expected = valid.clone();
        let server = thread::spawn(move || {
            let mut seen = vec![];
            for output in [invalid, valid] {
                let (mut socket, _) = listener.accept().unwrap();
                seen.push(receive(&mut socket));
                respond(&mut socket, 200, reply(&output.to_string()));
            }
            seen
        });
        let output = checked_request(&rt, &|| Ok(()), &c,
            json!({"kind":"minutes","transcript":[{"segment_id":"s1","revision":1,"text":"合成原文"}]}), &known).unwrap();
        assert_eq!(output, expected);
        let seen = server.join().unwrap();
        for body in &seen {
            let input: Value =
                serde_json::from_str(body["messages"][1]["content"].as_str().unwrap()).unwrap();
            assert_eq!(input["task_type"], "minutes");
            assert!(input.get("kind").is_none());
        }
        assert!(seen[1]["messages"][2]["content"]
            .as_str()
            .unwrap()
            .contains("不能使用任务类型"));
    }
    #[test]
    fn kind_aliases_need_no_remote_repair_and_cannot_bypass_evidence_validation() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let (_root, rt, c) = fixture(&format!("http://{}", listener.local_addr().unwrap()));
        let mut output = fact();
        let mut rows = vec![output["summary"][0].clone(); 4];
        for (row, kind) in rows
            .iter_mut()
            .zip([" Discussion ", "决策", "风险", "建议"])
        {
            row["kind"] = json!(kind);
        }
        output["summary"] = json!(rows);
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            receive(&mut socket);
            respond(&mut socket, 200, reply(&output.to_string()));
        });
        let known = HashSet::from([("s1".into(), 1)]);
        let output = checked_request(&rt, &|| Ok(()), &c,
            json!({"task_type":"stage","transcript":[{"segment_id":"s1","revision":1,"text":"合成原文"}]}), &known).unwrap();
        server.join().unwrap();
        assert_eq!(output["summary"][3]["kind"], "suggestion");
        assert_eq!(output["summary"][1]["kind"], "decision");
        let scope = CitationScope {
            aliases: HashMap::new(),
            known,
        };
        let mut invalid = fact();
        invalid["summary"][0]["kind"] = json!("决策");
        invalid["summary"][0]["evidence"][0]["segment_id"] = json!("outside");
        assert!(scope
            .restore_and_validate(invalid)
            .unwrap_err()
            .contains("输入范围以外"));
        let mut invalid = fact();
        invalid["summary"][0]["kind"] = json!("决策");
        invalid["summary"][0]["evidence"] = json!([]);
        assert!(scope
            .restore_and_validate(invalid)
            .unwrap_err()
            .contains("缺少依据"));
    }
    #[test]
    fn invalid_section_is_repaired_from_original_evidence_with_explicit_contract() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let (_root, rt, c) = fixture(&format!("http://{}", listener.local_addr().unwrap()));
        let mut invalid = fact();
        invalid["summary"][0]["section"] = json!("当前主题");
        let known = HashSet::from([("s1".into(), 1)]);
        assert!(validate(&invalid, &known)
            .unwrap_err()
            .contains("summary[0].section"));
        let server = thread::spawn(move || {
            for output in [invalid, fact()] {
                let (mut socket, _) = listener.accept().unwrap();
                let request = receive(&mut socket);
                let input: Value =
                    serde_json::from_str(request["messages"][1]["content"].as_str().unwrap())
                        .unwrap();
                assert_eq!(input["task_type"], "stage");
                assert_eq!(
                    input["output_contract"]["summary"]["item"]["section"]["enum"][0],
                    "topic"
                );
                respond(&mut socket, 200, reply(&output.to_string()));
            }
        });
        let output=checked_request(&rt,&||Ok(()),&c,json!({"task_type":"stage","transcript":[{"segment_id":"s1","revision":1,"text":"合成原文"}]}),&known).unwrap();
        assert_eq!(output, fact());
        server.join().unwrap();
        let mut invalid = fact();
        invalid["summary"][0]["kind"] = json!("stage");
        assert!(validate(&invalid, &known).is_err());
        invalid["summary"][0]["kind"] = json!("discussion");
        invalid["summary"][0]["section"] = json!("stage");
        assert!(validate(&invalid, &known).is_err());
    }
    #[test]
    fn model_unanswered_wording_is_normalized_but_uncited_facts_and_actions_still_fail() {
        let mut input =
            json!({"transcript":[{"segment_id":"original","revision":3,"text":"合成原文"}]});
        let scope =
            prepare_citations(&mut input, &HashSet::from([("original".into(), 3)])).unwrap();
        let value = json!({"summary":[{"text":"合成会议信息尚未说明，不能推断","kind":"unanswered","section":"minutes","evidence":[]}],"actions":[]});
        let accepted = scope.restore_and_validate(value.clone()).unwrap();
        assert_eq!(accepted["summary"][0]["text"], "会议中未明确");
        for kind in ["discussion", "decision", "risk", "question"] {
            let mut invalid = value.clone();
            invalid["summary"][0]["kind"] = json!(kind);
            assert!(scope
                .restore_and_validate(invalid)
                .unwrap_err()
                .contains("evidence为空"));
        }
        let mut action = value;
        action["actions"] = json!([{"task":"合成但无依据的行动","evidence":[]}]);
        assert!(scope.restore_and_validate(action).is_err());
        let mut too_many = fact();
        too_many["summary"][0]["evidence"] =
            json!(vec![json!({"segment_id":"original","revision":3}); 33]);
        let error = scope.restore_and_validate(too_many).unwrap_err();
        assert!(error.contains("引用过多"));
        assert!(!error.contains("缺少依据"));
    }
    #[test]
    fn short_reference_strings_expand_only_to_the_exact_frozen_pair() {
        let mut input =
            json!({"transcript":[{"segment_id":"original","revision":3,"text":"合成原文"}]});
        let scope =
            prepare_citations(&mut input, &HashSet::from([("original".into(), 3)])).unwrap();
        let mut value = fact();
        value["summary"][0]["evidence"] = json!(["ref_001"]);
        let out = scope.restore_and_validate(value.clone()).unwrap();
        assert_eq!(
            out["summary"][0]["evidence"],
            json!([{"segment_id":"original","revision":3}])
        );
        value["summary"][0]["evidence"] = json!(["ref_999"]);
        assert!(scope.restore_and_validate(value).is_err());
    }
    #[test]
    fn minutes_with_realistic_missing_metadata_and_short_references_need_no_repair() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let (_root, rt, c) = fixture(&format!("http://{}", listener.local_addr().unwrap()));
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let body = receive(&mut socket);
            let input: Value =
                serde_json::from_str(body["messages"][1]["content"].as_str().unwrap()).unwrap();
            let alias = input["allowed_evidence"][0]["segment_id"].clone();
            let out = json!({"summary":[{"text":"合成已讨论目标","kind":"discussion","section":"minutes","evidence":[alias]},{"text":"会议日期及主持人未提供，不能从本段确定","kind":"unanswered","section":"minutes","evidence":[]}],"actions":[]});
            respond(&mut socket, 200, reply(&out.to_string()));
        });
        let out=checked_request(&rt,&||Ok(()),&c,json!({"kind":"minutes","transcript":[{"segment_id":"original","revision":3,"text":"合成原文"}]}),&HashSet::from([("original".into(),3)])).unwrap();
        server.join().unwrap();
        assert_eq!(out["summary"][0]["evidence"][0]["segment_id"], "original");
        assert_eq!(out["summary"][1]["text"], "会议中未明确");
    }
    #[test]
    fn citation_aliases_exclude_overlap_and_fence_batches_revisions_and_reducers() {
        let known = HashSet::from([("long-original-a".into(), 3), ("long-original-b".into(), 1)]);
        let mut input = json!({"transcript":[{"segment_id":"long-original-a","revision":3,"text":"允许原文"},{"segment_id":"overlap","revision":1,"text":"不可外发的播报重叠片段","assistant_playback_overlap":true}],"previous_summary":{"summary":[{"evidence":[{"segment_id":"long-original-b","revision":1}]}]}});
        let scope = prepare_citations(&mut input, &known).unwrap();
        assert_eq!(input["transcript"].as_array().unwrap().len(), 1);
        assert_eq!(input["transcript"][0]["segment_id"], "ref_001");
        assert!(!input.to_string().contains("long-original"));
        assert!(!input.to_string().contains("播报重叠片段"));
        assert_eq!(
            input["previous_summary"]["summary"][0]["evidence"],
            json!([])
        );
        let mut output = fact();
        output["summary"][0]["evidence"] = json!([{"segment_id":"ref_001","revision":3}]);
        output["actions"] = json!([{"task":"合成行动","owner_text":null,"due_text":null,"evidence":[{"segment_id":"ref_001","revision":3}]}]);
        let restored = scope.restore_and_validate(output.clone()).unwrap();
        assert_eq!(
            restored["summary"][0]["evidence"][0]["segment_id"],
            "long-original-a"
        );
        assert_eq!(
            restored["actions"][0]["evidence"][0]["segment_id"],
            "long-original-a"
        );
        output["summary"][0]["evidence"][0]["revision"] = json!(2);
        assert!(scope.restore_and_validate(output.clone()).is_err());
        output["summary"][0]["evidence"] = json!([{"segment_id":"long-original-b","revision":1}]);
        assert!(scope.restore_and_validate(output).is_err());
        let mut reduce = json!({"parts":[restored]});
        let reduce_scope = prepare_citations(&mut reduce, &known).unwrap();
        assert_eq!(reduce_scope.known.len(), 1);
        assert_eq!(
            reduce["parts"][0]["summary"][0]["evidence"][0]["segment_id"],
            "ref_001"
        );
        let mut blocked = json!({"transcript":[{"segment_id":"overlap","revision":1,"assistant_playback_overlap":true}]});
        assert!(prepare_citations(&mut blocked, &known)
            .unwrap_err()
            .contains("人工核对"));
    }
    #[test]
    fn outside_batch_reference_is_repaired_using_only_allowed_aliases() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let (_root, rt, c) = fixture(&format!("http://{}", listener.local_addr().unwrap()));
        let server = thread::spawn(move || {
            let mut seen = vec![];
            for sid in ["other-batch", "ref_001"] {
                let (mut socket, _) = listener.accept().unwrap();
                seen.push(receive(&mut socket));
                let mut output = fact();
                output["summary"][0]["evidence"] = json!([{"segment_id":sid,"revision":1}]);
                respond(&mut socket, 200, reply(&output.to_string()));
            }
            seen
        });
        let output=checked_request(&rt,&||Ok(()),&c,json!({"kind":"minutes","transcript":[{"segment_id":"original","revision":1,"text":"合成允许原文"}]}),&HashSet::from([("original".into(),1),("other-batch".into(),1)])).unwrap();
        assert_eq!(
            output["summary"][0]["evidence"][0]["segment_id"],
            "original"
        );
        let requests = server.join().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(requests[1]["messages"][2]["content"]
            .as_str()
            .unwrap()
            .contains("allowed_evidence"));
    }
    #[test]
    fn whole_minutes_with_212_segments_and_10_playback_overlaps_persists_partial_draft() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let (_root, rt, _c) = fixture(&format!("http://{}", listener.local_addr().unwrap()));
        let mid = rt.create("合成整场纪要回归", "general").unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        {
            let mut db = rt.db().unwrap();
            let tx = db.transaction().unwrap();
            tx.execute(
                "UPDATE meetings SET state='completed',recording_duration_ms=212000 WHERE id=?",
                [&mid],
            )
            .unwrap();
            for i in 0..212 {
                let sid = id();
                let text = format!(
                    "合成段落{i}：{}",
                    "本段讨论会议目标与方案验收条件，观点仍需通过测试验证。".repeat(4)
                );
                tx.execute(
                    "INSERT INTO transcript_segments VALUES (?,?,?,?,'mixed',1,'final')",
                    params![sid, mid, i * 1000, (i + 1) * 1000],
                )
                .unwrap();
                tx.execute(
                    "INSERT INTO transcript_revisions VALUES (?,1,?,?,'asr','now')",
                    params![sid, mid, text],
                )
                .unwrap();
                if i < 200 && i % 20 == 0 {
                    tx.execute("INSERT INTO assistant_playback_events(id,meeting_id,text,start_host_ns,start_ms,end_ms) VALUES (?,?,'合成播报',0,?,?)",params![id(),mid,i*1000,i*1000+300]).unwrap();
                }
            }
            tx.commit().unwrap();
        }
        let stopped = Arc::new(AtomicBool::new(false));
        let stop = stopped.clone();
        let server = thread::spawn(move || {
            let mut inputs = vec![];
            let until = std::time::Instant::now() + Duration::from_secs(12);
            while !stop.load(Ordering::Acquire) && std::time::Instant::now() < until {
                let (mut socket, _) = match listener.accept() {
                    Ok(v) => v,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(e) => panic!("fake HTTP: {e}"),
                };
                let body = receive(&mut socket);
                let input: Value =
                    serde_json::from_str(body["messages"][1]["content"].as_str().unwrap()).unwrap();
                let reference = input["allowed_evidence"][0].clone();
                assert!(reference.is_object());
                let mut output = fact();
                output["summary"][0]["section"] = json!("minutes");
                output["summary"][0]["evidence"] = json!([reference]);
                respond(&mut socket, 200, reply(&output.to_string()));
                inputs.push(input);
            }
            inputs
        });
        let job = submit(rt.clone(), &mid, "minutes", "", "all").unwrap();
        let until = std::time::Instant::now() + Duration::from_secs(10);
        let (state, error) = loop {
            let v: (String, Option<String>) = rt
                .db()
                .unwrap()
                .query_row(
                    "SELECT state,error_code FROM analysis_jobs WHERE id=?",
                    [job["job_id"].as_str().unwrap()],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .unwrap();
            if !["pending", "running"].contains(&v.0.as_str()) {
                break v;
            }
            assert!(std::time::Instant::now() < until);
            thread::sleep(Duration::from_millis(10));
        };
        stopped.store(true, Ordering::Release);
        let inputs = server.join().unwrap();
        assert_eq!(state, "succeeded", "{error:?}");
        assert!(inputs.iter().any(|v| v["parts"].is_array()));
        assert_eq!(
            inputs
                .iter()
                .filter_map(|v| v["transcript"].as_array())
                .map(Vec::len)
                .sum::<usize>(),
            202
        );
        assert!(inputs
            .iter()
            .all(|v| !v.to_string().contains("assistant_playback_overlap\":true")));
        let d = rt.detail(&mid).unwrap();
        let artifact = &d["artifacts"][0];
        assert_eq!(artifact["status"], "incomplete");
        assert_eq!(
            artifact["coverage"]["assistant_playback_overlap_segments"],
            10
        );
        let sid = artifact["content"]["summary"][0]["evidence"][0]["segment_id"]
            .as_str()
            .unwrap();
        assert!(uuid::Uuid::parse_str(sid).is_ok());
        let raw: String = rt
            .db()
            .unwrap()
            .query_row(
                "SELECT input_snapshot FROM analysis_jobs WHERE id=?",
                [job["job_id"].as_str().unwrap()],
                |r| r.get(0),
            )
            .unwrap();
        let snapshot: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(snapshot["excluded_manifest"].as_array().unwrap().len(), 10);
        assert_eq!(snapshot["manifest"].as_array().unwrap().len(), 212);
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
    #[ignore = "requires explicit user permission for one real meeting model task and isolated source backup"]
    fn authorized_meeting_minutes_diagnostic() {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::env::var("HELMMEET_ALLOW_MEETING_MODEL").as_deref(),
            Ok("1")
        );
        let source = std::path::PathBuf::from(
            std::env::var_os("HELMMEET_MEETING_DIAGNOSTIC_SOURCE").expect("source root"),
        );
        let mid = std::env::var("HELMMEET_MEETING_DIAGNOSTIC_ID").expect("one meeting id");
        uuid::Uuid::parse_str(&mid).unwrap();
        let isolated = tempfile::tempdir().unwrap();
        std::fs::set_permissions(isolated.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        {
            let src = rusqlite::Connection::open_with_flags(
                source.join("app.sqlite"),
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            )
            .unwrap();
            let mut dest = rusqlite::Connection::open(isolated.path().join("app.sqlite")).unwrap();
            rusqlite::backup::Backup::new(&src, &mut dest)
                .unwrap()
                .run_to_completion(64, Duration::from_millis(5), None)
                .unwrap();
        }
        for name in ["credentials.json", "settings.json"] {
            if source.join(name).is_file() {
                std::fs::copy(source.join(name), isolated.path().join(name)).unwrap();
                std::fs::set_permissions(
                    isolated.path().join(name),
                    std::fs::Permissions::from_mode(0o600),
                )
                .unwrap();
            }
        }
        let rt = Runtime::open_with_vault(
            isolated.path().into(),
            Arc::new(crate::credentials::FileStore::new(isolated.path().into())),
        )
        .unwrap();
        let job = submit(rt.clone(), &mid, "minutes", "", "all").unwrap();
        let until = std::time::Instant::now() + Duration::from_secs(900);
        let (state, error) = loop {
            let row: (String, Option<String>) = rt
                .db()
                .unwrap()
                .query_row(
                    "SELECT state,error_code FROM analysis_jobs WHERE id=?",
                    [job["job_id"].as_str().unwrap()],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .unwrap();
            if !["pending", "running"].contains(&row.0.as_str()) {
                break row;
            }
            assert!(std::time::Instant::now() < until);
            thread::sleep(Duration::from_millis(100));
        };
        for h in std::mem::take(&mut *rt.analysis_threads.lock().unwrap()) {
            h.join().unwrap();
        }
        let report = json!({"state":state,"error":error,"requests":MEETING_DIAGNOSTICS.lock().unwrap().clone()});
        println!("MEETING_DIAGNOSTIC_SHAPE={report}");
        // The original app database and audio are never opened for writing.
        assert!(["succeeded", "failed"].contains(&state.as_str()));
        if std::env::var("HELMMEET_MEETING_DIAGNOSTIC_REQUIRE_SUCCESS").as_deref() == Ok("1") {
            assert_eq!(state, "succeeded", "isolated minutes validation failed");
        }
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
