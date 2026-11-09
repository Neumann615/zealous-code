//! Provider 抽象与 OpenAI-compatible 实现(spec 10)。
//!
//! 失败语义(A6):配置缺失/非法 → `Invalid`,创建任务即失败并落账;
//! 网络/5xx/429 → 指数退避重试,耗尽 → 可解释错误;4xx(除 429)不重试。
//! `api_key` 只存在于 Provider 内部,禁止进入账本与日志(spec 20)。

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use std::io::{BufRead, BufReader};
use std::time::Duration;

/// 对话角色:user/assistant/tool(system 单独字段承载)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatRole {
    User,
    Assistant,
    /// spec 164:工具结果消息(对应一次 tool_call)
    Tool,
}

/// spec 164:一次工具调用(assistant 发起)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCallInfo {
    pub id: String,
    pub name: String,
    /// JSON 文本(分片拼装后的完整参数)
    pub arguments: String,
}

/// spec 164:单轮模型用量
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Usage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
}

/// spec 164:工具定义(OpenAI function 形态;parameters 为 JSON Schema 文本)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub parameters: String,
}

/// 一条历史消息(spec 70:多轮上下文原生传递,不再拼接进单个字符串)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatMessage {
    pub role: ChatRole,
    pub content: String,
    /// spec 164:assistant 消息携带的工具调用
    pub tool_calls: Vec<ToolCallInfo>,
    /// spec 164:tool 结果消息对应的调用 id
    pub tool_call_id: Option<String>,
}

impl ChatMessage {
    pub fn user(content: impl Into<String>) -> Self {
        ChatMessage {
            role: ChatRole::User,
            content: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        ChatMessage {
            role: ChatRole::Assistant,
            content: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }

    /// spec 164:携带工具调用的 assistant 消息
    pub fn assistant_with_calls(calls: Vec<ToolCallInfo>) -> Self {
        ChatMessage {
            role: ChatRole::Assistant,
            content: String::new(),
            tool_calls: calls,
            tool_call_id: None,
        }
    }

    /// spec 164:工具结果消息
    pub fn tool_result(call_id: impl Into<String>, content: impl Into<String>) -> Self {
        ChatMessage {
            role: ChatRole::Tool,
            content: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: Some(call_id.into()),
        }
    }
}

/// 单次模型调用请求:system + 历史消息序列(顺序即语义)
#[derive(Debug, Clone)]
pub struct ChatRequest {
    pub system: Option<String>,
    pub messages: Vec<ChatMessage>,
    /// spec 164:可用工具(native tool-calls;空 = 不声明工具)
    pub tools: Vec<ToolDef>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatResponse {
    pub text: String,
    /// spec 164:模型发起的工具调用(原生 tool-calls)
    pub tool_calls: Vec<ToolCallInfo>,
    /// spec 164:本轮用量(流式需 include_usage;缺省 None)
    pub usage: Option<Usage>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderError {
    /// 配置缺失/非法——创建任务即失败的原因(A6)
    Invalid(String),
    /// 可重试的暂时性失败(网络/5xx/429),只携带底层原因文本
    /// (spec 152:不再滥用 Exhausted,避免错误双重嵌套)
    Transient(String),
    /// 重试耗尽仍失败,携带最后一次可解释原因
    Exhausted { attempts: u32, last: String },
    /// 不可重试的 4xx
    Rejected { status: u16, summary: String },
}

impl std::fmt::Display for ProviderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProviderError::Invalid(r) => write!(f, "provider_config_invalid: {r}"),
            ProviderError::Transient(r) => write!(f, "{r}"),
            ProviderError::Exhausted { attempts, last } => {
                write!(f, "provider_exhausted after {attempts} attempts: {last}")
            }
            ProviderError::Rejected { status, summary } => {
                write!(f, "provider_rejected {status}: {summary}")
            }
        }
    }
}

impl std::error::Error for ProviderError {}

/// 解析后的配置。`Display` 与 `Debug` 都刻意不输出 api_key
/// (spec 10/70:任何失败输出不得含凭据;derive(Debug) 曾把 key 打进
/// 测试 panic 消息,已改为手动实现)。
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderConfig {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    /// 单次 HTTP 超时(默认 120s,spec 10)
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,
    /// 网络错误/5xx/429 的重试次数(默认 3,spec 10)
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,
}

/// Debug 脱敏:api_key 只显示长度
impl std::fmt::Debug for ProviderConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderConfig")
            .field("base_url", &self.base_url)
            .field("api_key_len", &self.api_key.len())
            .field("model", &self.model)
            .field("timeout_secs", &self.timeout_secs)
            .field("max_retries", &self.max_retries)
            .finish()
    }
}

fn default_timeout_secs() -> u64 {
    120
}
fn default_max_retries() -> u32 {
    3
}

impl ProviderConfig {
    /// 解析优先级(spec 10,代码内固定):环境变量 > 用户配置文件。
    /// 配置文件:`~/.zealous/config.json`
    pub fn resolve() -> Result<Self, ProviderError> {
        Self::resolve_from(Self::config_path())
    }

