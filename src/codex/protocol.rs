use serde::de::{IgnoredAny, MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use std::path::PathBuf;

use crate::text::is_terminal_unsafe;

pub(super) const MAX_PROTOCOL_IDENTIFIER_BYTES: usize = 16 * 1024;

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

pub type LogoutAccountResponse = EmptyObjectResponse;

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

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(untagged)]
pub enum ThreadItemContent {
    UserInput(UserInput),
    Text(String),
    Other(Value),
}

impl From<UserInput> for ThreadItemContent {
    fn from(value: UserInput) -> Self {
        Self::UserInput(value)
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
    pub content: Vec<ThreadItemContent>,
    #[serde(default)]
    pub summary: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct TurnSnapshot {
    pub id: String,
    pub items: Vec<ThreadItem>,
    pub status: TurnStatus,
    #[serde(default)]
    pub error: Option<TurnError>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct ThreadSnapshot {
    pub id: String,
    pub turns: Vec<TurnSnapshot>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ThreadListEntry {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    pub preview: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub cwd: PathBuf,
    pub ephemeral: bool,
    pub source: SessionSource,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(untagged)]
pub enum SessionSource {
    Named(SessionSourceName),
    Custom {
        custom: String,
    },
    SubAgent {
        #[serde(rename = "subAgent")]
        sub_agent: Value,
    },
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum SessionSourceName {
    Cli,
    Vscode,
    Exec,
    AppServer,
    Unknown,
    #[serde(other)]
    Other,
}

impl SessionSource {
    pub(super) fn is_supported_resume_source(&self) -> bool {
        matches!(
            self,
            Self::Named(SessionSourceName::AppServer | SessionSourceName::Vscode)
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ThreadSourceKind {
    AppServer,
    Vscode,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadListParams {
    pub source_kinds: Vec<ThreadSourceKind>,
    pub archived: bool,
    pub cursor: Option<String>,
    pub cwd: PathBuf,
    pub limit: u32,
    pub sort_direction: String,
    pub sort_key: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ThreadListResponse {
    pub data: Vec<ThreadListEntry>,
    #[serde(default)]
    pub next_cursor: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadDeleteParams {
    pub thread_id: String,
}

pub type ThreadDeleteResponse = EmptyObjectResponse;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EmptyObjectResponse;

impl<'de> Deserialize<'de> for EmptyObjectResponse {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct EmptyObjectVisitor;

        impl<'de> Visitor<'de> for EmptyObjectVisitor {
            type Value = EmptyObjectResponse;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a JSON object")
            }

            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
                Ok(EmptyObjectResponse)
            }
        }

        deserializer.deserialize_map(EmptyObjectVisitor)
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadStartParams {
    pub thread_source: ThreadSourceKind,
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
    pub summary: ReasoningSummary,
    pub approval_policy: String,
    pub cwd: PathBuf,
    pub sandbox_policy: Value,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ReasoningSummary {
    Auto,
    Detailed,
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

pub type TurnInterruptResponse = EmptyObjectResponse;

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
pub struct ReasoningSummaryTextDeltaNotification {
    pub thread_id: String,
    pub turn_id: String,
    pub item_id: String,
    pub summary_index: i64,
    pub delta: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ReasoningSummaryPartAddedNotification {
    pub thread_id: String,
    pub turn_id: String,
    pub item_id: String,
    pub summary_index: i64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ReasoningTextDeltaNotification {
    pub thread_id: String,
    pub turn_id: String,
    pub item_id: String,
    pub content_index: i64,
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

/// The required token total from one app-server usage breakdown.
///
/// The installed schema contains additional breakdown fields. AgentHarness only
/// needs each breakdown's required `totalTokens` value and deliberately ignores
/// the other counters at the protocol boundary.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TokenUsageBreakdown {
    pub total_tokens: i64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ThreadTokenUsage {
    /// Current context occupancy used for the remaining-context meter.
    pub last: TokenUsageBreakdown,
    /// Cumulative usage retained for schema validation, not context occupancy.
    pub total: TokenUsageBreakdown,
    #[serde(default)]
    pub model_context_window: Option<i64>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ThreadTokenUsageUpdatedNotification {
    pub thread_id: String,
    pub turn_id: String,
    pub token_usage: ThreadTokenUsage,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProtocolEvent {
    AccountLoginCompleted(AccountLoginCompletedNotification),
    AccountUpdated,
    ThreadStarted(ThreadSnapshot),
    TurnStarted(TurnNotification),
    AgentMessageDelta(AgentMessageDeltaNotification),
    ReasoningSummaryTextDelta(ReasoningSummaryTextDeltaNotification),
    ReasoningSummaryPartAdded(ReasoningSummaryPartAddedNotification),
    ReasoningTextDelta(ReasoningTextDeltaNotification),
    ItemCompleted(ItemCompletedNotification),
    TurnCompleted(TurnNotification),
    ThreadTokenUsageUpdated(ThreadTokenUsageUpdatedNotification),
    Error(ErrorNotification),
}

pub fn parse_notification(method: &str, params: Value) -> Result<Option<ProtocolEvent>, String> {
    fn decode<T: for<'de> Deserialize<'de>>(method: &str, params: Value) -> Result<T, String> {
        serde_json::from_value(params)
            .map_err(|_| format!("{method} notification had invalid required fields"))
    }

    let invalid = || format!("{method} notification had invalid required fields");
    let event = match method {
        "account/login/completed" => ProtocolEvent::AccountLoginCompleted(decode(method, params)?),
        "account/updated" => ProtocolEvent::AccountUpdated,
        "thread/started" => {
            #[derive(Deserialize)]
            struct Body {
                thread: ThreadSnapshot,
            }
            let thread = decode::<Body>(method, params)?.thread;
            validate_thread_snapshot(&thread).map_err(|_| invalid())?;
            ProtocolEvent::ThreadStarted(thread)
        }
        "turn/started" => {
            let notification: TurnNotification = decode(method, params)?;
            validate_scope(&[&notification.thread_id]).map_err(|_| invalid())?;
            validate_turn_snapshot(&notification.turn).map_err(|_| invalid())?;
            if notification.turn.status != TurnStatus::InProgress {
                return Err(invalid());
            }
            ProtocolEvent::TurnStarted(notification)
        }
        "item/agentMessage/delta" => {
            let notification: AgentMessageDeltaNotification = decode(method, params)?;
            validate_scope(&[
                &notification.thread_id,
                &notification.turn_id,
                &notification.item_id,
            ])
            .map_err(|_| invalid())?;
            ProtocolEvent::AgentMessageDelta(notification)
        }
        "item/reasoning/summaryTextDelta" => {
            let notification: ReasoningSummaryTextDeltaNotification = decode(method, params)?;
            validate_scope(&[
                &notification.thread_id,
                &notification.turn_id,
                &notification.item_id,
            ])
            .map_err(|_| invalid())?;
            ProtocolEvent::ReasoningSummaryTextDelta(notification)
        }
        "item/reasoning/summaryPartAdded" => {
            let notification: ReasoningSummaryPartAddedNotification = decode(method, params)?;
            validate_scope(&[
                &notification.thread_id,
                &notification.turn_id,
                &notification.item_id,
            ])
            .map_err(|_| invalid())?;
            ProtocolEvent::ReasoningSummaryPartAdded(notification)
        }
        "item/reasoning/textDelta" => {
            let notification: ReasoningTextDeltaNotification = decode(method, params)?;
            validate_scope(&[
                &notification.thread_id,
                &notification.turn_id,
                &notification.item_id,
            ])
            .map_err(|_| invalid())?;
            ProtocolEvent::ReasoningTextDelta(notification)
        }
        "item/completed" => {
            let notification: ItemCompletedNotification = decode(method, params)?;
            validate_scope(&[&notification.thread_id, &notification.turn_id])
                .map_err(|_| invalid())?;
            validate_thread_item(&notification.item).map_err(|_| invalid())?;
            ProtocolEvent::ItemCompleted(notification)
        }
        "turn/completed" => {
            let notification: TurnNotification = decode(method, params)?;
            validate_scope(&[&notification.thread_id]).map_err(|_| invalid())?;
            validate_turn_snapshot(&notification.turn).map_err(|_| invalid())?;
            ProtocolEvent::TurnCompleted(notification)
        }
        "thread/tokenUsage/updated" => {
            let notification: ThreadTokenUsageUpdatedNotification = decode(method, params)?;
            validate_scope(&[&notification.thread_id, &notification.turn_id])
                .map_err(|_| invalid())?;
            ProtocolEvent::ThreadTokenUsageUpdated(notification)
        }
        "error" => {
            let notification: ErrorNotification = decode(method, params)?;
            validate_scope(&[&notification.thread_id, &notification.turn_id])
                .map_err(|_| invalid())?;
            ProtocolEvent::Error(notification)
        }
        _ => return Ok(None),
    };
    Ok(Some(event))
}

pub(super) fn validate_thread_snapshot(thread: &ThreadSnapshot) -> Result<(), ()> {
    validate_scope(&[&thread.id])?;
    for turn in &thread.turns {
        validate_turn_snapshot(turn)?;
    }
    Ok(())
}

pub(super) fn validate_turn_snapshot(turn: &TurnSnapshot) -> Result<(), ()> {
    validate_scope(&[&turn.id])?;
    for item in &turn.items {
        validate_thread_item(item)?;
    }
    Ok(())
}

fn validate_thread_item(item: &ThreadItem) -> Result<(), ()> {
    validate_scope(&[&item.id])?;
    if item.kind == "agentMessage" && item.text.is_none() {
        return Err(());
    }
    Ok(())
}

fn validate_scope(values: &[&str]) -> Result<(), ()> {
    if values.iter().any(|value| !valid_identifier(value)) {
        Err(())
    } else {
        Ok(())
    }
}

pub(super) fn valid_identifier(value: &str) -> bool {
    !value.is_empty()
        && value == value.trim()
        && value.len() <= MAX_PROTOCOL_IDENTIFIER_BYTES
        && !value.chars().any(is_terminal_unsafe)
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
    let method = object
        .get("method")
        .map(|method| {
            method
                .as_str()
                .ok_or_else(|| "JSON-RPC method must be a string".to_owned())
        })
        .transpose()?;
    let has_response_fields = object.contains_key("result") || object.contains_key("error");

    match (id, method) {
        (Some(_), Some(_)) | (None, Some(_)) if has_response_fields => {
            Err("JSON-RPC request or notification cannot contain response fields".to_owned())
        }
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
    use std::path::PathBuf;

    use serde_json::json;

    use super::{
        classify_message, parse_notification, CancelLoginAccountParams, CancelLoginAccountResponse,
        CancelLoginAccountStatus, InboundMessage, InitializeParams, LoginAccountParams,
        ProtocolEvent, ReasoningSummary, RequestId, ThreadDeleteParams, ThreadDeleteResponse,
        ThreadItemContent, ThreadListParams, ThreadListResponse, ThreadSourceKind,
        ThreadStartParams, MAX_PROTOCOL_IDENTIFIER_BYTES,
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
    fn models_thread_listing_and_deletion_from_the_installed_schema() {
        let params = serde_json::to_value(ThreadListParams {
            source_kinds: vec![ThreadSourceKind::AppServer, ThreadSourceKind::Vscode],
            archived: false,
            cursor: None,
            cwd: PathBuf::from("/tmp/conversation"),
            limit: 50,
            sort_direction: "desc".to_owned(),
            sort_key: "updated_at".to_owned(),
        })
        .unwrap();
        assert_eq!(params["sourceKinds"], json!(["appServer", "vscode"]));
        assert_eq!(params["cwd"], "/tmp/conversation");

        let start = serde_json::to_value(ThreadStartParams {
            thread_source: ThreadSourceKind::AppServer,
            approval_policy: "never".to_owned(),
            config: json!({}),
            cwd: PathBuf::from("/tmp/conversation"),
            sandbox: "danger-full-access".to_owned(),
            model: "m1".to_owned(),
        })
        .unwrap();
        assert_eq!(start["threadSource"], "appServer");
        let deletion = serde_json::to_value(ThreadDeleteParams {
            thread_id: "thr-old".to_owned(),
        })
        .unwrap();
        assert_eq!(deletion, json!({"threadId": "thr-old"}));
        let _: ThreadDeleteResponse = serde_json::from_value(json!({})).unwrap();
        assert!(serde_json::from_value::<ThreadDeleteResponse>(json!(null)).is_err());
        assert!(serde_json::from_value::<ThreadDeleteResponse>(json!([])).is_err());

        let list: ThreadListResponse = serde_json::from_value(json!({
            "data": [{
                "id": "thr-1",
                "name": null,
                "preview": "hello",
                "createdAt": 10,
                "updatedAt": 20,
                "cwd": "/tmp/conversation",
                "ephemeral": false,
                "source": "appServer"
            }],
            "nextCursor": "page-2"
        }))
        .unwrap();
        assert_eq!(list.data[0].updated_at, 20);
        assert_eq!(list.next_cursor.as_deref(), Some("page-2"));
        assert!(serde_json::from_value::<ThreadListResponse>(json!({
            "data": [{"id": "thr-malformed", "preview": "missing required fields"}]
        }))
        .is_err());
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

        for malformed in [
            json!([]),
            json!({"id": -1, "result": {}}),
            json!({"id": 1.5, "result": {}}),
            json!({"id": 1}),
            json!({"id": 1, "error": {"code": "bad", "message": "failure"}}),
            json!({"method": 7, "params": {}}),
            json!({"id": 1, "method": 7, "result": {}}),
            json!({"id": 1, "method": "approval", "result": {}}),
            json!({"method": "notice", "error": {"code": -1, "message": "bad"}}),
        ] {
            assert!(
                classify_message(malformed).is_err(),
                "malformed JSON-RPC frame was classified as usable"
            );
        }
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

    #[test]
    fn rejects_empty_event_scope_and_incomplete_agent_snapshots() {
        for (method, params) in [
            (
                "item/agentMessage/delta",
                json!({"threadId":"", "turnId":"turn", "itemId":"item", "delta":"hi"}),
            ),
            (
                "turn/started",
                json!({
                    "threadId":"thr",
                    "turn":{"id":"", "items":[], "status":"inProgress"}
                }),
            ),
            (
                "item/completed",
                json!({
                    "threadId":"thr", "turnId":"turn",
                    "item":{"id":"item", "type":"agentMessage"}
                }),
            ),
            (
                "turn/started",
                json!({
                    "threadId":"thr",
                    "turn":{"id":"turn", "items":[], "status":"completed"}
                }),
            ),
            (
                "item/agentMessage/delta",
                json!({
                    "threadId":"thr", "turnId":"turn", "itemId":"bad\nid", "delta":"x"
                }),
            ),
            (
                "item/agentMessage/delta",
                json!({
                    "threadId":"thr", "turnId":"turn",
                    "itemId":"x".repeat(MAX_PROTOCOL_IDENTIFIER_BYTES + 1), "delta":"x"
                }),
            ),
        ] {
            assert!(
                parse_notification(method, params).is_err(),
                "{method} accepted an unusable scope or snapshot"
            );
        }
    }

    #[test]
    fn decodes_installed_reasoning_notifications_and_completed_snapshot() {
        let summary = parse_notification(
            "item/reasoning/summaryTextDelta",
            json!({
                "threadId":"thr", "turnId":"turn", "itemId":"why",
                "summaryIndex":1, "delta":"checking"
            }),
        )
        .unwrap();
        assert!(matches!(
            summary,
            Some(ProtocolEvent::ReasoningSummaryTextDelta(delta))
                if delta.summary_index == 1 && delta.delta == "checking"
        ));

        let part = parse_notification(
            "item/reasoning/summaryPartAdded",
            json!({
                "threadId":"thr", "turnId":"turn", "itemId":"why", "summaryIndex":2
            }),
        )
        .unwrap();
        assert!(matches!(
            part,
            Some(ProtocolEvent::ReasoningSummaryPartAdded(part)) if part.summary_index == 2
        ));

        let text = parse_notification(
            "item/reasoning/textDelta",
            json!({
                "threadId":"thr", "turnId":"turn", "itemId":"why",
                "contentIndex":0, "delta":"emitted"
            }),
        )
        .unwrap();
        assert!(matches!(
            text,
            Some(ProtocolEvent::ReasoningTextDelta(delta))
                if delta.content_index == 0 && delta.delta == "emitted"
        ));

        let completed = parse_notification(
            "item/completed",
            json!({
                "threadId":"thr", "turnId":"turn", "completedAtMs":1,
                "item": {
                    "id":"why", "type":"reasoning",
                    "summary":["checking facts"], "content":["emitted detail"]
                }
            }),
        )
        .unwrap();
        assert!(matches!(
            completed,
            Some(ProtocolEvent::ItemCompleted(completed))
                if completed.item.summary == ["checking facts"]
                    && matches!(completed.item.content.as_slice(), [ThreadItemContent::Text(value)] if value == "emitted detail")
        ));

        assert!(parse_notification(
            "item/reasoning/summaryTextDelta",
            json!({"threadId":"thr", "turnId":"turn", "itemId":"why", "delta":"missing index"}),
        )
        .is_err());
        assert_eq!(
            serde_json::to_value(ReasoningSummary::Auto).unwrap(),
            json!("auto")
        );
        assert_eq!(
            serde_json::to_value(ReasoningSummary::Detailed).unwrap(),
            json!("detailed")
        );
    }

    #[test]
    fn decodes_token_usage_from_the_installed_schema() {
        let event = parse_notification(
            "thread/tokenUsage/updated",
            json!({
                "threadId": "thr",
                "turnId": "turn",
                "tokenUsage": {
                    "last": {
                        "cachedInputTokens": 0,
                        "inputTokens": 10,
                        "outputTokens": 5,
                        "reasoningOutputTokens": 2,
                        "totalTokens": 17
                    },
                    "total": {
                        "cachedInputTokens": 0,
                        "inputTokens": 30,
                        "outputTokens": 10,
                        "reasoningOutputTokens": 5,
                        "totalTokens": 45
                    },
                    "modelContextWindow": 100
                }
            }),
        )
        .unwrap();

        assert!(matches!(
            event,
            Some(ProtocolEvent::ThreadTokenUsageUpdated(usage))
                if usage.thread_id == "thr"
                    && usage.turn_id == "turn"
                    && usage.token_usage.last.total_tokens == 17
                    && usage.token_usage.total.total_tokens == 45
                    && usage.token_usage.model_context_window == Some(100)
        ));

        let null_window = parse_notification(
            "thread/tokenUsage/updated",
            json!({
                "threadId": "thr",
                "turnId": "turn",
                "tokenUsage": {
                    "last": { "totalTokens": 5 },
                    "total": { "totalTokens": 45 },
                    "modelContextWindow": null
                }
            }),
        )
        .unwrap();
        assert!(matches!(
            null_window,
            Some(ProtocolEvent::ThreadTokenUsageUpdated(usage))
                if usage.token_usage.model_context_window.is_none()
        ));
    }

    #[test]
    fn rejects_malformed_and_out_of_range_token_usage() {
        for malformed in [
            json!({"threadId":"thr","turnId":"turn"}),
            json!({"threadId":"thr","turnId":"turn","tokenUsage":{}}),
            json!({
                "threadId":"thr","turnId":"turn",
                "tokenUsage":{"last":{"totalTokens":5}}
            }),
            json!({
                "threadId":"thr","turnId":"turn",
                "tokenUsage":{"total":{"totalTokens":45}}
            }),
            json!({
                "threadId":"thr","turnId":"turn",
                "tokenUsage":{"last":{},"total":{"totalTokens":45}}
            }),
            json!({
                "threadId":"thr","turnId":"turn",
                "tokenUsage":{
                    "last":{"totalTokens":"5"},
                    "total":{"totalTokens":45}
                }
            }),
        ] {
            assert!(parse_notification("thread/tokenUsage/updated", malformed).is_err());
        }

        let too_large = serde_json::from_str(
            r#"{"threadId":"thr","turnId":"turn","tokenUsage":{"last":{"totalTokens":9223372036854775808},"total":{"totalTokens":45},"modelContextWindow":100}}"#,
        )
        .unwrap();
        assert!(parse_notification("thread/tokenUsage/updated", too_large).is_err());
    }
}
