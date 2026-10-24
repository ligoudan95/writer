//! AI NetGateway（FR-AI-01/08；5.1 chat_stream 契约的 M4 落地）。
//! 唯一网络出口原则：AI 流量全部经此模块（域名即用户配置的 Provider base_url）。
//! abort 真实传播：取消标志置位 → 关闭响应流（drop 连接），非仅 UI 忽略（FR-AI-08）。
//! B1：ai:chunk 按 50ms 批量聚合推送，控制 IPC 序列化开销。

use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use tokio::time::timeout;

use crate::db::AiProvider;
use crate::library::AppError;

/// 运行中的 AI 作业注册表（job_id → 取消标志）
pub struct AiState {
    pub jobs: Mutex<BTreeMap<String, Arc<AtomicBool>>>,
    /// 网络出口经 netcfg（默认直连绕过系统代理）；设置变更时热重建
    client: std::sync::RwLock<reqwest::Client>,
}

impl AiState {
    pub fn new() -> Self {
        Self::with_config(&crate::netcfg::load())
    }

    pub fn with_config(cfg: &crate::netcfg::NetConfig) -> Self {
        Self {
            jobs: Mutex::new(BTreeMap::new()),
            client: std::sync::RwLock::new(build_client(cfg)),
        }
    }

    /// Client 内部是 Arc，clone 廉价；进行中作业持旧客户端自然收尾
    pub fn client(&self) -> reqwest::Client {
        self.client
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// 网络设置变更后重建（旧客户端由运行中作业持有至结束）
    pub fn rebuild(&self, cfg: &crate::netcfg::NetConfig) {
        *self.client.write().unwrap_or_else(|e| e.into_inner()) = build_client(cfg);
    }
}

fn build_client(cfg: &crate::netcfg::NetConfig) -> reqwest::Client {
    // 不设 total timeout：思考模型（GLM-5.3 等）思考期持续流式输出可达数分钟，
    // 总超时会误杀——改由 chat_once 的流式空闲看门狗（120s 无字节）与非流式
    // 显式超时承担（2026-10-24 用户实测"AI 卡住"根因）
    crate::netcfg::apply_async(
        reqwest::Client::builder().connect_timeout(Duration::from_secs(15)),
        cfg,
    )
    .build()
    .unwrap_or_default()
}

impl Default for AiState {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMsg {
    pub role: String, // system | user | assistant
    pub content: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]  // job_id to jobId; undefined jobId gated out ALL ai events (root cause of stuck AI)
pub struct ChatStartAck {
    pub job_id: String,
    /// 预计发送字数（上下文 + 指令），用于发送前面板展示（FR-AI-03 上下文规范）
    pub chars: u64,
}

pub fn secrets_key(provider_id: &str) -> String {
    format!("ai:{provider_id}")
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// SSE 解析（纯逻辑，可单测）
// ---------------------------------------------------------------------------

/// 极简 SSE 增量解析器：**字节缓冲** + 按完整行解码。
/// SSE 按字节分帧，多字节 UTF-8 字符可能被 TCP/TLS 边界切开——
/// 逐 chunk from_utf8_lossy 会把切断的中文变成 U+FFFD 替换符（正文损坏）
#[derive(Default)]
pub struct SseParser {
    buf: Vec<u8>,
}

impl SseParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// 喂入一段响应字节，返回其中完整 data 行的负载（不含 "data: " 前缀）。
    /// UTF-8 多字节序列不含 0x0A，按 \n 字节切分不会截断字符
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<String> {
        self.buf.extend_from_slice(chunk);
        let mut out = Vec::new();
        while let Some(pos) = self.buf.iter().position(|&b| b == b'\n') {
            let line_bytes: Vec<u8> = self.buf.drain(..=pos).collect();
            let mut line = &line_bytes[..line_bytes.len() - 1];
            if line.last() == Some(&b'\r') {
                line = &line[..line.len() - 1];
            }
            if let Ok(s) = std::str::from_utf8(line) {
                if let Some(payload) = s.strip_prefix("data:") {
                    out.push(payload.trim_start().to_string());
                }
            }
            // 非 data 行（event:/注释/空行）按 SSE 规范忽略；非 UTF-8 行容错丢弃
        }
        out
    }
}

/// 从 chat.completions chunk JSON 中提取 delta 文本。
/// 流中 error 事件（OpenAI 兼容端中途下发的 {"error":...}）→ Err 归因上报，
/// 不再被静默吞掉后以不完整内容收场（FR-AI-08）
pub fn parse_delta(payload: &str) -> Result<Option<String>, AppError> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(payload) else {
        return Ok(None); // 非 JSON 负载（[DONE] 等）忽略
    };
    if let Some(err) = v.get("error") {
        let msg = err
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("未知错误");
        let code = match err
            .get("code")
            .and_then(|c| c.as_i64())
            .or_else(|| err.get("status").and_then(|c| c.as_i64()))
        {
            Some(401) | Some(403) => "ai_auth",
            Some(429) => "ai_rate_limited",
            _ => "ai_protocol",
        };
        return Err(AppError::new(code, format!("流中错误事件：{msg}")));
    }
    Ok(v.get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("delta"))
        .and_then(|d| d.get("content"))
        .and_then(|c| c.as_str())
        .map(|s| s.to_string()))
}