    /// 测试/嵌入用法:显式指定配置文件路径(None = 无配置文件),
    /// 使测试不依赖本机真实配置(否则全局配置存在时"无配置"分支不可测)。
    pub(crate) fn resolve_from(file: Option<PathBuf>) -> Result<Self, ProviderError> {
        let from_file = file.and_then(Self::read_config_file);
        let pick = |env_key: &str, file_val: Option<String>| -> Option<String> {
            std::env::var(env_key)
                .ok()
                .filter(|v| !v.trim().is_empty())
                .or(file_val)
        };
        let cfg = Self {
            base_url: pick(
                "ZEALOUS_BASE_URL",
                from_file.as_ref().map(|c| c.base_url.clone()),
            )
            .ok_or_else(|| ProviderError::Invalid("missing base_url".into()))?,
            api_key: pick(
                "ZEALOUS_API_KEY",
                from_file.as_ref().map(|c| c.api_key.clone()),
            )
            .ok_or_else(|| ProviderError::Invalid("missing api_key".into()))?,
            model: pick("ZEALOUS_MODEL", from_file.as_ref().map(|c| c.model.clone()))
                .ok_or_else(|| ProviderError::Invalid("missing model".into()))?,
            timeout_secs: pick(
                "ZEALOUS_TIMEOUT_SECS",
                from_file.as_ref().map(|c| c.timeout_secs.to_string()),
            )
            .and_then(|v| v.parse().ok())
            .unwrap_or_else(default_timeout_secs),
            max_retries: pick(
                "ZEALOUS_MAX_RETRIES",
                from_file.as_ref().map(|c| c.max_retries.to_string()),
            )
            .and_then(|v| v.parse().ok())
            .unwrap_or_else(default_max_retries),
        };
        cfg.validate()?;
        Ok(cfg)
    }

    fn config_path() -> Option<PathBuf> {
        dirs_home().map(|h| h.join(".zealous").join("config.json"))
    }

    fn read_config_file(path: PathBuf) -> Option<Self> {
        let text = std::fs::read_to_string(path).ok()?;
        serde_json::from_str(&text).ok()
    }

    fn validate(&self) -> Result<(), ProviderError> {
        if self.base_url.trim().is_empty() {
            return Err(ProviderError::Invalid("empty base_url".into()));
        }
        if !self.base_url.starts_with("http://") && !self.base_url.starts_with("https://") {
            return Err(ProviderError::Invalid(format!(
                "base_url must be http(s): {}",
                self.base_url
            )));
        }
        if self.api_key.trim().is_empty() {
            return Err(ProviderError::Invalid("empty api_key".into()));
        }
        if self.model.trim().is_empty() {
            return Err(ProviderError::Invalid("empty model".into()));
        }
        if self.timeout_secs == 0 || self.timeout_secs > 1800 {
            return Err(ProviderError::Invalid(format!(
                "timeout_secs out of range: {}",
                self.timeout_secs
            )));
        }
        Ok(())
    }

    fn timeout(&self) -> Duration {
        Duration::from_secs(self.timeout_secs)
    }
}

fn dirs_home() -> Option<PathBuf> {
    // 不引 dirs crate:Windows 用 USERPROFILE,*nix 用 HOME
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
}

/// Provider trait:内核只依赖此抽象;测试用 Mock 替换(spec 50)。
pub trait Provider: Send + Sync {
    fn complete(&self, model: &str, req: &ChatRequest) -> Result<ChatResponse, ProviderError>;

    /// spec 164: 流式调用;分型事件实时外发(text/reasoning/tool-args),
    /// 返回累计结果。默认实现忽略事件委托 complete(Mock 等替身零改动)。
    fn complete_streaming(
        &self,
        model: &str,
        req: &ChatRequest,
        on_event: &dyn Fn(ProviderStreamEvent),
    ) -> Result<ChatResponse, ProviderError> {
        let _ = on_event;
        self.complete(model, req)
    }
}

/// spec 164:流式事件分型(对齐 dsh-TUI text/reasoning/tool-args 词汇)
#[derive(Debug, Clone)]
pub enum ProviderStreamEvent {
    /// 助手正文增量(打字机)
    TextDelta(String),
    /// 思考流增量(reasoning_content;渲染由客户端决定)
    ReasoningDelta(String),
    /// 工具调用参数分片(按 index 分桶拼装)
    ToolCallDelta {
        index: usize,
        id: Option<String>,
        name: Option<String>,
        args_delta: String,
    },
}

/// 重试策略:退避序列由调用方注入,测试可零等待。
pub struct RetryPolicy {
    pub max_retries: u32,
    /// 第 n 次重试前的等待(毫秒);传入 n 从 0 开始
    pub backoff: Box<dyn Fn(u32) -> Duration + Send + Sync>,
}

impl RetryPolicy {
    pub fn default_policy() -> Self {
        Self {
            max_retries: 3,
            backoff: Box::new(|n| Duration::from_millis(500u64.saturating_mul(1 << n.min(6)))),
        }
    }
}

/// OpenAI-compatible chat/completions 客户端(同步,ureq + rustls)。
pub struct OpenAiCompatProvider {
    config: ProviderConfig,
    retry: RetryPolicy,
    /// 显式注入 NativeTls 连接器的 agent:ureq 2.12 仅启用 native-tls
    /// feature(无 rustls)时,https 默认处理器直接拒绝请求,必须经
    /// AgentBuilder::tls_connector 提供 TlsConnector。
    agent: ureq::Agent,
    /// 实际发起的 HTTP 尝试次数(观测/测试用)
    pub attempts: std::sync::atomic::AtomicU32,
}

