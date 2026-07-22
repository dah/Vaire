use std::collections::HashSet;

use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use thiserror::Error;

use super::protocol::{
    parse_notification, valid_identifier, validate_thread_snapshot, validate_turn_snapshot,
    AccountReadResponse, CancelLoginAccountParams, CancelLoginAccountResponse,
    CancelLoginAccountStatus, InitializeParams, InitializeResponse, LoginAccountParams,
    LoginAccountResponse, LogoutAccountResponse, ModelInfo, ModelListParams, ModelListResponse,
    ProtocolEvent, ReasoningSummary, ThreadDeleteParams, ThreadDeleteResponse, ThreadItemContent,
    ThreadListEntry, ThreadListParams, ThreadListResponse, ThreadReadParams, ThreadResponse,
    ThreadResumeParams, ThreadSnapshot, ThreadSourceKind, ThreadStartParams, TurnInterruptParams,
    TurnInterruptResponse, TurnStartParams, TurnStartResponse, UserInput,
};
use super::safety::{FullAccessPolicy, IsolationPaths};
use super::transport::{AppServerTransport, TransportError};
use crate::app::{ModelChoice, ThreadChoice, TranscriptEntry, TranscriptRole};
use crate::persistence::AccountScope;

const MAX_PAGINATION_PAGES: usize = 256;
const MAX_MODEL_PAGE_ITEMS: usize = 1_024;
const MAX_THREAD_PAGE_ITEMS: usize = 50;
const MAX_PAGINATION_ITEMS: usize = 16_384;
const MAX_PAGINATION_RETAINED_BYTES: usize = 16 * 1024 * 1024;
const MAX_CURSOR_BYTES: usize = 16 * 1024;

#[derive(Default)]
struct PaginationBudget {
    items: usize,
    retained_bytes: usize,
}

