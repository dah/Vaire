use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;

/// JSON-RPC request identifiers accepted by the generated Codex schema.
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(untagged)]
pub enum RequestId {
    Number(u64),
    String(String),
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct RpcErrorObject {
    pub code: i64,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum InboundMessage {
    Response {
        id: RequestId,
        result: Result<Value, RpcErrorObject>,
    },
    Notification {
        method: String,
        params: Value,
    },
    ServerRequest {
        id: RequestId,
        method: String,
        params: Value,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum InboundEvent {
    Notification { method: String, params: Value },
    SafetyViolation { id: RequestId, method: String },
    ConnectionClosed { category: String },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientInfo {
    pub name: String,
    pub title: String,
    pub version: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeCapabilities {
    pub experimental_api: bool,
    pub mcp_server_openai_form_elicitation: bool,
    pub request_attestation: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeParams {
    pub client_info: ClientInfo,
    pub capabilities: InitializeCapabilities,
}

impl InitializeParams {
    pub fn agentharness() -> Self {
        Self {
            client_info: ClientInfo {
                name: "agentharness".to_owned(),
                title: "AgentHarness".to_owned(),
                version: env!("CARGO_PKG_VERSION").to_owned(),
            },
            capabilities: InitializeCapabilities::default(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResponse {
    pub codex_home: PathBuf,
    pub platform_family: String,
    pub platform_os: String,
    pub user_agent: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct AccountInfo {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub email: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AccountReadResponse {
    pub account: Option<AccountInfo>,
    pub requires_openai_auth: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct LoginAccountParams {
    #[serde(rename = "type")]
    pub kind: &'static str,
}

impl LoginAccountParams {
    pub fn chatgpt() -> Self {
        Self { kind: "chatgpt" }
    }

    pub fn chatgpt_device_code() -> Self {
        Self {
            kind: "chatgptDeviceCode",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LoginAccountResponse {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub auth_url: Option<String>,
    #[serde(default)]
    pub login_id: Option<String>,
    #[serde(default)]
    pub verification_url: Option<String>,
    #[serde(default)]
    pub user_code: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CancelLoginAccountParams {
    pub login_id: String,
}

impl CancelLoginAccountParams {
    pub fn new(login_id: impl Into<String>) -> Self {
        Self {
            login_id: login_id.into(),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum CancelLoginAccountStatus {
    Canceled,
    NotFound,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct CancelLoginAccountResponse {
    pub status: CancelLoginAccountStatus,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AccountLoginCompletedNotification {
    pub success: bool,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub login_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ReasoningEffortOption {
    pub reasoning_effort: String,
    pub description: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ModelInfo {
    pub id: String,
    pub display_name: String,
    pub is_default: bool,
    pub default_reasoning_effort: String,
    pub supported_reasoning_efforts: Vec<ReasoningEffortOption>,
    #[serde(default)]
    pub hidden: bool,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelListParams {
    pub cursor: Option<String>,
    pub include_hidden: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ModelListResponse {
    pub data: Vec<ModelInfo>,
    pub next_cursor: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum TurnStatus {
    Completed,
    Interrupted,
    Failed,
    InProgress,
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct TurnError {
    pub message: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct UserInput {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub text: Option<String>,
}

impl UserInput {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            kind: "text".to_owned(),
            text: Some(text.into()),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct ThreadItem {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub content: Vec<UserInput>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct TurnSnapshot {
    pub id: String,
    #[serde(default)]
    pub items: Vec<ThreadItem>,
    pub status: TurnStatus,
    #[serde(default)]
    pub error: Option<TurnError>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct ThreadSnapshot {
    pub id: String,
    #[serde(default)]
    pub turns: Vec<TurnSnapshot>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadStartParams {
    pub approval_policy: String,
    pub config: Value,
    pub cwd: PathBuf,
    pub sandbox: String,
    pub model: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadResumeParams {
    pub thread_id: String,
    pub approval_policy: String,
    pub config: Value,
    pub cwd: PathBuf,
    pub sandbox: String,
    pub model: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadReadParams {
    pub thread_id: String,
    pub include_turns: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct ThreadResponse {
    pub thread: ThreadSnapshot,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnStartParams {
    pub thread_id: String,
    pub input: Vec<UserInput>,
    pub model: String,
    pub effort: String,
    pub approval_policy: String,
    pub cwd: PathBuf,
    pub sandbox_policy: Value,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct TurnStartResponse {
    pub turn: TurnSnapshot,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnInterruptParams {
    pub thread_id: String,
    pub turn_id: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AgentMessageDeltaNotification {
    pub thread_id: String,
    pub turn_id: String,
    pub item_id: String,
    pub delta: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ItemCompletedNotification {
    pub thread_id: String,
    pub turn_id: String,
    pub item: ThreadItem,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TurnNotification {
    pub thread_id: String,
    pub turn: TurnSnapshot,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ErrorNotification {
    pub thread_id: String,
    pub turn_id: String,
    pub error: TurnError,
    pub will_retry: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProtocolEvent {
    AccountLoginCompleted(AccountLoginCompletedNotification),
    AccountUpdated,
    ThreadStarted(ThreadSnapshot),
    TurnStarted(TurnNotification),
    AgentMessageDelta(AgentMessageDeltaNotification),
    ItemCompleted(ItemCompletedNotification),
    TurnCompleted(TurnNotification),
    Error(ErrorNotification),
}

pub fn parse_notification(method: &str, params: Value) -> Result<Option<ProtocolEvent>, String> {
    fn decode<T: for<'de> Deserialize<'de>>(method: &str, params: Value) -> Result<T, String> {
        serde_json::from_value(params)
            .map_err(|_| format!("{method} notification had invalid required fields"))
    }

    let event = match method {
        "account/login/completed" => ProtocolEvent::AccountLoginCompleted(decode(method, params)?),
        "account/updated" => ProtocolEvent::AccountUpdated,
        "thread/started" => {
            #[derive(Deserialize)]
            struct Body {
                thread: ThreadSnapshot,
            }
            ProtocolEvent::ThreadStarted(decode::<Body>(method, params)?.thread)
        }
        "turn/started" => ProtocolEvent::TurnStarted(decode(method, params)?),
        "item/agentMessage/delta" => ProtocolEvent::AgentMessageDelta(decode(method, params)?),
        "item/completed" => ProtocolEvent::ItemCompleted(decode(method, params)?),
        "turn/completed" => ProtocolEvent::TurnCompleted(decode(method, params)?),
        "error" => ProtocolEvent::Error(decode(method, params)?),
        _ => return Ok(None),
    };
    Ok(Some(event))
}

pub fn classify_message(value: Value) -> Result<InboundMessage, String> {
    let object = value
        .as_object()
        .ok_or_else(|| "JSON-RPC frame must be an object".to_owned())?;

    let id = object
        .get("id")
        .cloned()
        .map(serde_json::from_value::<RequestId>)
        .transpose()
        .map_err(|_| "JSON-RPC id must be a non-negative integer or string".to_owned())?;
    let method = object.get("method").and_then(Value::as_str);

    match (id, method) {
        (Some(id), Some(method)) => Ok(InboundMessage::ServerRequest {
            id,
            method: method.to_owned(),
            params: object.get("params").cloned().unwrap_or(Value::Null),
        }),
        (None, Some(method)) => Ok(InboundMessage::Notification {
            method: method.to_owned(),
            params: object.get("params").cloned().unwrap_or(Value::Null),
        }),
        (Some(id), None) => {
            let result = match (object.get("result"), object.get("error")) {
                (Some(result), None) => Ok(result.clone()),
                (None, Some(error)) => serde_json::from_value::<RpcErrorObject>(error.clone())
                    .map(Err)
                    .map_err(|_| "invalid JSON-RPC error object".to_owned())?,
                _ => {
                    return Err(
                        "JSON-RPC response must contain exactly one of result or error".to_owned(),
                    )
                }
            };
            Ok(InboundMessage::Response { id, result })
        }
        (None, None) => Err("JSON-RPC frame has neither id nor method".to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{
        classify_message, parse_notification, CancelLoginAccountParams, CancelLoginAccountResponse,
        CancelLoginAccountStatus, InboundMessage, InitializeParams, LoginAccountParams,
        ProtocolEvent, RequestId,
    };

    #[test]
    fn initializes_without_experimental_capabilities() {
        let params = serde_json::to_value(InitializeParams::agentharness()).unwrap();
        assert_eq!(params["capabilities"]["experimentalApi"], false);
        assert_eq!(
            params["capabilities"]["mcpServerOpenaiFormElicitation"],
            false
        );
        assert_eq!(params["capabilities"]["requestAttestation"], false);
    }

    #[test]
    fn models_login_cancellation_from_the_installed_schema() {
        let params = serde_json::to_value(CancelLoginAccountParams::new("login-active")).unwrap();
        assert_eq!(params, json!({"loginId": "login-active"}));

        let response: CancelLoginAccountResponse =
            serde_json::from_value(json!({"status": "notFound"})).unwrap();
        assert_eq!(response.status, CancelLoginAccountStatus::NotFound);

        let device = serde_json::to_value(LoginAccountParams::chatgpt_device_code()).unwrap();
        assert_eq!(device, json!({"type": "chatgptDeviceCode"}));
    }

    #[test]
    fn separates_responses_notifications_and_server_requests() {
        assert!(matches!(
            classify_message(json!({"id": 1, "result": {"ok": true}})).unwrap(),
            InboundMessage::Response {
                id: RequestId::Number(1),
                result: Ok(_)
            }
        ));
        assert!(matches!(
            classify_message(json!({"method": "thread/started", "params": {}})).unwrap(),
            InboundMessage::Notification { .. }
        ));
        assert!(matches!(
            classify_message(json!({"id": "s1", "method": "item/tool/call", "params": {}}))
                .unwrap(),
            InboundMessage::ServerRequest { .. }
        ));
    }

    #[test]
    fn rejects_ambiguous_responses() {
        let error = classify_message(json!({"id": 1, "result": {}, "error": {}})).unwrap_err();
        assert!(error.contains("exactly one"));
    }

    #[test]
    fn decodes_required_stream_scope_and_tolerates_unknown_notifications() {
        let event = parse_notification(
            "item/agentMessage/delta",
            json!({"threadId":"thr","turnId":"turn","itemId":"item","delta":"hi"}),
        )
        .unwrap();
        assert!(
            matches!(event, Some(ProtocolEvent::AgentMessageDelta(delta)) if delta.item_id == "item")
        );
        assert_eq!(
            parse_notification("future/event", json!({"anything": true})).unwrap(),
            None
        );
        assert!(parse_notification("turn/completed", json!({"threadId":"thr"})).is_err());
    }
}