impl OpenAiCompatProvider {
    pub fn new(config: ProviderConfig) -> Self {
        // Windows 上 native-tls 走 schannel,零额外编译依赖(不用 rustls/ring)
        let tls: Arc<ureq::native_tls::TlsConnector> =
            Arc::new(ureq::native_tls::TlsConnector::new().expect("init native TLS (schannel)"));
        let agent = ureq::AgentBuilder::new()
            .tls_connector(tls)
            .timeout(config.timeout())
            .build();
        Self {
            config,
            retry: RetryPolicy::default_policy(),
            agent,
            attempts: std::sync::atomic::AtomicU32::new(0),
        }
    }

    #[cfg(test)]
    fn with_retry(mut self, retry: RetryPolicy) -> Self {
        self.retry = retry;
        self
    }

    fn endpoint(&self) -> String {
        let base = self.config.base_url.trim_end_matches('/');
        format!("{base}/chat/completions")
    }

    fn once(&self, model: &str, req: &ChatRequest) -> Result<ChatResponse, ProviderError> {
        self.attempts
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        // OpenAI 映射:system 在前,历史 user/assistant 按序透传(spec 70)
        let messages = messages_json(req);

        let mut body = serde_json::json!({
            "model": model,
            "messages": messages,
        });
        // spec 164:native tool-calls 声明
        if !req.tools.is_empty() {
            let tools: Vec<serde_json::Value> = req
                .tools
                .iter()
                .map(|td| {
                    let params =
                        serde_json::from_str::<serde_json::Value>(&td.parameters).unwrap_or_else(
                            |_| serde_json::json!({"type": "object", "properties": {}}),
                        );
                    serde_json::json!({
                        "type": "function",
                        "function": {
                            "name": td.name,
                            "description": td.description,
                            "parameters": params,
                        },
                    })
                })
                .collect();
            body["tools"] = serde_json::Value::Array(tools);
        }
        let resp = self
            .agent
            .post(&self.endpoint())
            .set("Authorization", &format!("Bearer {}", self.config.api_key))
            .set("Content-Type", "application/json")
            .send_string(&body.to_string())
            .map_err(classify_ureq)?;

        let status = resp.status();
        let text = resp.into_string().map_err(|e| ProviderError::Rejected {
            status,
            summary: format!("unreadable body: {e}"),
        })?;
        let parsed: ChatCompletionsResponse =
            serde_json::from_str(&text).map_err(|e| ProviderError::Rejected {
                status,
                summary: format!("bad json: {e}"),
            })?;
        parsed.into_response()
    }

    /// spec 164: SSE 流式调用。逐行解析 `data:` 帧(分型事件:正文/思考/
    /// 工具分片),`[DONE]` 终止;末块 usage 并入返回值。坏行容错跳过。
    fn once_streaming(
        &self,
        model: &str,
        req: &ChatRequest,
        on_event: &dyn Fn(ProviderStreamEvent),
    ) -> Result<ChatResponse, ProviderError> {
        self.attempts
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let messages = messages_json(req);
        let body = serde_json::json!({
            "model": model,
            "messages": messages,
            "stream": true,
            "stream_options": { "include_usage": true },
        })
        .to_string();
        let resp = self
            .agent
            .post(&self.endpoint())
            .set("Authorization", &format!("Bearer {}", self.config.api_key))
            .set("Content-Type", "application/json")
            .send_string(&body.to_string())
            .map_err(classify_ureq)?;
        let mut acc = StreamAccum::default();
        for line in BufReader::new(resp.into_reader()).lines() {
            let line = line.map_err(|e| ProviderError::Transient(format!("stream read: {e}")))?;
            let Some(data) = line.strip_prefix("data:") else {
                continue;
            };
            let data = data.trim();
            if data.is_empty() || data == "[DONE]" {
                if data == "[DONE]" {
                    break;
                }
                continue;
            }
            let Ok(v) = serde_json::from_str::<serde_json::Value>(data) else {
                continue;
            };
            for ev in apply_stream_chunk(&v, &mut acc) {
                on_event(ev);
            }
        }
        if acc.text.is_empty() && acc.calls.is_empty() {
            return Err(ProviderError::Rejected {
                status: 200,
                summary: "empty stream".into(),
            });
        }
        Ok(ChatResponse {
            text: acc.text,
            tool_calls: acc
                .calls
                .into_iter()
                .map(|c| ToolCallInfo {
                    id: c.id.unwrap_or_default(),
                    name: c.name.unwrap_or_default(),
                    arguments: c.args,
                })
                .filter(|c| !c.name.is_empty())
                .collect(),
            usage: acc.usage,
        })
    }
}

impl Provider for OpenAiCompatProvider {
    /// spec 164: 流式重试语义 —— 首个事件到达前失败按既有策略重试;
    /// 流已开始(事件已外发)后失败不再重试,避免 delta 重复。
    fn complete_streaming(
        &self,
        model: &str,
        req: &ChatRequest,
        on_event: &dyn Fn(ProviderStreamEvent),
    ) -> Result<ChatResponse, ProviderError> {
        let attempts = self.retry.max_retries + 1;
        let mut last_err: Option<String> = None;
        for attempt in 0..attempts {
            let started = std::sync::atomic::AtomicBool::new(false);
            let cb = |ev: ProviderStreamEvent| {
                started.store(true, std::sync::atomic::Ordering::Relaxed);
                on_event(ev);
            };
            match self.once_streaming(model, req, &cb) {
                Ok(resp) => return Ok(resp),
                Err(e @ ProviderError::Rejected { .. }) => return Err(e),
                Err(e) => {
                    if started.load(std::sync::atomic::Ordering::Relaxed) {
                        return Err(e);
                    }
                    last_err = Some(e.to_string());
                }
            }
            if attempt + 1 < attempts {
                std::thread::sleep((self.retry.backoff)(attempt));
            }
        }
        Err(ProviderError::Exhausted {
            attempts,
            last: last_err.unwrap_or_else(|| "unknown".into()),
        })
    }

