//! app-server 协议类型(spec 100):JSON-lines,请求带 id,通知无 id。
//! 载荷复用 core 的事件与投影序列化形态,不创造专用概念。

use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u32 = 1;

/// stdin 单行请求:`{"id":1,"cmd":"get_task","params":{...}}`
#[derive(Debug, Clone, Deserialize)]
pub struct Request {
    pub id: u64,
    pub cmd: String,
    #[serde(default)]
    pub params: serde_json::Value,
}

/// 成功响应
#[derive(Debug, Serialize)]
pub struct Response {
    pub id: u64,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ProtocolError>,
}

/// 错误响应
#[derive(Debug, Serialize)]
pub struct ErrorResponse {
    pub id: u64,
    pub ok: bool,
    pub error: ProtocolError,
}

#[derive(Debug, Serialize)]
pub struct ProtocolError {
    pub code: String,
    pub message: String,
}

impl ProtocolError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_string(),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl Response {
    pub fn ok(id: u64, result: serde_json::Value) -> Self {
        Self {
            id,
            ok: true,
            result: Some(result),
            error: None,
        }
    }
}

impl From<u64> for ErrorResponse {
    fn from(id: u64) -> Self {
        Self::from_error(id, ProtocolError::new("internal", "unknown"))
    }
}

impl ErrorResponse {
    pub fn from_error(id: u64, err: ProtocolError) -> Self {
        Self {
            id,
            ok: false,
            error: err,
        }
    }
}

/// stdout 通知(无 id):`{"event":"...","params":{...}}`
#[derive(Debug, Serialize)]
pub struct Notification {
    pub event: &'static str,
    pub params: serde_json::Value,
}

impl Notification {
    pub fn task_updated(task_id: &str, event: &zealous_core::TaskEvent) -> Self {
        Self {
            event: "task_updated",
            params: serde_json::json!({ "task_id": task_id, "event": event }),
        }
    }

    pub fn progress(level: &str, line: &str) -> Self {
        Self {
            event: "progress",
            params: serde_json::json!({ "level": level, "line": line }),
        }
    }

    pub fn approval_requested(
        task_id: &str,
        call_id: &str,
        tool: &str,
        risk: &str,
        summary: &str,
    ) -> Self {
        Self {
            event: "approval_requested",
            params: serde_json::json!({
                "task_id": task_id,
                "call_id": call_id,
                "tool": tool,
                "risk": risk,
                "summary": summary,
            }),
        }
    }

    pub fn run_finished(task_id: &str, status: &str) -> Self {
        Self {
            event: "run_finished",
            params: serde_json::json!({ "task_id": task_id, "status": status }),
        }
    }

    pub fn error_occurred(message: &str) -> Self {
        Self {
            event: "error_occurred",
            params: serde_json::json!({ "message": message }),
        }
    }
}

/// 解析一行请求;JSON/结构非法返回 parse_error
pub fn parse_request(line: &str) -> Result<Request, ProtocolError> {
    serde_json::from_str(line)
        .map_err(|e| ProtocolError::new("parse_error", format!("请求解析失败: {e}")))
}

/// 无法解析时的响应(id 未知,置 0)
pub fn parse_error_response(line: &str) -> Response {
    let er = ErrorResponse::from_error(
        0,
        ProtocolError::new(
            "parse_error",
            format!("无法解析请求行: {}", truncate(line, 200)),
        ),
    );
    Response {
        id: er.id,
        ok: false,
        result: None,
        error: Some(er.error),
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(max).collect::<String>())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_roundtrip() {
        let line = r#"{"id":7,"cmd":"get_task","params":{"task_id":"task_x"}}"#;
        let req = parse_request(line).unwrap();
        assert_eq!(req.id, 7);
        assert_eq!(req.cmd, "get_task");
        assert_eq!(req.params["task_id"], "task_x");
    }

    #[test]
    fn parse_error_is_structured() {
        let err = parse_request("not json");
        assert!(err.is_err());
        let resp = parse_error_response("not json");
        assert!(!resp.ok);
        let code = resp.error.as_ref().expect("error 字段应存在").code.clone();
        assert_eq!(code, "parse_error");
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains(r#""ok":false"#));
        assert!(json.contains(r#""id":0"#));
    }

    #[test]
    fn response_and_notification_json_shapes() {
        let resp = Response::ok(3, serde_json::json!({"status":"Completed"}));
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains(r#""id":3"#) && json.contains(r#""ok":true"#));

        let note =
            Notification::approval_requested("task_1", "call_1", "write_file", "T1", "write x");
        let json = serde_json::to_string(&note).unwrap();
        assert!(json.contains(r#""event":"approval_requested""#));
        // 通知不携带 id 字段(注意 call_id 尾部含 id 子串,须带引号匹配)
        assert!(!json.contains(r#""id""#), "{json}");
    }
}
