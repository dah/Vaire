use std::collections::HashSet;

use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use thiserror::Error;

use super::protocol::{
    parse_notification, AccountReadResponse, CancelLoginAccountParams, CancelLoginAccountResponse,
    CancelLoginAccountStatus, InitializeParams, InitializeResponse, LoginAccountParams,
    LoginAccountResponse, ModelInfo, ModelListParams, ModelListResponse, ProtocolEvent,
    ThreadDeleteParams, ThreadDeleteResponse, ThreadListEntry, ThreadListParams,
    ThreadListResponse, ThreadReadParams, ThreadResponse, ThreadResumeParams, ThreadSnapshot,
    ThreadStartParams, TurnInterruptParams, TurnStartParams, TurnStartResponse, UserInput,
};
use super::safety::{ConversationSafetyPolicy, IsolationPaths};
use super::transport::{AppServerTransport, TransportError};
use crate::app::{ModelChoice, ThreadChoice, TranscriptEntry, TranscriptRole};
use crate::persistence::AccountScope;

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
    policy: ConversationSafetyPolicy,
}

impl SessionService {
    pub fn new(
        transport: AppServerTransport,
        paths: IsolationPaths,
        policy: ConversationSafetyPolicy,
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
        Ok(LoginChallenge {
            login_id: response.login_id.ok_or_else(|| {
                SessionError::Protocol("login response omitted loginId".to_owned())
            })?,
            auth_url: response.auth_url.ok_or_else(|| {
                SessionError::Protocol("login response omitted authUrl".to_owned())
            })?,
        })
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
        Ok(DeviceLoginChallenge {
            login_id: response.login_id.ok_or_else(|| {
                SessionError::Protocol("device login response omitted loginId".to_owned())
            })?,
            verification_url: response.verification_url.ok_or_else(|| {
                SessionError::Protocol("device login response omitted verificationUrl".to_owned())
            })?,
            user_code: response.user_code.ok_or_else(|| {
                SessionError::Protocol("device login response omitted userCode".to_owned())
            })?,
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
        self.transport
            .request_default("account/logout", json!({}))
            .await?;
        Ok(())
    }

    pub async fn list_models(&self) -> Result<Vec<ModelInfo>, SessionError> {
        let mut cursor = None;
        let mut seen_cursors = HashSet::new();
        let mut seen_models = HashSet::new();
        let mut models = Vec::new();
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
            for model in response.data {
                if !model.hidden && seen_models.insert(model.id.clone()) {
                    models.push(model);
                }
            }
            let Some(next) = response.next_cursor else {
                break;
            };
            if !seen_cursors.insert(next.clone()) {
                return Err(SessionError::Protocol(
                    "model/list returned a cursor cycle".to_owned(),
                ));
            }
            cursor = Some(next);
        }
        Ok(models)
    }

    pub async fn list_threads(&self) -> Result<Vec<ThreadListEntry>, SessionError> {
        let mut cursor = None;
        let mut seen_cursors = HashSet::new();
        let mut seen_threads = HashSet::new();
        let mut threads = Vec::new();
        loop {
            let response: ThreadListResponse = decode(
                "thread/list",
                self.transport
                    .request_default(
                        "thread/list",
                        ThreadListParams {
                            source_kinds: vec!["appServer".to_owned()],
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
            for thread in response.data {
                if thread.id.trim().is_empty() {
                    return Err(SessionError::Protocol(
                        "thread/list returned an empty thread id".to_owned(),
                    ));
                }
                if thread.cwd != self.paths.conversation {
                    return Err(SessionError::Protocol(
                        "thread/list returned a thread outside the AgentHarness working directory"
                            .to_owned(),
                    ));
                }
                if !thread.ephemeral && seen_threads.insert(thread.id.clone()) {
                    threads.push(thread);
                }
            }
            let Some(next) = response.next_cursor else {
                break;
            };
            if next.is_empty() || !seen_cursors.insert(next.clone()) {
                return Err(SessionError::Protocol(
                    "thread/list returned an invalid cursor cycle".to_owned(),
                ));
            }
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
                        sandbox: "read-only".to_owned(),
                        model: model.to_owned(),
                    },
                )
                .await?,
        )?;
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
        decode(
            "turn/start",
            self.transport
                .request_default(
                    "turn/start",
                    TurnStartParams {
                        thread_id: thread_id.to_owned(),
                        input: vec![UserInput::text(text)],
                        model: model.to_owned(),
                        effort: effort.to_owned(),
                        approval_policy: "never".to_owned(),
                        cwd: self.paths.conversation.clone(),
                        sandbox_policy: overrides["sandboxPolicy"].clone(),
                    },
                )
                .await?,
        )
    }

    pub async fn interrupt_turn(&self, thread_id: &str, turn_id: &str) -> Result<(), SessionError> {
        self.transport
            .request_default(
                "turn/interrupt",
                TurnInterruptParams {
                    thread_id: thread_id.to_owned(),
                    turn_id: turn_id.to_owned(),
                },
            )
            .await?;
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
            approval_policy: "never".to_owned(),
            config: overrides["config"].clone(),
            cwd: self.paths.conversation.clone(),
            sandbox: "read-only".to_owned(),
            model: model.to_owned(),
        }
    }
}

fn decode<T: DeserializeOwned>(method: &'static str, value: Value) -> Result<T, SessionError> {
    serde_json::from_value(value).map_err(|_| SessionError::Decode { method })
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

    use super::{history_entries, model_choices, thread_choices};
    use crate::codex::protocol::{
        ModelInfo, ReasoningEffortOption, ThreadItem, ThreadListEntry, ThreadSnapshot,
        TurnSnapshot, TurnStatus, UserInput,
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
                        content: vec![UserInput::text("hello")],
                    },
                    ThreadItem {
                        id: "tool".to_owned(),
                        kind: "commandExecution".to_owned(),
                        text: None,
                        content: vec![],
                    },
                    ThreadItem {
                        id: "a".to_owned(),
                        kind: "agentMessage".to_owned(),
                        text: Some("hi".to_owned()),
                        content: vec![],
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
            },
            ThreadListEntry {
                id: "preview".to_owned(),
                name: None,
                preview: "  First line\nsecond line".to_owned(),
                created_at: 1,
                updated_at: 2,
                cwd: PathBuf::from("/tmp/conversation"),
                ephemeral: false,
            },
            ThreadListEntry {
                id: "empty".to_owned(),
                name: Some(" ".to_owned()),
                preview: String::new(),
                created_at: 1,
                updated_at: 1,
                cwd: PathBuf::from("/tmp/conversation"),
                ephemeral: false,
            },
        ]);
        assert_eq!(choices[0].title, "A name");
        assert_eq!(choices[1].title, "First line");
        assert_eq!(choices[2].title, "Untitled thread");
    }
}