    fn complete(&self, model: &str, req: &ChatRequest) -> Result<ChatResponse, ProviderError> {
        let attempts = self.retry.max_retries + 1;
        let mut last_err: Option<String> = None;
        for attempt in 0..attempts {
            match self.once(model, req) {
                Ok(resp) => return Ok(resp),
                // 4xx(非 429)不可重试:立即以可解释状态失败
                Err(e @ ProviderError::Rejected { .. }) => return Err(e),
                Err(e) => {
                    last_err = Some(e.to_string());
                }
            }
            if attempt + 1 < attempts {
                std::thread::sleep((self.retry.backoff)(attempt));
            }
        }
        Err(ProviderError::Exhausted {
            attempts,
            last: last_err.unwrap_or_else(|| "unknown".into()),
        })
    }
}

/// spec 164:流式分片桶(按 tool_calls.index 聚合)
#[derive(Default)]
struct StreamCallAccum {
    index: usize,
    id: Option<String>,
    name: Option<String>,
    args: String,
}

/// spec 164:流式累积器(正文 + 工具调用 + 用量)
#[derive(Default)]
struct StreamAccum {
    text: String,
    calls: Vec<StreamCallAccum>,
    usage: Option<Usage>,
}

/// spec 164:解析一个 chunk(纯函数,可测);返回应外发的事件序列。
/// 与 dsh-TUI 的事件词汇同构:text / reasoning / tool-args 三型。
fn apply_stream_chunk(
    v: &serde_json::Value,
    acc: &mut StreamAccum,
) -> Vec<ProviderStreamEvent> {
    let mut events = Vec::new();
    if let Some(u) = v.get("usage").filter(|u| u.is_object()) {
        acc.usage = Some(Usage {
            prompt_tokens: u["prompt_tokens"].as_u64().unwrap_or(0),
            completion_tokens: u["completion_tokens"].as_u64().unwrap_or(0),
            total_tokens: u["total_tokens"].as_u64().unwrap_or(0),
        });
    }
    let Some(choices) = v["choices"].as_array() else {
        return events;
    };
    for ch in choices {
        let delta = &ch["delta"];
        if let Some(rc) = delta["reasoning_content"].as_str().filter(|s| !s.is_empty()) {
            events.push(ProviderStreamEvent::ReasoningDelta(rc.to_string()));
        }
        if let Some(tc) = delta["content"].as_str().filter(|s| !s.is_empty()) {
            acc.text.push_str(tc);
            events.push(ProviderStreamEvent::TextDelta(tc.to_string()));
        }
        if let Some(tcs) = delta["tool_calls"].as_array() {
            for tc in tcs {
                let idx = tc["index"].as_u64().unwrap_or(0) as usize;
                let id = tc["id"].as_str().map(|s| s.to_string());
                let name = tc["function"]["name"].as_str().map(|s| s.to_string());
                let args = tc["function"]["arguments"].as_str().unwrap_or("").to_string();
                let slot = match acc.calls.iter_mut().find(|c| c.index == idx) {
                    Some(c) => c,
                    None => {
                        acc.calls.push(StreamCallAccum {
                            index: idx,
                            id: None,
                            name: None,
                            args: String::new(),
                        });
                        acc.calls.last_mut().unwrap()
                    }
                };
                if id.is_some() {
                    slot.id = id.clone();
                }
                if name.is_some() {
                    slot.name = name.clone();
                }
                slot.args.push_str(&args);
                events.push(ProviderStreamEvent::ToolCallDelta {
                    index: idx,
                    id,
                    name,
                    args_delta: args,
                });
            }
        }
    }
    events
}

#[cfg(test)]
mod sse_tests {
    use super::*;

    fn chunk(delta: serde_json::Value) -> serde_json::Value {
        serde_json::json!({ "choices": [{ "index": 0, "delta": delta }] })
    }

    #[test]
    fn stream_text_and_reasoning_split() {
        let mut acc = StreamAccum::default();
        let evs = apply_stream_chunk(
            &chunk(serde_json::json!({"role":"assistant","reasoning_content":"think"})),
            &mut acc,
        );
        assert_eq!(evs.len(), 1);
        assert!(matches!(&evs[0], ProviderStreamEvent::ReasoningDelta(s) if s == "think"));
        let evs = apply_stream_chunk(&chunk(serde_json::json!({"content":"你好"})), &mut acc);
        assert_eq!(evs.len(), 1);
        assert!(matches!(&evs[0], ProviderStreamEvent::TextDelta(s) if s == "你好"));
        assert_eq!(acc.text, "你好");
        // finish_reason 帧无 delta 内容 → 零事件
        let evs = apply_stream_chunk(
            &chunk(serde_json::json!({"finish_reason":"stop"})),
            &mut acc,
        );
        assert!(evs.is_empty());
    }

