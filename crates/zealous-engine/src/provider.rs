//! Provider 抽象与 OpenAI-compatible 实现(spec 10)。
//!
//! 失败语义(A6):配置缺失/非法 → `Invalid`,创建任务即失败并落账;
//! 网络/5xx/429 → 指数退避重试,耗尽 → 可解释错误;4xx(除 429)不重试。
//! `api_key` 只存在于 Provider 内部,禁止进入账本与日志(spec 20)。

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// 对话角色:V1 只需要 user/assistant(system 单独字段承载)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatRole {
    User,
    Assistant,
}

/// 一条历史消息(spec 70:多轮上下文原生传递,不再拼接进单个字符串)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatMessage {
    pub role: ChatRole,
    pub content: String,
}

impl ChatMessage {
    pub fn user(content: impl Into<String>) -> Self {
        ChatMessage {
            role: ChatRole::User,
            content: content.into(),
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        ChatMessage {
            role: ChatRole::Assistant,
            content: content.into(),
        }
    }
}

/// 单次模型调用请求:system + 历史消息序列(顺序即语义)
#[derive(Debug, Clone)]
pub struct ChatRequest {
    pub system: Option<String>,
    pub messages: Vec<ChatMessage>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatResponse {
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderError {
    /// 配置缺失/非法——创建任务即失败的原因(A6)
    Invalid(String),
    /// 重试耗尽仍失败,携带最后一次可解释原因
    Exhausted { attempts: u32, last: String },
    /// 不可重试的 4xx
    Rejected { status: u16, summary: String },
}

impl std::fmt::Display for ProviderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProviderError::Invalid(r) => write!(f, "provider_config_invalid: {r}"),
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

/// 解析后的配置。`Display` 刻意不输出 key。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
        let from_file = Self::from_config_file();
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

    fn from_config_file() -> Option<Self> {
        let path = Self::config_path()?;
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
        let mut messages = Vec::new();
        if let Some(sys) = &req.system {
            messages.push(serde_json::json!({ "role": "system", "content": sys }));
        }
        for m in &req.messages {
            let role = match m.role {
                ChatRole::User => "user",
                ChatRole::Assistant => "assistant",
            };
            messages.push(serde_json::json!({ "role": role, "content": m.content }));
        }

        let body = serde_json::json!({
            "model": model,
            "messages": messages,
        })
        .to_string();

        let resp = self
            .agent
            .post(&self.endpoint())
            .set("Authorization", &format!("Bearer {}", self.config.api_key))
            .set("Content-Type", "application/json")
            .send_string(&body)
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
}

impl Provider for OpenAiCompatProvider {
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

fn classify_ureq(e: ureq::Error) -> ProviderError {
    match e {
        // ureq 把 4xx/5xx 作为 Error 返回:统一转成带状态的错误
        ureq::Error::Status(code, resp) => {
            let summary = resp
                .into_string()
                .map(|s| truncate(&s, 200))
                .unwrap_or_else(|_| "<unreadable body>".into());
            if code == 429 || code >= 500 {
                ProviderError::Exhausted {
                    attempts: 0,
                    last: format!("http {code}: {summary}"),
                }
            } else {
                ProviderError::Rejected {
                    status: code,
                    summary,
                }
            }
        }
        other => ProviderError::Exhausted {
            attempts: 0,
            last: format!("network: {other}"),
        },
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
}

#[derive(Deserialize)]
struct ChatChoice {
    message: ApiResponseMessage,
}

#[derive(Deserialize)]
struct ApiResponseMessage {
    content: String,
}

impl ChatCompletionsResponse {
    fn into_response(self) -> Result<ChatResponse, ProviderError> {
        self.choices
            .into_iter()
            .next()
            .map(|c| ChatResponse {
                text: c.message.content,
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
    pub calls: std::sync::atomic::AtomicU32,
    /// 收到的请求记录(断言上下文顺序/内容用)
    pub prompts: std::sync::Mutex<Vec<ChatRequest>>,
}

impl MockProvider {
    pub fn ok(text: &str) -> Self {
        Self {
            script: std::sync::Mutex::new(vec![Ok(text.to_string())]),
            calls: Default::default(),
            prompts: std::sync::Mutex::new(Vec::new()),
        }
    }

    pub fn scripted(script: Vec<Result<String, String>>) -> Self {
        Self {
            script: std::sync::Mutex::new(script),
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
        item.map(|text| ChatResponse { text })
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
        let _env = ENV_LOCK.lock().unwrap();
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
        let err = ProviderConfig::resolve().unwrap_err();
        assert!(matches!(err, ProviderError::Invalid(_)));

        // env 齐全 → 成功
        std::env::set_var("ZEALOUS_BASE_URL", "https://api.example.com/v1");
        std::env::set_var("ZEALOUS_API_KEY", "test-key");
        std::env::set_var("ZEALOUS_MODEL", "test-model");
        let cfg = ProviderConfig::resolve().unwrap();
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
                    system: None,
                    messages: vec![ChatMessage::user("u")]
                }
            )
            .is_err());
        assert_eq!(mock.calls.load(std::sync::atomic::Ordering::Relaxed), 3);
    }
}
