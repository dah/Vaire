use super::*;
use crate::app::popup::{catalog_search_matches, model_search_matches, POPUP_PAGE_ROWS};
use crate::app::transcript::MAX_MESSAGE_BYTES;

mod popup;

impl AppState {
    pub(in crate::app) fn reduce_intent(&mut self, intent: Intent) -> Vec<Effect> {
        self.notice = None;
        match intent {
            Intent::Help => self.notice = Some(HELP_TEXT.to_owned()),
            Intent::Quit => {
                self.shutting_down = true;
                self.pending_new_thread_scope = None;
                self.pending_new_claude_session = false;
                self.pending_thread_deletions = None;
                let mut effects = Vec::new();
                if let TurnState::Streaming { thread_id, turn_id } = &self.turn {
                    effects.push(Effect::InterruptTurn {
                        thread_id: thread_id.clone(),
                        turn_id: turn_id.clone(),
                    });
                }
                if matches!(self.turn, TurnState::OpenRouterStreaming { .. }) {
                    effects.push(Effect::InterruptOpenRouterTurn);
                }
                if matches!(self.turn, TurnState::ClaudeStreaming { .. }) {
                    effects.push(Effect::InterruptClaudeTurn);
                }
                effects.push(Effect::Shutdown);
                return effects;
            }
            Intent::ShowModels => {
                let choices = self.available_model_keys();
                if choices.is_empty() {
                    self.notice = Some("model catalog is not available".to_owned());
                } else {
                    let selected = choices
                        .iter()
                        .position(|key| self.selected_model.as_ref() == Some(key))
                        .unwrap_or(0);
                    self.popup = Some(PopupState::Model {
                        choices,
                        selected,
                        search: String::new(),
                    });
                }
            }
            Intent::ShowReasoning => match self.active_provider {
                ProviderId::Codex => {
                    self.notice = Some(self.current_model().map_or_else(
                        || "select a model first".to_owned(),
                        |model| model.supported_reasoning_efforts.join(", "),
                    ));
                }
                ProviderId::OpenRouter => {
                    self.notice = Some("OpenRouter reasoning effort is unsupported".to_owned());
                }
                ProviderId::Claude => {
                    let current = self
                        .preferences
                        .claude
                        .selected_effort
                        .map_or("default", ClaudeEffort::as_str);
                    self.notice = Some(format!(
                        "Claude effort: {current}; choices: default, low, medium, high, xhigh, max"
                    ));
                }
            },
            Intent::ToggleThinking => {
                self.thinking.visible = !self.thinking.visible;
            }
            Intent::ShowLogin => {
                self.popup = Some(PopupState::Auth {
                    mode: AuthPopupMode::Login,
                    selected: self.active_provider,
                });
            }
            Intent::ShowLogout => {
                self.popup = Some(PopupState::Auth {
                    mode: AuthPopupMode::Logout,
                    selected: self.active_provider,
                });
            }
            Intent::PopupClose => self.popup = None,
            Intent::PopupMoveUp
            | Intent::PopupMoveDown
            | Intent::PopupPageUp
            | Intent::PopupPageDown
            | Intent::PopupMoveFirst
            | Intent::PopupMoveLast => {
                self.move_popup_selection(intent);
            }
            Intent::PopupSearchAppend(character) => {
                if let Some(
                    PopupState::Model {
                        search, selected, ..
                    }
                    | PopupState::OpenRouterCatalog {
                        search, selected, ..
                    },
                ) = &mut self.popup
                {
                    if search.len().saturating_add(character.len_utf8()) <= 256
                        && !character.is_control()
                    {
                        search.push(character);
                        *selected = 0;
                    }
                }
            }
            Intent::PopupSearchBackspace => {
                if let Some(
                    PopupState::Model {
                        search, selected, ..
                    }
                    | PopupState::OpenRouterCatalog {
                        search, selected, ..
                    },
                ) = &mut self.popup
                {
                    search.pop();
                    *selected = 0;
                }
            }
            Intent::PopupCatalogToggle => self.toggle_catalog_choice(),
            Intent::PopupOpenCatalog => {
                if self.openrouter.catalog.is_empty() {
                    self.notice = Some("refresh the OpenRouter catalog first".to_owned());
                } else {
                    self.popup = Some(PopupState::OpenRouterCatalog {
                        models: self.openrouter.catalog.clone(),
                        draft_enabled: self.preferences.openrouter.enabled_model_ids.clone(),
                        selected: 0,
                        search: String::new(),
                    });
                }
            }
            Intent::PopupRefresh => {
                return match self.popup.as_ref().and_then(PopupState::selected_provider) {
                    Some(ProviderId::Claude)
                        if matches!(self.claude.auth_operation, ClaudeAuthOperation::Idle) =>
                    {
                        vec![Effect::RefreshClaude]
                    }
                    Some(ProviderId::Claude) => {
                        self.notice = Some(
                            "wait for the current Claude authentication operation to finish"
                                .to_owned(),
                        );
                        Vec::new()
                    }
                    _ => vec![Effect::RefreshOpenRouter],
                };
            }
            Intent::PopupSelect => return self.select_popup(),
            Intent::SelectModel(id) => {
                let Ok(key) = ModelKey::codex(id.clone()) else {
                    self.notice = Some(format!("unknown model {id}; use /model"));
                    return Vec::new();
                };
                return self.reduce_intent(Intent::SelectProviderModel(key));
            }
            Intent::SelectProviderModel(key) => {
                if self.pending_new_claude_session {
                    self.notice =
                        Some("wait for the new Claude conversation request to finish".to_owned());
                    return Vec::new();
                }
                if !self.model_key_is_available(&key) {
                    self.notice = Some(format!("unknown model {}; use /model", key.id));
                    return Vec::new();
                }
                let switching_provider = key.provider != self.active_provider;
                if switching_provider {
                    if self.turn.is_active() {
                        self.notice = Some("wait for or interrupt the active turn".to_owned());
                        return Vec::new();
                    }
                    self.pending_new_claude_session = false;
                    self.active_provider = key.provider;
                    self.preferences.active_provider = key.provider;
                    self.preferences.clear_auto_resume();
                    self.thread = ThreadState::None;
                    self.openrouter.conversation = OpenRouterConversationState::None;
                    self.claude.conversation = ClaudeConversationState::None;
                    self.claude.resolved_model = None;
                    self.turn = TurnState::Idle;
                    self.clear_transcript();
                    self.thinking.clear_content();
                    self.reset_context_window();
                    self.selected_model = Some(key.clone());
                    self.selected_reasoning = match key.provider {
                        ProviderId::Codex => self
                            .models
                            .iter()
                            .find(|model| model.id == key.id)
                            .map(|model| model.default_reasoning_effort.clone()),
                        ProviderId::OpenRouter | ProviderId::Claude => None,
                    };
                    self.sync_active_selection_preferences();
                    self.notice = Some(
                        "Switching provider starts a new conversation; use /resume for history."
                            .to_owned(),
                    );
                    return vec![Effect::Persist(self.preferences.clone())];
                }
                if key.provider == ProviderId::OpenRouter {
                    let changed = self.selected_model.as_ref() != Some(&key);
                    self.selected_model = Some(key);
                    self.selected_reasoning = None;
                    if changed {
                        self.invalidate_context_for_current_turn();
                    }
                    self.sync_active_selection_preferences();
                    return vec![Effect::Persist(self.preferences.clone())];
                }
                if key.provider == ProviderId::Claude {
                    let changed = self.selected_model.as_ref() != Some(&key);
                    self.selected_model = Some(key);
                    self.selected_reasoning = None;
                    if changed {
                        self.claude.conversation = ClaudeConversationState::None;
                        self.claude.resolved_model = None;
                        self.preferences.set_auto_resume_claude_session(None);
                        self.clear_transcript();
                        self.thinking.clear_content();
                        self.reset_context_window();
                        self.notice = Some(
                            "Changing Claude model starts a new conversation; use /resume for history."
                                .to_owned(),
                        );
                    }
                    self.sync_active_selection_preferences();
                    return vec![Effect::Persist(self.preferences.clone())];
                }
                let model = self
                    .models
                    .iter()
                    .find(|model| model.id == key.id)
                    .cloned()
                    .expect("availability checked above");
                let model_key = model.key();
                let model_changed = self.selected_model.as_ref() != Some(&model_key);
                let old_reasoning = self.selected_reasoning.clone();
                self.selected_model = Some(model_key);
                self.selected_reasoning = old_reasoning
                    .clone()
                    .filter(|effort| model.supported_reasoning_efforts.contains(effort))
                    .or_else(|| Some(model.default_reasoning_effort.clone()));
                if old_reasoning.is_some() && old_reasoning != self.selected_reasoning {
                    self.notice =
                        Some("reasoning was reset to the selected model's default".to_owned());
                }
                if model_changed {
                    self.invalidate_context_for_current_turn();
                }
                self.sync_selection_preferences();
                return vec![Effect::Persist(self.preferences.clone())];
            }
            Intent::RefreshOpenRouter => return vec![Effect::RefreshOpenRouter],
            Intent::LogoutOpenRouter => return vec![Effect::LogoutOpenRouter],
            Intent::RefreshClaude => {
                if matches!(self.claude.auth_operation, ClaudeAuthOperation::Idle) {
                    return vec![Effect::RefreshClaude];
                }
                self.notice = Some(
                    "wait for the current Claude authentication operation to finish".to_owned(),
                );
            }
            Intent::LogoutClaude => {
                if matches!(self.claude.auth_operation, ClaudeAuthOperation::Idle) {
                    return vec![Effect::LogoutClaude];
                }
                self.notice = Some(
                    "wait for the current Claude authentication operation to finish".to_owned(),
                );
            }
            Intent::SelectReasoning(effort) => {
                match self.active_provider {
                    ProviderId::OpenRouter => {
                        self.notice = Some("OpenRouter reasoning effort is unsupported".to_owned());
                        return Vec::new();
                    }
                    ProviderId::Claude => {
                        if self.turn.is_active() {
                            self.notice = Some("wait for or interrupt the active turn".to_owned());
                            return Vec::new();
                        }
                        if self.pending_new_claude_session {
                            self.notice = Some(
                                "wait for the new Claude conversation request to finish".to_owned(),
                            );
                            return Vec::new();
                        }
                        let selected_effort = if effort == "default" {
                            None
                        } else {
                            let Ok(selected) = effort.parse::<ClaudeEffort>() else {
                                self.notice = Some(format!(
                                    "unsupported Claude effort {effort}; use /reasoning"
                                ));
                                return Vec::new();
                            };
                            Some(selected)
                        };
                        if self.preferences.claude.selected_effort == selected_effort {
                            self.notice = Some(format!(
                                "Claude effort is already {}; it applies to the next turn",
                                selected_effort.map_or("default", ClaudeEffort::as_str)
                            ));
                            return Vec::new();
                        }
                        self.preferences.claude.selected_effort = selected_effort;
                        self.notice = Some(format!(
                            "Claude effort set to {}; it applies to the next turn",
                            selected_effort.map_or("default", ClaudeEffort::as_str)
                        ));
                        return vec![Effect::Persist(self.preferences.clone())];
                    }
                    ProviderId::Codex => {}
                }
                let Some(model) = self.current_model() else {
                    self.notice = Some("select a model first".to_owned());
                    return Vec::new();
                };
                if !model.supported_reasoning_efforts.contains(&effort) {
                    self.notice = Some(format!("unsupported reasoning {effort}; use /reasoning"));
                    return Vec::new();
                }
                self.selected_reasoning = Some(effort);
                self.sync_selection_preferences();
                return vec![Effect::Persist(self.preferences.clone())];
            }
            Intent::Login => return self.begin_login(Effect::StartLogin),
            Intent::LoginDevice => return self.begin_login(Effect::StartDeviceLogin),
            Intent::Logout => {
                match &self.auth {
                    AuthState::SigningIn { login_id } => {
                        return vec![Effect::CancelLogin {
                            login_id: login_id.clone(),
                        }];
                    }
                    AuthState::SignedIn { .. } => return vec![Effect::Logout],
                    _ => {}
                }
                self.notice = Some("not signed in".to_owned());
            }
            Intent::NewThread => {
                if self.active_provider == ProviderId::Claude {
                    let reason = if !matches!(self.claude.availability, ClaudeAvailability::Ready) {
                        Some("Claude Code runtime is not available".to_owned())
                    } else if !matches!(self.claude.auth_operation, ClaudeAuthOperation::Idle) {
                        Some(
                            "wait for the current Claude authentication operation to finish"
                                .to_owned(),
                        )
                    } else if self.claude.auth != ClaudeAuthStatus::Subscription {
                        Some(
                            "sign in to a Claude subscription with /login before starting a conversation"
                                .to_owned(),
                        )
                    } else if self.selected_model.as_ref().is_none_or(|key| {
                        key.provider != ProviderId::Claude || !self.model_key_is_available(key)
                    }) {
                        Some("select a Claude model alias with /model".to_owned())
                    } else if self.turn.is_active() {
                        Some("wait for or interrupt the active turn".to_owned())
                    } else if self.conversation_popup().is_some() {
                        Some(
                            "close the conversation picker before starting a conversation"
                                .to_owned(),
                        )
                    } else if self.pending_new_claude_session {
                        Some("wait for the new Claude conversation request to finish".to_owned())
                    } else {
                        None
                    };
                    if let Some(reason) = reason {
                        self.notice = Some(reason);
                    } else {
                        self.pending_new_claude_session = true;
                        self.notice = Some("Starting a new Claude conversation…".to_owned());
                        return vec![Effect::StartNewClaudeSession];
                    }
                    return Vec::new();
                }
                if self.active_provider == ProviderId::OpenRouter {
                    if let Some(reason) = self.send_block_reason() {
                        self.notice = Some(reason);
                    } else {
                        self.notice = Some("Starting a new OpenRouter conversation…".to_owned());
                        return vec![Effect::StartNewOpenRouterConversation];
                    }
                    return Vec::new();
                }
                if let Some(reason) = self.thread_action_block_reason(false) {
                    self.notice = Some(reason);
                } else {
                    let AuthState::SignedIn { scope } = &self.auth else {
                        unreachable!("thread_action_block_reason requires signed-in auth")
                    };
                    self.pending_new_thread_scope = Some(scope.clone());
                    self.notice = Some("Starting a new thread…".to_owned());
                    return vec![Effect::StartNewThread];
                }
            }
            Intent::Resume => {
                if self.pending_new_claude_session {
                    self.notice =
                        Some("wait for the new Claude conversation request to finish".to_owned());
                } else if self.turn.is_active() {
                    self.notice = Some("wait for or interrupt the active turn".to_owned());
                } else if self.popup.is_some() {
                    self.notice = Some("close the current popup before opening history".to_owned());
                } else {
                    self.pending_thread_deletions = None;
                    self.popup = Some(PopupState::Conversation(ThreadPickerState::loading()));
                    return vec![Effect::ListThreads];
                }
            }
            Intent::ThreadPickerMoveUp => self.move_thread_picker(-1),
            Intent::ThreadPickerMoveDown => self.move_thread_picker(1),
            Intent::ThreadPickerSelect => return self.select_thread_picker(),
            Intent::ThreadPickerClose => self.close_thread_picker(),
            Intent::ThreadPickerRequestDelete => self.request_selected_thread_delete(),
            Intent::ThreadPickerRequestClearInactive => self.request_clear_inactive_threads(),
            Intent::ThreadPickerConfirmDelete => return self.confirm_thread_delete(),
            Intent::ThreadPickerCancelDelete => {
                if let Some(picker) = self.conversation_popup_mut() {
                    if picker.confirmation.take().is_some() {
                        picker.message = Some("Deletion cancelled".to_owned());
                    }
                }
            }
            Intent::SendMessage(text) => {
                let text = sanitize_terminal_text(&text);
                if text.trim().is_empty() {
                    self.notice = Some("enter a message or /help".to_owned());
                    return Vec::new();
                }
                if text.len() > MAX_MESSAGE_BYTES {
                    self.notice = Some(format!(
                        "message is too large; keep it under {} KiB",
                        MAX_MESSAGE_BYTES / 1024
                    ));
                    return Vec::new();
                }
                if let Some(reason) = self.send_block_reason() {
                    self.notice = Some(reason);
                } else {
                    self.turn = TurnState::Starting;
                    self.thinking.clear_content();
                    self.transcript_dropped_prefix_bytes.clear();
                    self.transcript.push(TranscriptEntry {
                        provider: self.active_provider,
                        role: TranscriptRole::User,
                        status: TranscriptEntryStatus::Normal,
                        text: text.clone(),
                        item_id: None,
                        turn_id: None,
                    });
                    self.enforce_transcript_bound();
                    return vec![match self.active_provider {
                        ProviderId::Codex => Effect::SendMessage { text },
                        ProviderId::OpenRouter => Effect::SendOpenRouterMessage { text },
                        ProviderId::Claude => Effect::SendClaudeMessage {
                            text,
                            effort: self.preferences.claude.selected_effort,
                        },
                    }];
                }
            }
            Intent::Interrupt => {
                if let TurnState::Streaming { thread_id, turn_id } = &self.turn {
                    return vec![Effect::InterruptTurn {
                        thread_id: thread_id.clone(),
                        turn_id: turn_id.clone(),
                    }];
                }
                if matches!(self.turn, TurnState::OpenRouterStreaming { .. }) {
                    return vec![Effect::InterruptOpenRouterTurn];
                }
                if matches!(self.turn, TurnState::ClaudeStreaming { .. }) {
                    return vec![Effect::InterruptClaudeTurn];
                }
                self.notice = Some("there is no active turn to interrupt".to_owned());
            }
        }
        Vec::new()
    }
}