    #[test]
    fn stream_tool_call_fragments_bucketed_by_index() {
        let mut acc = StreamAccum::default();
        apply_stream_chunk(
            &chunk(serde_json::json!({"tool_calls":[{"index":0,"id":"call_1","function":{"name":"calc","arguments":"{\"expr\":"}}]})),
            &mut acc,
        );
        apply_stream_chunk(
            &chunk(serde_json::json!({"tool_calls":[{"index":0,"function":{"arguments":"\"2+3\"}"}}]})),
            &mut acc,
        );
        assert_eq!(acc.calls.len(), 1);
        assert_eq!(acc.calls[0].id.as_deref(), Some("call_1"));
        assert_eq!(acc.calls[0].name.as_deref(), Some("calc"));
        assert_eq!(acc.calls[0].args, "{\"expr\":\"2+3\"}");
    }

    #[test]
    fn stream_usage_final_chunk() {
        let mut acc = StreamAccum::default();
        apply_stream_chunk(
            &serde_json::json!({ "choices": [], "usage": {"prompt_tokens": 19, "completion_tokens": 50, "total_tokens": 69} }),
            &mut acc,
        );
        assert_eq!(
            acc.usage,
            Some(Usage { prompt_tokens: 19, completion_tokens: 50, total_tokens: 69 }),
        );
    }
}

/// spec 164:ChatRequest → OpenAI messages(含 tool_calls/tool 结果消息)
fn messages_json(req: &ChatRequest) -> Vec<serde_json::Value> {
    let mut messages = Vec::new();
    if let Some(sys) = &req.system {
        messages.push(serde_json::json!({ "role": "system", "content": sys }));
    }
    for m in &req.messages {
        match m.role {
            ChatRole::User => messages.push(serde_json::json!({
                "role": "user",
                "content": m.content,
            })),
            ChatRole::Assistant => {
                let mut msg = serde_json::json!({ "role": "assistant", "content": m.content });
                if !m.tool_calls.is_empty() {
                    msg["tool_calls"] = serde_json::Value::Array(
                        m.tool_calls
                            .iter()
                            .map(|tc| {
                                serde_json::json!({
                                    "id": tc.id,
                                    "type": "function",
                                    "function": {
                                        "name": tc.name,
                                        "arguments": tc.arguments,
                                    },
                                })
                            })
                            .collect(),
                    );
                }
                messages.push(msg);
            }
            ChatRole::Tool => messages.push(serde_json::json!({
                "role": "tool",
                "content": m.content,
                "tool_call_id": m.tool_call_id.clone().unwrap_or_default(),
            })),
        }
    }
    messages
}

fn classify_ureq(e: ureq::Error) -> ProviderError {
    match e {
        // ureq 把 4xx/5xx 作为 Error 返回:统一转成带状态的错误
        ureq::Error::Status(code, resp) => {
            let summary = resp
                .into_string()
                .map(|s| truncate(&s, 200))
                .unwrap_or_else(|_| "<unreadable body>".into());
            if code == 429 || code >= 500 {
                ProviderError::Transient(format!("http {code}: {summary}"))
            } else {
                ProviderError::Rejected {
                    status: code,
                    summary,
                }
            }
        }
        other => {
            // ureq Transport Display 会叠加多层 "Network Error: " 前缀
            // (spec 152:归一为单个 network: 前缀,去掉重复段)
            // ureq Transport Display 形如
            // "{url}: Network Error: Network Error: {os/os error 文本}",
            // "Network Error" 段与我们的 network: 前缀语义重复,整体移除
            let msg = other.to_string().replace("Network Error: ", "");
            ProviderError::Transient(format!("network: {msg}"))
        }
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let cut: String = s.chars().take(max).collect();
        format!("{cut}…")
    }
}

#[derive(Deserialize)]
struct ChatCompletionsResponse {
    choices: Vec<ChatChoice>,
    usage: Option<ApiUsage>,
}

#[derive(Deserialize)]
struct ChatChoice {
    message: ApiResponseMessage,
}

#[derive(Deserialize)]
struct ApiResponseMessage {
    #[serde(default)]
    content: String,
    #[serde(default)]
    tool_calls: Vec<ApiToolCall>,
}

#[derive(Deserialize)]
struct ApiToolCall {
    #[serde(default)]
    id: String,
    function: ApiFunction,
}

#[derive(Deserialize)]
struct ApiFunction {
    #[serde(default)]
    name: String,
    #[serde(default)]
    arguments: String,
}

#[derive(Deserialize)]
struct ApiUsage {
    prompt_tokens: u64,
    completion_tokens: u64,
    total_tokens: u64,
}

impl ChatCompletionsResponse {
    fn into_response(self) -> Result<ChatResponse, ProviderError> {
        self.choices
            .into_iter()
            .next()
            .map(|c| ChatResponse {
                text: c.message.content,
                tool_calls: c
                    .message
                    .tool_calls
                    .into_iter()
                    .map(|tc| ToolCallInfo {
                        id: tc.id,
                        name: tc.function.name,
                        arguments: tc.function.arguments,
                    })
                    .collect(),
                usage: self.usage.map(|u| Usage {
                    prompt_tokens: u.prompt_tokens,
                    completion_tokens: u.completion_tokens,
                    total_tokens: u.total_tokens,
                }),
            })
            .ok_or_else(|| ProviderError::Rejected {
                status: 200,
                summary: "empty choices".into(),
            })
    }
}