impl PaginationBudget {
    fn retain(&mut self, method: &'static str, bytes: usize) -> Result<(), SessionError> {
        self.items = self.items.checked_add(1).ok_or_else(|| {
            SessionError::Protocol(format!("{method} exceeded the retained item limit"))
        })?;
        self.retained_bytes = self.retained_bytes.checked_add(bytes).ok_or_else(|| {
            SessionError::Protocol(format!("{method} exceeded the retained byte limit"))
        })?;
        if self.items > MAX_PAGINATION_ITEMS {
            return Err(SessionError::Protocol(format!(
                "{method} exceeded the retained item limit"
            )));
        }
        if self.retained_bytes > MAX_PAGINATION_RETAINED_BYTES {
            return Err(SessionError::Protocol(format!(
                "{method} exceeded the retained byte limit"
            )));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AccountState {
    SignedOut,
    Chatgpt { scope: Option<AccountScope> },
    Unsupported(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LoginChallenge {
    pub login_id: String,
    pub auth_url: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeviceLoginChallenge {
    pub login_id: String,
    pub verification_url: String,
    pub user_code: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum SessionEvent {
    Protocol(ProtocolEvent),
    UnknownNotification(String),
    SafetyViolation(String),
    ConnectionClosed(String),
}

#[derive(Debug, Error)]
pub enum SessionError {
    #[error(transparent)]
    Transport(#[from] TransportError),
    #[error("app-server response for {method} did not match the tested protocol")]
    Decode { method: &'static str },
    #[error("app-server protocol violation: {0}")]
    Protocol(String),
}

pub struct SessionService {
    transport: AppServerTransport,
    paths: IsolationPaths,
    policy: FullAccessPolicy,
}

impl SessionService {
    pub fn new(
        transport: AppServerTransport,
        paths: IsolationPaths,
        policy: FullAccessPolicy,
    ) -> Self {
        Self {
            transport,
            paths,
            policy,
        }
    }

    pub fn generation(&self) -> u64 {
        self.transport.generation()
    }

    pub async fn initialize(&self) -> Result<InitializeResponse, SessionError> {
        let response = self
            .transport
            .request_default("initialize", InitializeParams::agentharness())
            .await?;
        let response = decode("initialize", response)?;
        self.transport.notify("initialized", json!({})).await?;
        Ok(response)
    }

    pub async fn read_account(&self) -> Result<AccountState, SessionError> {
        let response: AccountReadResponse = decode(
            "account/read",
            self.transport
                .request_default("account/read", json!({"refreshToken": false}))
                .await?,
        )?;
        let Some(account) = response.account else {
            return Ok(AccountState::SignedOut);
        };
        if account.kind != "chatgpt" {
            return Ok(AccountState::Unsupported(account.kind));
        }
        Ok(AccountState::Chatgpt {
            scope: account
                .email
                .as_deref()
                .and_then(AccountScope::from_chatgpt_email),
        })
    }

    pub async fn start_login(&self) -> Result<LoginChallenge, SessionError> {
        let response: LoginAccountResponse = decode(
            "account/login/start",
            self.transport
                .request_default("account/login/start", LoginAccountParams::chatgpt())
                .await?,
        )?;
        if response.kind != "chatgpt" {
            return Err(SessionError::Protocol(
                "login did not return the ChatGPT browser flow".to_owned(),
            ));
        }
        let login_id = response
            .login_id
            .filter(|value| valid_identifier(value))
            .ok_or_else(|| {
                SessionError::Protocol(
                    "login response omitted or returned invalid loginId".to_owned(),
                )
            })?;
        let auth_url = response
            .auth_url
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| SessionError::Protocol("login response omitted authUrl".to_owned()))?;
        Ok(LoginChallenge { login_id, auth_url })
    }

    pub async fn start_device_login(&self) -> Result<DeviceLoginChallenge, SessionError> {
        let response: LoginAccountResponse = decode(
            "account/login/start",
            self.transport
                .request_default(
                    "account/login/start",
                    LoginAccountParams::chatgpt_device_code(),
                )
                .await?,
        )?;
        if response.kind != "chatgptDeviceCode" {
            return Err(SessionError::Protocol(
                "login did not return the ChatGPT device-code flow".to_owned(),
            ));
        }
        let login_id = response
            .login_id
            .filter(|value| valid_identifier(value))
            .ok_or_else(|| {
                SessionError::Protocol(
                    "device login response omitted or returned invalid loginId".to_owned(),
                )
            })?;
        let verification_url = response
            .verification_url
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| {
                SessionError::Protocol("device login response omitted verificationUrl".to_owned())
            })?;
        let user_code = response
            .user_code
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| {
                SessionError::Protocol("device login response omitted userCode".to_owned())
            })?;
        Ok(DeviceLoginChallenge {
            login_id,
            verification_url,
            user_code,
        })
    }

    pub async fn cancel_login(
        &self,
        login_id: &str,
    ) -> Result<CancelLoginAccountStatus, SessionError> {
        let response: CancelLoginAccountResponse = decode(
            "account/login/cancel",
            self.transport
                .request_default(
                    "account/login/cancel",
                    CancelLoginAccountParams::new(login_id),
                )
                .await?,
        )?;
        Ok(response.status)
    }

    pub async fn logout(&self) -> Result<(), SessionError> {
        let _: LogoutAccountResponse = decode(
            "account/logout",
            self.transport
                .request_default("account/logout", json!({}))
                .await?,
        )?;
        Ok(())
    }

    pub async fn list_models(&self) -> Result<Vec<ModelInfo>, SessionError> {
        let mut cursor = None;
        let mut seen_cursors = HashSet::new();
        let mut seen_models = HashSet::new();
        let mut models = Vec::new();
        let mut pages = 0;
        let mut budget = PaginationBudget::default();
        loop {
            let response: ModelListResponse = decode(
                "model/list",
                self.transport
                    .request_default(
                        "model/list",
                        ModelListParams {
                            cursor: cursor.clone(),
                            include_hidden: false,
                        },
                    )
                    .await?,
            )?;
            pages += 1;
            validate_page_len("model/list", response.data.len(), MAX_MODEL_PAGE_ITEMS)?;
            for model in response.data {
                if model.hidden {
                    continue;
                }
                if !valid_identifier(&model.id)
                    || !valid_identifier(&model.default_reasoning_effort)
                    || model
                        .supported_reasoning_efforts
                        .iter()
                        .any(|option| !valid_identifier(&option.reasoning_effort))
                {
                    return Err(SessionError::Protocol(
                        "model/list returned an invalid model or reasoning id".to_owned(),
                    ));
                }
                if seen_models.insert(model.id.clone()) {
                    budget.retain("model/list", model_retained_bytes(&model))?;
                    models.push(model);
                }
            }
            let Some(next) =
                next_cursor("model/list", pages, &mut seen_cursors, response.next_cursor)?
            else {
                break;
            };
            cursor = Some(next);
        }
        Ok(models)
    }

    pub async fn list_threads(&self) -> Result<Vec<ThreadListEntry>, SessionError> {
        let mut cursor = None;
        let mut seen_cursors = HashSet::new();
        let mut seen_threads = HashSet::new();
        let mut threads = Vec::new();
        let mut pages = 0;
        let mut budget = PaginationBudget::default();
        loop {
            let response: ThreadListResponse = decode(
                "thread/list",
                self.transport
                    .request_default(
                        "thread/list",
                        ThreadListParams {
                            source_kinds: vec![
                                ThreadSourceKind::AppServer,
                                ThreadSourceKind::Vscode,
                            ],
                            archived: false,
                            cursor: cursor.clone(),
                            cwd: self.paths.conversation.clone(),
                            limit: 50,
                            sort_direction: "desc".to_owned(),
                            sort_key: "updated_at".to_owned(),
                        },
                    )
                    .await?,
            )?;
            pages += 1;
            validate_page_len("thread/list", response.data.len(), MAX_THREAD_PAGE_ITEMS)?;
            for thread in response.data {
                if !valid_identifier(&thread.id) {
                    return Err(SessionError::Protocol(
                        "thread/list returned an invalid thread id".to_owned(),
                    ));
                }
                if thread.cwd != self.paths.conversation {
                    return Err(SessionError::Protocol(
                        "thread/list returned a thread outside the AgentHarness working directory"
                            .to_owned(),
                    ));
                }
                if !thread.source.is_supported_resume_source() {
                    return Err(SessionError::Protocol(
                        "thread/list returned a thread from an unsupported source".to_owned(),
                    ));
                }
                if !thread.ephemeral && seen_threads.insert(thread.id.clone()) {
                    budget.retain("thread/list", thread_retained_bytes(&thread))?;
                    threads.push(thread);
                }
            }
            let Some(next) = next_cursor(
                "thread/list",
                pages,
                &mut seen_cursors,
                response.next_cursor,
            )?
            else {
                break;
            };
            cursor = Some(next);
        }
        Ok(threads)
    }

    pub async fn start_thread(&self, model: &str) -> Result<ThreadSnapshot, SessionError> {
        let params = self.thread_start_params(model);
        let response: ThreadResponse = decode(
            "thread/start",
            self.transport
                .request_default("thread/start", params)
                .await?,
        )?;
        validate_thread_snapshot(&response.thread).map_err(|_| {
            SessionError::Protocol("thread/start returned an invalid thread snapshot".to_owned())
        })?;
        Ok(response.thread)
    }

    pub async fn resume_thread(
        &self,
        thread_id: &str,
        model: &str,
    ) -> Result<ThreadSnapshot, SessionError> {
        let overrides = self.policy.thread_start_overrides(&self.paths.conversation);
        let response: ThreadResponse = decode(
            "thread/resume",
            self.transport
                .request_default(
                    "thread/resume",
                    ThreadResumeParams {
                        thread_id: thread_id.to_owned(),
                        approval_policy: "never".to_owned(),
                        config: overrides["config"].clone(),
                        cwd: self.paths.conversation.clone(),
                        sandbox: "danger-full-access".to_owned(),
                        model: model.to_owned(),
                    },
                )
                .await?,
        )?;
        validate_thread_snapshot(&response.thread).map_err(|_| {
            SessionError::Protocol("thread/resume returned an invalid thread snapshot".to_owned())
        })?;
        if response.thread.id != thread_id {
            return Err(SessionError::Protocol(
                "thread/resume returned a different thread id".to_owned(),
            ));
        }
        self.read_thread(thread_id).await
    }

    pub async fn read_thread(&self, thread_id: &str) -> Result<ThreadSnapshot, SessionError> {
        let response: ThreadResponse = decode(
            "thread/read",
            self.transport
                .request_default(
                    "thread/read",
                    ThreadReadParams {
                        thread_id: thread_id.to_owned(),
                        include_turns: true,
                    },
                )
                .await?,
        )?;
        validate_thread_snapshot(&response.thread).map_err(|_| {
            SessionError::Protocol("thread/read returned an invalid thread snapshot".to_owned())
        })?;
        if response.thread.id != thread_id {
            return Err(SessionError::Protocol(
                "thread/read returned a different thread id".to_owned(),
            ));
        }
        Ok(response.thread)
    }

    pub async fn delete_thread(&self, thread_id: &str) -> Result<(), SessionError> {
        let _: ThreadDeleteResponse = decode(
            "thread/delete",
            self.transport
                .request_default(
                    "thread/delete",
                    ThreadDeleteParams {
                        thread_id: thread_id.to_owned(),
                    },
                )
                .await?,
        )?;
        Ok(())
    }

    pub async fn start_turn(
        &self,
        thread_id: &str,
        text: &str,
        model: &str,
        effort: &str,
    ) -> Result<TurnStartResponse, SessionError> {
        let overrides = self.policy.turn_start_overrides(&self.paths.conversation);
        let response: TurnStartResponse = decode(
            "turn/start",
            self.transport
                .request_default(
                    "turn/start",
                    TurnStartParams {
                        thread_id: thread_id.to_owned(),
                        input: vec![UserInput::text(text)],
                        model: model.to_owned(),
                        effort: effort.to_owned(),
                        summary: ReasoningSummary::Detailed,
                        approval_policy: "never".to_owned(),
                        cwd: self.paths.conversation.clone(),
                        sandbox_policy: overrides["sandboxPolicy"].clone(),
                    },
                )
                .await?,
        )?;
        validate_turn_snapshot(&response.turn).map_err(|_| {
            SessionError::Protocol("turn/start returned an invalid turn snapshot".to_owned())
        })?;
        if response.turn.status != super::protocol::TurnStatus::InProgress {
            return Err(SessionError::Protocol(
                "turn/start returned a terminal or unknown turn status".to_owned(),
            ));
        }
        Ok(response)
    }

    pub async fn interrupt_turn(&self, thread_id: &str, turn_id: &str) -> Result<(), SessionError> {
        let _: TurnInterruptResponse = decode(
            "turn/interrupt",
            self.transport
                .request_default(
                    "turn/interrupt",
                    TurnInterruptParams {
                        thread_id: thread_id.to_owned(),
                        turn_id: turn_id.to_owned(),
                    },
                )
                .await?,
        )?;
        Ok(())
    }

    pub async fn next_event(&mut self) -> Option<Result<SessionEvent, SessionError>> {
        let event = self.transport.next_event().await?;
        if event.generation != self.transport.generation() {
            return Some(Ok(SessionEvent::UnknownNotification(
                "stale-generation".to_owned(),
            )));
        }
        use super::protocol::InboundEvent;
        Some(match event.event {
            InboundEvent::Notification { method, params } => {
                match parse_notification(&method, params) {
                    Ok(Some(event)) => Ok(SessionEvent::Protocol(event)),
                    Ok(None) => Ok(SessionEvent::UnknownNotification(method)),
                    Err(message) => Err(SessionError::Protocol(message)),
                }
            }
            InboundEvent::SafetyViolation { method, .. } => {
                Ok(SessionEvent::SafetyViolation(method))
            }
            InboundEvent::ConnectionClosed { category } => {
                Ok(SessionEvent::ConnectionClosed(category))
            }
        })
    }

    pub async fn shutdown(&mut self) -> Result<(), SessionError> {
        self.transport.shutdown().await?;
        Ok(())
    }

    fn thread_start_params(&self, model: &str) -> ThreadStartParams {
        let overrides = self.policy.thread_start_overrides(&self.paths.conversation);
        ThreadStartParams {
            thread_source: ThreadSourceKind::AppServer,
            approval_policy: "never".to_owned(),
            config: overrides["config"].clone(),
            cwd: self.paths.conversation.clone(),
            sandbox: "danger-full-access".to_owned(),
            model: model.to_owned(),
        }
    }
}

fn decode<T: DeserializeOwned>(method: &'static str, value: Value) -> Result<T, SessionError> {
    serde_json::from_value(value).map_err(|_| SessionError::Decode { method })
}

fn next_cursor(
    method: &'static str,
    pages: usize,
    seen: &mut HashSet<String>,
    next: Option<String>,
) -> Result<Option<String>, SessionError> {
    let Some(next) = next else {
        return Ok(None);
    };
    if next.is_empty() {
        return Err(SessionError::Protocol(format!(
            "{method} returned an empty pagination cursor"
        )));
    }
    if next.len() > MAX_CURSOR_BYTES || next.chars().any(crate::text::is_terminal_unsafe) {
        return Err(SessionError::Protocol(format!(
            "{method} returned an invalid pagination cursor"
        )));
    }
    if seen.contains(&next) {
        return Err(SessionError::Protocol(format!(
            "{method} returned a cursor cycle"
        )));
    }
    if pages >= MAX_PAGINATION_PAGES {
        return Err(SessionError::Protocol(format!(
            "{method} exceeded the pagination limit"
        )));
    }
    seen.insert(next.clone());
    Ok(Some(next))
}

fn validate_page_len(
    method: &'static str,
    items: usize,
    maximum: usize,
) -> Result<(), SessionError> {
    if items > maximum {
        Err(SessionError::Protocol(format!(
            "{method} exceeded the page item limit"
        )))
    } else {
        Ok(())
    }
}

fn model_retained_bytes(model: &ModelInfo) -> usize {
    let option_bytes = model
        .supported_reasoning_efforts
        .iter()
        .fold(0usize, |total, option| {
            total
                .saturating_add(option.reasoning_effort.len())
                .saturating_add(option.description.len())
        });
    model
        .id
        .len()
        .saturating_mul(2)
        .saturating_add(model.display_name.len())
        .saturating_add(model.default_reasoning_effort.len())
        .saturating_add(option_bytes)
}

fn thread_retained_bytes(thread: &ThreadListEntry) -> usize {
    thread
        .id
        .len()
        .saturating_mul(2)
        .saturating_add(thread.name.as_deref().map_or(0, str::len))
        .saturating_add(thread.preview.len())
        .saturating_add(thread.cwd.to_string_lossy().len())
}

pub fn model_choices(models: &[ModelInfo]) -> Vec<ModelChoice> {
    models
        .iter()
        .map(|model| ModelChoice {
            id: model.id.clone(),
            display_name: model.display_name.clone(),
            is_default: model.is_default,
            default_reasoning_effort: model.default_reasoning_effort.clone(),
            supported_reasoning_efforts: model
                .supported_reasoning_efforts
                .iter()
                .map(|option| option.reasoning_effort.clone())
                .collect(),
        })
        .collect()
}

pub fn thread_choices(threads: Vec<ThreadListEntry>) -> Vec<ThreadChoice> {
    threads
        .into_iter()
        .map(|thread| {
            let title = thread
                .name
                .as_deref()
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .or_else(|| {
                    thread
                        .preview
                        .lines()
                        .next()
                        .map(str::trim)
                        .filter(|preview| !preview.is_empty())
                })
                .unwrap_or("Untitled thread")
                .to_owned();
            ThreadChoice {
                id: thread.id,
                title,
                updated_at: thread.updated_at,
            }
        })
        .collect()
}

pub fn history_entries(thread: &ThreadSnapshot) -> Vec<TranscriptEntry> {
    let mut entries = Vec::new();
    for turn in &thread.turns {
        for item in &turn.items {
            match item.kind.as_str() {
                "userMessage" => {
                    for input in &item.content {
                        if let ThreadItemContent::UserInput(input) = input {
                            if input.kind == "text" {
                                if let Some(text) = &input.text {
                                    entries.push(TranscriptEntry {
                                        role: TranscriptRole::User,
                                        text: text.clone(),
                                        item_id: Some(item.id.clone()),
                                        turn_id: Some(turn.id.clone()),
                                    });
                                }
                            }
                        }
                    }
                }
                "agentMessage" => {
                    if let Some(text) = &item.text {
                        entries.push(TranscriptEntry {
                            role: TranscriptRole::Assistant,
                            text: text.clone(),
                            item_id: Some(item.id.clone()),
                            turn_id: Some(turn.id.clone()),
                        });
                    }
                }
                _ => {}
            }
        }
    }
    entries
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use std::collections::HashSet;

    use super::{
        history_entries, model_choices, next_cursor, thread_choices, validate_page_len,
        PaginationBudget, SessionError, MAX_CURSOR_BYTES, MAX_PAGINATION_RETAINED_BYTES,
        MAX_THREAD_PAGE_ITEMS,
    };
    use crate::codex::protocol::{
        ModelInfo, ReasoningEffortOption, SessionSource, SessionSourceName, ThreadItem,
        ThreadItemContent, ThreadListEntry, ThreadSnapshot, TurnSnapshot, TurnStatus, UserInput,
    };

    #[test]
    fn converts_catalog_and_restores_only_conversation_history() {
        let choices = model_choices(&[ModelInfo {
            id: "m".to_owned(),
            display_name: "Model".to_owned(),
            is_default: true,
            default_reasoning_effort: "high".to_owned(),
            supported_reasoning_efforts: vec![ReasoningEffortOption {
                reasoning_effort: "high".to_owned(),
                description: "deep".to_owned(),
            }],
            hidden: false,
        }]);
        assert_eq!(choices[0].supported_reasoning_efforts, vec!["high"]);
        let thread = ThreadSnapshot {
            id: "thr".to_owned(),
            turns: vec![TurnSnapshot {
                id: "turn".to_owned(),
                status: TurnStatus::Completed,
                error: None,
                items: vec![
                    ThreadItem {
                        id: "u".to_owned(),
                        kind: "userMessage".to_owned(),
                        text: None,
                        content: vec![UserInput::text("hello").into()],
                        summary: vec![],
                    },
                    ThreadItem {
                        id: "tool".to_owned(),
                        kind: "commandExecution".to_owned(),
                        text: None,
                        content: vec![],
                        summary: vec![],
                    },
                    ThreadItem {
                        id: "why".to_owned(),
                        kind: "reasoning".to_owned(),
                        text: None,
                        content: vec![ThreadItemContent::Text("emitted detail".to_owned())],
                        summary: vec!["checked facts".to_owned()],
                    },
                    ThreadItem {
                        id: "a".to_owned(),
                        kind: "agentMessage".to_owned(),
                        text: Some("hi".to_owned()),
                        content: vec![],
                        summary: vec![],
                    },
                ],
            }],
        };
        let history = history_entries(&thread);
        assert_eq!(history.len(), 2);
        assert_eq!(history[1].text, "hi");
    }

    #[test]
    fn thread_choices_prefer_names_then_preview_then_fallback() {
        let choices = thread_choices(vec![
            ThreadListEntry {
                id: "named".to_owned(),
                name: Some("  A name  ".to_owned()),
                preview: "ignored".to_owned(),
                created_at: 1,
                updated_at: 3,
                cwd: PathBuf::from("/tmp/conversation"),
                ephemeral: false,
                source: SessionSource::Named(SessionSourceName::AppServer),
            },
            ThreadListEntry {
                id: "preview".to_owned(),
                name: None,
                preview: "  First line\nsecond line".to_owned(),
                created_at: 1,
                updated_at: 2,
                cwd: PathBuf::from("/tmp/conversation"),
                ephemeral: false,
                source: SessionSource::Named(SessionSourceName::Vscode),
            },
            ThreadListEntry {
                id: "empty".to_owned(),
                name: Some(" ".to_owned()),
                preview: String::new(),
                created_at: 1,
                updated_at: 1,
                cwd: PathBuf::from("/tmp/conversation"),
                ephemeral: false,
                source: SessionSource::Named(SessionSourceName::AppServer),
            },
        ]);
        assert_eq!(choices[0].title, "A name");
        assert_eq!(choices[1].title, "First line");
        assert_eq!(choices[2].title, "Untitled thread");
    }

    #[test]
    fn pagination_limits_bound_page_shape_cursor_and_retained_memory() {
        assert!(matches!(
            validate_page_len("thread/list", MAX_THREAD_PAGE_ITEMS + 1, MAX_THREAD_PAGE_ITEMS),
            Err(SessionError::Protocol(message)) if message.contains("page item limit")
        ));

        let mut seen = HashSet::new();
        assert!(matches!(
            next_cursor(
                "model/list",
                1,
                &mut seen,
                Some("x".repeat(MAX_CURSOR_BYTES + 1)),
            ),
            Err(SessionError::Protocol(message)) if message.contains("pagination cursor")
        ));

        let mut budget = PaginationBudget::default();
        budget
            .retain("model/list", MAX_PAGINATION_RETAINED_BYTES)
            .unwrap();
        assert!(matches!(
            budget.retain("model/list", 1),
            Err(SessionError::Protocol(message)) if message.contains("retained byte limit")
        ));
    }
}