/// 从 chat.completions chunk 中提取思考增量（GLM-5.3 等思考模型；
/// 仅用于 UI 进度指示"思考中"，不进入正文）
pub fn parse_reasoning(payload: &str) -> Option<String> {
    let v = serde_json::from_str::<serde_json::Value>(payload).ok()?;
    v.get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("delta"))
        .and_then(|d| d.get("reasoning_content"))
        .and_then(|c| c.as_str())
        .map(|s| s.to_string())
}

/// 非流式响应提取完整回复
pub fn parse_full_response(body: &str) -> Result<String, AppError> {
    let v: serde_json::Value = serde_json::from_str(body)
        .map_err(|e| AppError::new("ai_protocol", format!("响应不是合法 JSON：{e}")))?;
    v.get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| AppError::new("ai_protocol", "响应缺少 choices[0].message.content"))
}

/// 模型列表响应解析
pub fn parse_models(body: &str) -> Result<Vec<String>, AppError> {
    let v: serde_json::Value = serde_json::from_str(body)
        .map_err(|e| AppError::new("ai_protocol", format!("模型列表解析失败：{e}")))?;
    let mut out = Vec::new();
    if let Some(data) = v.get("data").and_then(|d| d.as_array()) {
        for m in data {
            if let Some(id) = m.get("id").and_then(|i| i.as_str()) {
                out.push(id.to_string());
            }
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// 请求构造与错误归因（FR-AI-08）
// ---------------------------------------------------------------------------

/// 端点拼装：base_url 以版本段（v1/v4/…）结尾时视为已含版本——只追加业务路径，
/// 不再强插 v1（智谱 paas/v4 等非 v1 版本号根地址的适配，FR-AI-01 兼容扩展）
fn endpoint(base_url: &str, path: &str) -> String {
    let base = base_url.trim_end_matches('/');
    let is_versioned = base
        .rsplit('/')
        .next()
        .map(|s| {
            let t = s.strip_prefix('v').unwrap_or(s);
            !t.is_empty() && t.chars().all(|c| c.is_ascii_digit())
        })
        .unwrap_or(false);
    let tail = if is_versioned {
        path.strip_prefix("v1/").unwrap_or(path)
    } else {
        path
    };
    format!("{base}/{tail}")
}

fn map_status(status: u16) -> AppError {
    match status {
        401 | 403 => AppError::new("ai_auth", "API Key 无效或无权限（401/403）"),
        404 => AppError::new("ai_protocol", "端点不存在（404）：请检查 Base URL 是否为 OpenAI 兼容根地址"),
        429 => AppError::new("ai_rate_limited", "Provider 限流（429）"),
        500..=599 => AppError::new("net", format!("Provider 服务端错误（{status}）")),
        code => AppError::new("net", format!("Provider 返回 {code}")),
    }
}

fn map_reqwest_err(e: &reqwest::Error) -> AppError {
    if e.is_timeout() {
        AppError::new("net_timeout", format!("请求超时：{e}"))
    } else if e.is_connect() {
        AppError::new("net_connect", format!("无法连接 Provider：{e}"))
    } else {
        AppError::new("net", e.to_string())
    }
}

// ---------------------------------------------------------------------------
// 作业执行（异步流式；取消=关流断连）
// ---------------------------------------------------------------------------

pub fn register_job(state: &crate::ai::AiState, id: String, cancel: Arc<AtomicBool>) {
    state
        .jobs
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(id, cancel);
}

pub fn unregister_job(state: &AiState, id: &str) {
    state.jobs.lock().unwrap_or_else(|e| e.into_inner()).remove(id);
}

pub fn cancel_job(state: &AiState, id: &str) -> bool {
    match state.jobs.lock().unwrap_or_else(|e| e.into_inner()).get(id) {
        Some(f) => {
            f.store(true, Ordering::Release);
            true
        }
        None => false,
    }
}

/// 单次 AI 请求的全部上下文（作业线程捕获）
pub struct ChatJob {
    pub id: String,
    pub kind: String, // polish | continue | chat
    pub provider: AiProvider,
    pub api_key: String,
    pub messages: Vec<ChatMsg>,
    pub lib_root: std::path::PathBuf,
}

/// 执行一个聊天作业：流式消费 → 50ms 批量 ai:chunk → ai:done / ai:error / ai:cancelled。
/// emit 在异步任务中调用；lib_root 仅用于落请求日志（不含内容）。
pub async fn run_chat_job(
    state: Arc<AiState>,
    app: tauri::AppHandle,
    job: ChatJob,
) {
    use tauri::Emitter;
    let ChatJob { id, kind, provider, api_key, messages, lib_root } = job;
    let cancel = Arc::new(AtomicBool::new(false));
    register_job(&state, id.clone(), cancel.clone());
    let app2 = app.clone();
    let id2 = id.clone();
    let kind2 = kind.clone();
    let outcome = chat_once(
        &state.client(),
        &provider,
        &api_key,
        &messages,
        &cancel,
        |text| {
            let _ = app.emit(
                "ai:chunk",
                serde_json::json!({ "jobId": id, "kind": kind, "text": text }),
            );
        },
        // 思考流（GLM-5.3 等思考模型）：文本增量外显（chat_once 侧已 300ms 批聚）
        Some(Box::new(move |text: &str| {
            let _ = app2.emit(
                "ai:thinking",
                serde_json::json!({ "jobId": id2, "kind": kind2, "text": text }),
            );
        })),
    )
    .await;
    unregister_job(&state, &id);
    match outcome {
        Ok(content) => {
            let _ = app.emit(
                "ai:done",
                serde_json::json!({ "jobId": id, "kind": kind, "content": content }),
            );
        }
        Err(e) if e.code == "ai_cancelled" => {
            let _ = app.emit(
                "ai:cancelled",
                serde_json::json!({ "jobId": id, "kind": kind }),
            );
        }
        Err(e) => {
            let _ = app.emit(
                "ai:error",
                serde_json::json!({ "jobId": id, "kind": kind, "code": e.code, "message": e.message }),
            );
        }
    }
    let _ = lib_root; // 日志已在命令侧先行落库（无论成败，均记录发送事实）
}

/// 单次请求核心：流式开关 → SSE 消费/批聚 或 非流式单发。
/// on_thinking：思考模型进度回调（None = 不关心）；流式空闲 120s 视为断连。
async fn chat_once(
    client: &reqwest::Client,
    provider: &AiProvider,
    api_key: &str,
    messages: &[ChatMsg],
    cancel: &AtomicBool,
    mut emit_chunk: impl FnMut(&str),
    mut on_thinking: Option<Box<dyn FnMut(&str) + Send>>,
) -> Result<String, AppError> {
    // 发起请求前先查取消（零网络发送，FR-AI-08）
    if cancel.load(Ordering::Acquire) {
        return Err(AppError::new("ai_cancelled", "已取消"));
    }
    let url = endpoint(&provider.base_url, "v1/chat/completions");
    let body = serde_json::json!({
        "model": provider.model,
        "messages": messages,
        "stream": provider.stream,
    });
    let resp = client
        .post(&url)
        .bearer_auth(api_key)
        .json(&body)
        .send()
        .await
        .map_err(|e| map_reqwest_err(&e))?;
    let status = resp.status().as_u16();
    if !(200..=299).contains(&status) {
        // 错误体尽量带回摘要（多数 OpenAI 兼容端为 {error:{message}}）
        let text = resp.text().await.unwrap_or_default();
        let mut err = map_status(status);
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
            if let Some(m) = v.pointer("/error/message").and_then(|m| m.as_str()) {
                err.message = format!("{}：{}", err.message, m);
            }
        }
        return Err(err);
    }

    if !provider.stream {
        // 非流式：思考模型可能 3-5 分钟，给足显式超时（client 已无 total timeout）
        let text = tokio::time::timeout(Duration::from_secs(600), resp.text())
            .await
            .map_err(|_| AppError::new("net_timeout", "非流式请求超过 600 秒，已中断"))?
            .map_err(|e| map_reqwest_err(&e))?;
        let content = parse_full_response(&text)?;
        emit_chunk(&content);
        return Ok(content);
    }

    // 流式：SSE 增量 → 50ms 批量（B1）；空闲看门狗 120s（思考模型持续输出不会误杀）
    use futures_util::StreamExt;
    const IDLE_LIMIT: Duration = Duration::from_secs(120);
    let mut stream = resp.bytes_stream();
    let mut parser = SseParser::new();
    let mut full = String::new();
    let mut batch = String::new();
    let mut last_flush = Instant::now();
    let mut reasoning_batch = String::new();
    let mut thinking_emitted = false; // 首个思考增量立即外显（用户感知），其后 300ms 批聚
    let mut last_thinking_flush = Instant::now();
    let mut cancelled = false;
    loop {
        // 空闲看门狗：120s 无任何字节（含思考流）= 断连
        let item = match tokio::time::timeout(IDLE_LIMIT, stream.next()).await {
            Err(_) => {
                return Err(AppError::new(
                    "net_timeout",
                    "连接空闲超过 120 秒（无任何数据），已中断",
                ));
            }
            Ok(None) => break,
            Ok(Some(item)) => item,
        };
        if cancel.load(Ordering::Acquire) {
            cancelled = true;
            break; // drop stream → 连接断开（真实中断）
        }
        let chunk = item.map_err(|e| map_reqwest_err(&e))?;
        for payload in parser.feed(&chunk) {
            if payload == "[DONE]" {
                break; // 服务器随后自然关流；此处提前结束解析即可
            }
            // 思考增量：计入指示器（1s 节流外显），不进正文
            if let Some(r) = parse_reasoning(&payload) {
                reasoning_batch.push_str(&r);
                if !thinking_emitted || last_thinking_flush.elapsed() >= Duration::from_millis(300) {
                    if let Some(f) = on_thinking.as_mut() {
                        f(&reasoning_batch);
                    }
                    reasoning_batch.clear();
                    thinking_emitted = true;
                    last_thinking_flush = Instant::now();
                }
                continue;
            }
            match parse_delta(&payload) {
                Ok(Some(delta)) => {
                    // 首个正文到达：冲刷思考余量（前端据此终结"思考中"态）
                    if thinking_emitted && !reasoning_batch.is_empty() {
                        if let Some(f) = on_thinking.as_mut() {
                            f(&reasoning_batch);
                        }
                        reasoning_batch.clear();
                    }
                    full.push_str(&delta);
                    batch.push_str(&delta);
                }
                Ok(None) => {}
                // 流中 error 事件：先冲刷已收内容再上抛错误（前端按 ai:error 呈现）
                Err(e) => {
                    if !batch.is_empty() {
                        emit_chunk(&batch);
                        batch.clear();
                    }
                    return Err(e);
                }
            }
        }
        if !batch.is_empty() && last_flush.elapsed() >= Duration::from_millis(50) {
            emit_chunk(&batch);
            batch.clear();
            last_flush = Instant::now();
        }
    }
    if cancelled {
        if !batch.is_empty() {
            emit_chunk(&batch);
        }
        return Err(AppError::new("ai_cancelled", "已取消"));
    }
    if !batch.is_empty() {
        emit_chunk(&batch);
    }
    Ok(full)
}

// ---------------------------------------------------------------------------
// 连通性 / 模型列表（经 NetGateway 出口）
// ---------------------------------------------------------------------------

pub async fn fetch_models(
    client: &reqwest::Client,
    base_url: &str,
    api_key: &str,
) -> Result<Vec<String>, AppError> {
    let url = endpoint(base_url, "v1/models");
    let resp = client
        .get(&url)
        .bearer_auth(api_key)
        .timeout(Duration::from_secs(20))
        .send()
        .await
        .map_err(|e| map_reqwest_err(&e))?;
    let status = resp.status().as_u16();
    if !(200..=299).contains(&status) {
        return Err(map_status(status));
    }
    let text = resp.text().await.map_err(|e| map_reqwest_err(&e))?;
    parse_models(&text)
}

pub fn total_chars(messages: &[ChatMsg]) -> u64 {
    messages.iter().map(|m| m.content.chars().count() as u64).sum()
}

pub fn new_job_id() -> String {
    format!("{}-{}", now_secs(), uuid::Uuid::new_v4().simple())
}

// ---------------------------------------------------------------------------
// 单元测试（NFR-10 分层：解析与批聚逻辑脱离网络回归）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sse_parses_across_chunk_boundaries() {
        let mut p = SseParser::new();
        // 跨 chunk 断行 + 多行负载 + 忽略非 data 行
        let a = p.feed(b"event: message\ndata: {\"a\":1}\nda");
        assert_eq!(a, vec![r#"{"a":1}"#.to_string()]);
        let b = p.feed(b"ta: {\"b\":2}\n\ndata: [DONE]\n");
        assert_eq!(b, vec![r#"{"b":2}"#.to_string(), "[DONE]".to_string()]);
    }

    #[test]
    fn sse_handles_crlf() {
        let mut p = SseParser::new();
        let out = p.feed(b"data: x\r\ndata: y\r\n");
        assert_eq!(out, vec!["x".to_string(), "y".to_string()]);
    }

    #[test]
    fn sse_multibyte_split_across_chunks() {
        // 中文（多字节 UTF-8）被切在字节边界：字节缓冲必须无损还原，不得出 U+FFFD
        let mut p = SseParser::new();
        let payload = format!("data: {{\"choices\":[{{\"delta\":{{\"content\":\"{}\"}}}}]}}\n\n", "中文续写内容");
        let bytes = payload.as_bytes();
        let cut = bytes.len() / 2;
        let a = p.feed(&bytes[..cut]);
        let b = p.feed(&bytes[cut..]);
        assert!(a.is_empty() || !a.iter().any(|s| s.contains('\u{fffd}')));
        let all: Vec<String> = a.into_iter().chain(b).collect();
        let joined = all.join("");
        assert!(joined.contains("中文续写内容"), "切断的中文必须无损还原：{joined}");
        assert!(!joined.contains('\u{fffd}'));
    }

    #[test]
    fn delta_extraction() {
        assert_eq!(
            parse_delta(r#"{"choices":[{"delta":{"content":"你好"}}]}"#).unwrap().as_deref(),
            Some("你好")
        );
        assert_eq!(parse_delta(r#"{"choices":[{"delta":{"role":"assistant"}}]}"#).unwrap(), None);
        assert_eq!(parse_delta("not json").unwrap(), None);
    }

    #[test]
    fn reasoning_extraction() {
        assert_eq!(
            parse_reasoning(r#"{"choices":[{"delta":{"reasoning_content":"思考中"}}]}"#).as_deref(),
            Some("思考中")
        );
        // 正文 delta 不产 reasoning
        assert_eq!(parse_reasoning(r#"{"choices":[{"delta":{"content":"正文"}}]}"#), None);
        assert_eq!(parse_reasoning("not json"), None);
    }

    #[test]
    fn stream_error_event_attributed() {
        // 流中 error 事件：不再被吞掉
        let e = parse_delta(r#"{"error":{"message":"quota exceeded","code":429}}"#).unwrap_err();
        assert_eq!(e.code, "ai_rate_limited");
        let e = parse_delta(r#"{"error":{"message":"bad key","code":401}}"#).unwrap_err();
        assert_eq!(e.code, "ai_auth");
    }

    #[test]
    fn full_response_and_models() {
        let body = r#"{"choices":[{"message":{"role":"assistant","content":"完整回复"}}]}"#;
        assert_eq!(parse_full_response(body).unwrap(), "完整回复");
        assert!(parse_full_response("{}").is_err());

        let models = parse_models(r#"{"data":[{"id":"gpt-x"},{"id":"glm-4"}]}"#).unwrap();
        assert_eq!(models, vec!["gpt-x", "glm-4"]);
    }

    #[test]
    fn status_error_attribution() {
        assert_eq!(map_status(401).code, "ai_auth");
        assert_eq!(map_status(429).code, "ai_rate_limited");
        assert_eq!(map_status(404).code, "ai_protocol");
        assert_eq!(map_status(502).code, "net");
    }

    #[test]
    fn endpoint_join_and_chars() {
        assert_eq!(endpoint("https://api.x.com/", "v1/chat/completions"), "https://api.x.com/v1/chat/completions");
        // 带版本段根地址：不再强插 v1（智谱 paas/v4）
        assert_eq!(endpoint("https://open.bigmodel.cn/api/paas/v4", "v1/chat/completions"), "https://open.bigmodel.cn/api/paas/v4/chat/completions");
        assert_eq!(endpoint("https://open.bigmodel.cn/api/paas/v4/", "v1/models"), "https://open.bigmodel.cn/api/paas/v4/models");
        // 根路径含端口/无版本段：保持 v1 前缀
        assert_eq!(endpoint("https://api.deepseek.com", "v1/chat/completions"), "https://api.deepseek.com/v1/chat/completions");
        let msgs = vec![
            ChatMsg { role: "user".into(), content: "你好".into() },
            ChatMsg { role: "system".into(), content: "abc".into() },
        ];
        assert_eq!(total_chars(&msgs), 5);
    }

    #[tokio::test]
    async fn cancel_flag_breaks_stream() {
        // 直接验证取消语义：取消标志一旦置位，流消费立即中断（连接 drop 由上层保证）
        let cancel = AtomicBool::new(true); // 已取消
        let provider = AiProvider {
            id: String::new(),
            name: "t".into(),
            base_url: "http://127.0.0.1:9".into(), // 不可达端口
            model: "m".into(),
            enabled: true,
            stream: true,
            is_default: false,
        };
        let client = reqwest::Client::new();
        let r = chat_once(&client, &provider, "k", &[], &cancel, |_t| {}, None).await;
        // 请求前先查取消：未发出任何网络请求即返回 ai_cancelled
        assert!(r.is_err());
        assert_eq!(r.unwrap_err().code, "ai_cancelled");
    }
}

#[cfg(test)]
mod socket_tests {
    use super::*;
    use std::io::{Read, Write};

    /// 本地 OpenAI 兼容 SSE 服务器（单连接）：返回 chunk json 流 + [DONE]
    fn spawn_sse_server() -> (String, std::sync::mpsc::Receiver<()>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            if let Ok((mut sock, _)) = listener.accept() {
                let mut buf = [0u8; 4096];
                let _ = sock.read(&mut buf); // 请求头（足够即可）
                let body = concat!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n",
                    "data: {\"choices\":[{\"delta\":{\"content\":\"你好\"}}]}\n\n",
                    "data: {\"choices\":[{\"delta\":{\"content\":\"，续写\"}}]}\n\n",
                    "data: [DONE]\n\n",
                );
                let _ = sock.write_all(body.as_bytes());
                let _ = sock.flush();
                let _ = tx.send(());
            }
        });
        (format!("http://127.0.0.1:{port}"), rx)
    }

    fn provider_for(base: &str, stream: bool) -> AiProvider {
        AiProvider {
            id: String::new(),
            name: "mock".into(),
            base_url: base.into(),
            model: "m".into(),
            enabled: true,
            stream,
            is_default: false,
        }
    }

    #[tokio::test]
    async fn chat_once_stream_over_real_socket() {
        let (base, _done) = spawn_sse_server();
        let provider = provider_for(&base, true);
        let client = reqwest::Client::new();
        let cancel = AtomicBool::new(false);
        let collected = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let c2 = collected.clone();
        let content = chat_once(&client, &provider, "k", &[], &cancel, move |t| {
            c2.lock().unwrap().push_str(t);
        }, None)
        .await
        .unwrap();
        // 完整内容 = 各 delta 拼接（解析 + [DONE] 终止 + 聚合全对）
        assert_eq!(content, "你好，续写");
        assert_eq!(*collected.lock().unwrap(), content, "批聚推送的总和必须等于完整内容");
    }

    /// 思考模型流：reasoning_content 大量输出不进正文、不误判；正文照常拼接
    #[tokio::test]
    async fn chat_once_thinking_stream() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            if let Ok((mut sock, _)) = listener.accept() {
                let mut buf = [0u8; 4096];
                let _ = sock.read(&mut buf);
                let body = concat!(
                    "HTTP/1.1 200 OK
Content-Type: text/event-stream

",
                    "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"想一点\"}}]}

",
                    "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"再想一点\"}}]}

",
                    "data: {\"choices\":[{\"delta\":{\"content\":\"正文\"}}]}

",
                    "data: [DONE]

",
                );
                let _ = sock.write_all(body.as_bytes());
                let _ = sock.flush();
            }
        });
        let provider = provider_for(&format!("http://127.0.0.1:{port}"), true);
        let client = reqwest::Client::new();
        let cancel = AtomicBool::new(false);
        let think = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let t2 = think.clone();
        let content = chat_once(&client, &provider, "k", &[], &cancel, |_| {}, Some(Box::new(move |t: &str| {
            t2.lock().unwrap().push_str(t);
        })))
        .await
        .unwrap();
        assert_eq!(content, "正文", "思考内容不得进入正文");
        assert!(think.lock().unwrap().contains("想一点"), "思考文本增量应经回调外显");
    }

    #[tokio::test]
    async fn cancel_before_send_hits_no_server() {
        // 取消先于请求 → 不应有任何连接（服务器 accept 超时证明零流量）
        let (base, done) = spawn_sse_server();
        let provider = provider_for(&base, true);
        let client = reqwest::Client::new();
        let cancel = AtomicBool::new(true);
        let r = chat_once(&client, &provider, "k", &[], &cancel, |_| {}, None).await;
        assert_eq!(r.unwrap_err().code, "ai_cancelled");
        // 服务器线程未收到连接（1.5s 内无 accept）
        assert!(done.recv_timeout(Duration::from_millis(1500)).is_err(), "取消后不得产生任何网络请求（A7 精神）");
    }

    #[tokio::test]
    async fn models_fetch_over_real_socket() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            if let Ok((mut sock, _)) = listener.accept() {
                let mut buf = [0u8; 1024];
                let _ = sock.read(&mut buf);
                let body = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n{\"data\":[{\"id\":\"glm-4.7\"},{\"id\":\"gpt-test\"}]}";
                let _ = sock.write_all(body.as_bytes());
            }
        });
        let models = fetch_models(&reqwest::Client::new(), &format!("http://127.0.0.1:{port}"), "k").await.unwrap();
        assert_eq!(models, vec!["glm-4.7", "gpt-test"]);
    }
}