/// 确定性 Mock:按脚本依次返回;用于编排与验收测试(spec 50 Provider trait 替身)。
pub struct MockProvider {
    /// 每个元素:Ok(text) 或 Err(描述);耗尽后重复最后一项
    pub script: std::sync::Mutex<Vec<Result<String, String>>>,
    /// spec 164:结构化脚本(scripted_chat 用;None = 纯文本模式)
    pub script_chat: std::sync::Mutex<Option<Vec<Result<ChatResponse, String>>>>,
    pub calls: std::sync::atomic::AtomicU32,
    /// 收到的请求记录(断言上下文顺序/内容用)
    pub prompts: std::sync::Mutex<Vec<ChatRequest>>,
}

impl MockProvider {
    pub fn ok(text: &str) -> Self {
        Self {
            script: std::sync::Mutex::new(vec![Ok(text.to_string())]),
            script_chat: std::sync::Mutex::new(None),
            calls: Default::default(),
            prompts: std::sync::Mutex::new(Vec::new()),
        }
    }

    pub fn scripted(script: Vec<Result<String, String>>) -> Self {
        Self {
            script: std::sync::Mutex::new(script),
            script_chat: std::sync::Mutex::new(None),
            calls: Default::default(),
            prompts: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// spec 164:结构化脚本(可携带 tool_calls/usage),native 循环测试用;
    /// 存在时优先于文本脚本消费
    pub fn scripted_chat(script: Vec<Result<ChatResponse, String>>) -> Self {
        Self {
            script: std::sync::Mutex::new(Vec::new()),
            script_chat: std::sync::Mutex::new(Some(script)),
            calls: Default::default(),
            prompts: std::sync::Mutex::new(Vec::new()),
        }
    }
}

impl Provider for MockProvider {
    fn complete(&self, _model: &str, req: &ChatRequest) -> Result<ChatResponse, ProviderError> {
        self.calls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.prompts.lock().unwrap().push(req.clone());
        // spec 164:结构化脚本优先
        {
            let mut chat = self.script_chat.lock().unwrap();
            if let Some(q) = chat.as_mut() {
                let item = if q.is_empty() {
                    return Err(ProviderError::Exhausted {
                        attempts: 1,
                        last: "mock script exhausted".into(),
                    });
                } else if q.len() == 1 {
                    q[0].clone()
                } else {
                    q.remove(0)
                };
                return item
                    .map_err(|last| ProviderError::Exhausted { attempts: 1, last });
            }
        }
        let mut script = self.script.lock().unwrap();
        let item = if script.is_empty() {
            return Err(ProviderError::Exhausted {
                attempts: 1,
                last: "mock script exhausted".into(),
            });
        } else if script.len() == 1 {
            script[0].clone()
        } else {
            script.remove(0)
        };
        item
            .map(|text| ChatResponse { text, tool_calls: Vec::new(), usage: None })
            .map_err(|last| ProviderError::Exhausted { attempts: 1, last })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::ENV_LOCK;
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::time::Duration;

    /// 就绪屏障:部分环境(如杀软对新监听端口)会在短暂窗口内丢弃 SYN,
    /// 造成 loopback connect 偶发超时。在正式调用前用可重试的探测连接
    /// 穿过该窗口;探测连接由测试服务线程的正常 accept 循环消化。
    fn wait_listening(addr: std::net::SocketAddr) {
        for _ in 0..50 {
            if TcpStream::connect(addr).is_ok() {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("test server never became reachable at {addr}");
    }

    /// 全测试进程共享一个 mock endpoint:端口只创建一次(规避杀软对多个
    /// 新端口的干扰窗口),按请求路径路由行为,各测试互不共享可变脚本。
    fn shared_server() -> std::net::SocketAddr {
        use std::sync::atomic::{AtomicU32, Ordering};
        static SERVER: std::sync::OnceLock<std::net::SocketAddr> = std::sync::OnceLock::new();
        static RETRY_HITS: AtomicU32 = AtomicU32::new(0);
        *SERVER.get_or_init(|| {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            std::thread::spawn(move || loop {
                let Ok((mut stream, _)) = listener.accept() else { break };
                let mut buf = [0u8; 8192];
                let _ = stream.read(&mut buf);
                let first_line = String::from_utf8_lossy(&buf);
                let path = first_line.split_whitespace().nth(1).unwrap_or("");
                let resp = if path.starts_with("/retry") {
                    // 前两次命中 500,之后 200(A6:5xx 重试路径)
                    let hit = RETRY_HITS.fetch_add(1, Ordering::Relaxed);
                    if hit < 2 {
                        "HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string()
                    } else {
                        let body = serde_json::json!({
                            "choices": [{ "message": { "content": "hello from server" } }]
                        })
                        .to_string();
                        format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body)
                    }
                } else if path.starts_with("/always500") {
                    // spec 152:恒 500(retry exhausted 错误形态断言)
                    "HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string()
                } else if path.starts_with("/hang") {
                    // spec 152:客户端超时路径。在独立线程延迟响应,
                    // 不阻塞共享 accept 循环(否则并行测试被排队超时)。
                    std::thread::spawn(move || {
                        std::thread::sleep(Duration::from_secs(3));
                        let resp = "HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
                        let _ = stream.write_all(resp.as_bytes());
                        let _ = stream.flush();
                    });
                    continue;
                } else if path.starts_with("/badjson") {
                    // spec 152:200 + 非法 JSON(Rejected 不重试断言)
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 14\r\nConnection: close\r\n\r\nnot json {{{\r\n".to_string()
                } else {
                    // /reject 及未知路径:恒 401(A6:4xx 不重试路径)
                    "HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string()
                };
                let _ = stream.write_all(resp.as_bytes());
                let _ = stream.flush();
                // 优雅关闭:带未读数据直接 drop 会触发 RST(os 10054),
                // 客户端偶发丢响应。先关发送方向,再把对端剩余字节读净。
                let _ = stream.shutdown(std::net::Shutdown::Write);
                let mut sink = [0u8; 4096];
                while let Ok(n) = stream.read(&mut sink) {
                    if n == 0 {
                        break;
                    }
                }
            });
            addr
        })
    }

    fn zero_backoff_policy() -> RetryPolicy {
        RetryPolicy {
            max_retries: 2,
            backoff: Box::new(|_| Duration::ZERO),
        }
    }

    #[test]
    fn config_resolution_env_overrides_file_and_validates() {
        let _env = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // 无配置 → Invalid(可解释)
        for k in [
            "ZEALOUS_BASE_URL",
            "ZEALOUS_API_KEY",
            "ZEALOUS_MODEL",
            "ZEALOUS_TIMEOUT_SECS",
            "ZEALOUS_MAX_RETRIES",
        ] {
            std::env::remove_var(k);
        }
        // resolve_from(None) 隔离本机真实 ~/.zealous/config.json
        let err = ProviderConfig::resolve_from(None).unwrap_err();
        assert!(matches!(err, ProviderError::Invalid(_)));

        // env 齐全 → 成功(与配置文件无关)
        std::env::set_var("ZEALOUS_BASE_URL", "https://api.example.invalid/v1");
        std::env::set_var("ZEALOUS_API_KEY", "test-key");
        std::env::set_var("ZEALOUS_MODEL", "test-model");
        let cfg = ProviderConfig::resolve_from(None).unwrap();
        assert_eq!(cfg.model, "test-model");
        assert_eq!(cfg.max_retries, 3);
        for k in ["ZEALOUS_BASE_URL", "ZEALOUS_API_KEY", "ZEALOUS_MODEL"] {
            std::env::remove_var(k);
        }
    }

    #[test]
    fn openai_mapping_preserves_message_order() {
        // spec 70:多轮上下文原生传递——system 在前,历史 user/assistant 按序透传
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let captured: Arc<std::sync::Mutex<String>> =
            Arc::new(std::sync::Mutex::new(String::new()));
        let captured2 = captured.clone();
        let server = std::thread::spawn(move || loop {
            let Ok((mut stream, _)) = listener.accept() else {
                break;
            };
            // 读完完整请求(头部 + Content-Length 体),单次 read 可能截断
            let mut raw: Vec<u8> = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                let n = match stream.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                raw.extend_from_slice(&chunk[..n]);
                let text = String::from_utf8_lossy(&raw);
                if let Some(header_end) = text.find("\r\n\r\n") {
                    let len = text
                        .split("\r\n")
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .and_then(|v| v.trim().parse::<usize>().ok())
                        })
                        .unwrap_or(0);
                    if raw.len() >= header_end + 4 + len {
                        break;
                    }
                }
            }
            let text = String::from_utf8_lossy(&raw).to_string();
            // 取请求体(空行之后)
            let body = text.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
            *captured2.lock().unwrap() = body;
            let resp = serde_json::json!({
                "choices": [{ "message": { "content": "ok" } }]
            })
            .to_string();
            let _ = stream.write_all(
                format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", resp.len(), resp).as_bytes(),
            );
        });

        let cfg = ProviderConfig {
            base_url: format!("http://{addr}/v1"),
            api_key: "k".into(),
            model: "m".into(),
            timeout_secs: 5,
            max_retries: 0,
        };
        let provider = OpenAiCompatProvider::new(cfg);
        let req = ChatRequest {
            tools: Vec::new(),
            system: Some("SYS".into()),
            messages: vec![
                ChatMessage::user("u1"),
                ChatMessage::assistant("a1"),
                ChatMessage::user("u2"),
            ],
        };
        provider.complete("m", &req).unwrap();

        let body = captured.lock().unwrap().clone();
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        let msgs = v["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 4);
        assert_eq!(msgs[0]["role"], "system");
        assert_eq!(msgs[0]["content"], "SYS");
        assert_eq!(msgs[1]["role"], "user");
        assert_eq!(msgs[1]["content"], "u1");
        assert_eq!(msgs[2]["role"], "assistant");
        assert_eq!(msgs[2]["content"], "a1");
        assert_eq!(msgs[3]["role"], "user");
        assert_eq!(msgs[3]["content"], "u2");
        assert_eq!(v["model"], "m");
        drop(server);
    }

    #[test]
    fn retries_on_5xx_then_succeeds_against_local_server() {
        let addr = shared_server();
        wait_listening(addr);
        let cfg = ProviderConfig {
            base_url: format!("http://{addr}/retry"),
            api_key: "k".into(),
            model: "m".into(),
            timeout_secs: 5,
            max_retries: 2,
        };
        let provider = OpenAiCompatProvider::new(cfg).with_retry(zero_backoff_policy());
        let resp = provider
            .complete(
                "m",
                &ChatRequest {
                    tools: Vec::new(),
                    system: None,
                    messages: vec![ChatMessage::user("hi")],
                },
            )
            .unwrap();
        assert_eq!(resp.text, "hello from server");
        // 两次 5xx + 至少一次成功;偶发网络抖动可能多消耗一次重试,语义不变
        let attempts = provider.attempts.load(std::sync::atomic::Ordering::Relaxed);
        assert!((3..=4).contains(&attempts), "attempts={attempts}");
    }

    #[test]
    fn client_error_4xx_fails_fast_without_retry() {
        let addr = shared_server();
        wait_listening(addr);
        let cfg = ProviderConfig {
            base_url: format!("http://{addr}/reject"),
            api_key: "k".into(),
            model: "m".into(),
            timeout_secs: 5,
            max_retries: 2,
        };
        let provider = OpenAiCompatProvider::new(cfg).with_retry(zero_backoff_policy());
        let err = provider
            .complete(
                "m",
                &ChatRequest {
                    tools: Vec::new(),
                    system: None,
                    messages: vec![ChatMessage::user("hi")],
                },
            )
            .unwrap_err();
        // 关键断言:4xx 不重试,单次调用即以 Rejected 失败
        assert!(
            matches!(err, ProviderError::Rejected { status: 401, .. }),
            "{err:?}"
        );
    }

    #[test]
    fn mock_provider_script_is_consumed_in_order() {
        let mock = MockProvider::scripted(vec![Ok("first".into()), Err("boom".into())]);
        let r1 = mock
            .complete(
                "m",
                &ChatRequest {
                    tools: Vec::new(),
                    system: None,
                    messages: vec![ChatMessage::user("u")],
                },
            )
            .unwrap();
        assert_eq!(r1.text, "first");
        assert!(mock
            .complete(
                "m",
                &ChatRequest {
                    tools: Vec::new(),
                    system: None,
                    messages: vec![ChatMessage::user("u")]
                }
            )
            .is_err());
        // 耗尽后重复最后一项
        assert!(mock
            .complete(
                "m",
                &ChatRequest {
                    tools: Vec::new(),
                    system: None,
                    messages: vec![ChatMessage::user("u")]
                }
            )
            .is_err());
        assert_eq!(mock.calls.load(std::sync::atomic::Ordering::Relaxed), 3);
    }

    // ---------- spec 152:错误去嵌套 ----------

    fn single_line_count(s: &str, needle: &str) -> usize {
        s.matches(needle).count()
    }

    #[test]
    fn http_500_retry_exhausted_error_is_single_layer() {
        let addr = shared_server();
        wait_listening(addr);
        let cfg = ProviderConfig {
            base_url: format!("http://{addr}/always500"),
            api_key: "k".into(),
            model: "m".into(),
            timeout_secs: 5,
            max_retries: 1,
        };
        let provider = OpenAiCompatProvider::new(cfg).with_retry(zero_backoff_policy());
        let err = provider
            .complete(
                "m",
                &ChatRequest {
                    tools: Vec::new(),
                    system: None,
                    messages: vec![ChatMessage::user("hi")],
                },
            )
            .unwrap_err();
        let text = err.to_string();
        // 单层:exhausted after 只出现一次,且直接跟底层 http 原因
        assert_eq!(
            single_line_count(&text, "exhausted after"),
            1,
            "nested exhausted: {text}"
        );
        assert!(
            text.starts_with("provider_exhausted after ") && text.contains(": http 500:"),
            "{text}"
        );
        assert!(!text.contains("provider_exhausted after 0"), "{text}");
    }

    #[test]
    fn timeout_retry_error_mentions_network_error_at_most_once() {
        let addr = shared_server();
        wait_listening(addr);
        let cfg = ProviderConfig {
            base_url: format!("http://{addr}/hang"),
            api_key: "k".into(),
            model: "m".into(),
            timeout_secs: 1,
            max_retries: 1,
        };
        let provider = OpenAiCompatProvider::new(cfg).with_retry(zero_backoff_policy());
        let err = provider
            .complete(
                "m",
                &ChatRequest {
                    tools: Vec::new(),
                    system: None,
                    messages: vec![ChatMessage::user("hi")],
                },
            )
            .unwrap_err();
        let text = err.to_string();
        assert_eq!(
            single_line_count(&text, "exhausted after"),
            1,
            "nested exhausted: {text}"
        );
        assert!(
            single_line_count(&text, "Network Error") <= 1,
            "repeated Network Error: {text}"
        );
        assert!(text.contains("network:"), "{text}");
    }

    #[test]
    fn bad_json_200_is_rejected_without_retry() {
        let addr = shared_server();
        wait_listening(addr);
        let cfg = ProviderConfig {
            base_url: format!("http://{addr}/badjson"),
            api_key: "k".into(),
            model: "m".into(),
            timeout_secs: 5,
            max_retries: 2,
        };
        let provider = OpenAiCompatProvider::new(cfg).with_retry(zero_backoff_policy());
        let err = provider
            .complete(
                "m",
                &ChatRequest {
                    tools: Vec::new(),
                    system: None,
                    messages: vec![ChatMessage::user("hi")],
                },
            )
            .unwrap_err();
        match &err {
            ProviderError::Rejected {
                status: 200,
                summary,
            } => {
                assert!(summary.starts_with("bad json"), "{summary}");
            }
            other => panic!("expected Rejected 200/bad json, got {other:?}"),
        }
        // 不可重试:单次调用即失败
        assert_eq!(
            provider.attempts.load(std::sync::atomic::Ordering::Relaxed),
            1
        );
    }
}
