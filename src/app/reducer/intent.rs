use super::*;
use crate::app::popup::{catalog_search_matches, model_search_matches, POPUP_PAGE_ROWS};
use crate::app::transcript::MAX_MESSAGE_BYTES;

impl AppState {
    pub(in crate::app) fn reduce_intent(&mut self, intent: Intent) -> Vec<Effect> {
        self.notice = None;
        match intent {
            Intent::Help => self.notice = Some(HELP_TEXT.to_owned()),
            Intent::Quit => {
                self.shutting_down = true;
                self.pending_new_thread_scope = None;
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
            Intent::ShowReasoning => {
                if self.active_provider == ProviderId::OpenRouter {
                    self.notice = Some(
                        "OpenRouter reasoning effort is unsupported in this milestone".to_owned(),
                    );
                    return Vec::new();
                }
                self.notice = Some(self.current_model().map_or_else(
                    || "select a model first".to_owned(),
                    |model| model.supported_reasoning_efforts.join(", "),
                ));
            }
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
            Intent::PopupRefresh => return vec![Effect::RefreshOpenRouter],
            Intent::PopupSelect => return self.select_popup(),
            Intent::SelectModel(id) => {
                let Ok(key) = ModelKey::codex(id.clone()) else {
                    self.notice = Some(format!("unknown model {id}; use /model"));
                    return Vec::new();
                };
                return self.reduce_intent(Intent::SelectProviderModel(key));
            }
            Intent::SelectProviderModel(key) => {
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
                    self.active_provider = key.provider;
                    self.preferences.active_provider = key.provider;
                    self.preferences.clear_auto_resume();
                    self.thread = ThreadState::None;
                    self.openrouter.conversation = OpenRouterConversationState::None;
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
                        ProviderId::OpenRouter => None,
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
            Intent::SelectReasoning(effort) => {
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
                if self.turn.is_active() {
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
                        text: text.clone(),
                        item_id: None,
                        turn_id: None,
                    });
                    self.enforce_transcript_bound();
                    return vec![match self.active_provider {
                        ProviderId::Codex => Effect::SendMessage { text },
                        ProviderId::OpenRouter => Effect::SendOpenRouterMessage { text },
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
                self.notice = Some("there is no active turn to interrupt".to_owned());
            }
        }
        Vec::new()
    }
}

impl AppState {
    fn move_popup_selection(&mut self, intent: Intent) {
        let Some(popup) = &mut self.popup else {
            return;
        };
        let movement = |selected: usize, count: usize| match intent {
            Intent::PopupMoveUp => move_bounded(selected, -1, count),
            Intent::PopupMoveDown => move_bounded(selected, 1, count),
            Intent::PopupPageUp => move_bounded(selected, -POPUP_PAGE_ROWS, count),
            Intent::PopupPageDown => move_bounded(selected, POPUP_PAGE_ROWS, count),
            Intent::PopupMoveFirst => 0,
            Intent::PopupMoveLast => count.saturating_sub(1),
            _ => selected,
        };
        match popup {
            PopupState::Auth { selected, .. } => {
                if matches!(intent, Intent::PopupMoveUp | Intent::PopupMoveDown) {
                    *selected = match selected {
                        ProviderId::Codex => ProviderId::OpenRouter,
                        ProviderId::OpenRouter => ProviderId::Codex,
                    };
                }
            }
            PopupState::Model {
                choices,
                selected,
                search,
            } => {
                let count = choices
                    .iter()
                    .filter(|key| model_search_matches(key, search))
                    .count();
                *selected = movement(*selected, count);
            }
            PopupState::OpenRouterCatalog {
                models,
                selected,
                search,
                ..
            } => {
                let count = models
                    .iter()
                    .filter(|model| catalog_search_matches(model, search))
                    .count();
                *selected = movement(*selected, count);
            }
            PopupState::OpenRouterSecret | PopupState::Conversation(_) => {}
        }
    }

    fn toggle_catalog_choice(&mut self) {
        let Some(PopupState::OpenRouterCatalog {
            models,
            draft_enabled,
            selected,
            search,
        }) = &mut self.popup
        else {
            return;
        };
        let Some(model) = models
            .iter()
            .filter(|model| catalog_search_matches(model, search))
            .nth(*selected)
        else {
            return;
        };
        if !draft_enabled.remove(&model.id) {
            draft_enabled.insert(model.id.clone());
        }
    }

    fn select_popup(&mut self) -> Vec<Effect> {
        let Some(popup) = self.popup.clone() else {
            return Vec::new();
        };
        match popup {
            PopupState::Auth { mode, selected } => match (mode, selected) {
                (AuthPopupMode::Login, ProviderId::Codex) => {
                    self.popup = None;
                    self.reduce_intent(Intent::Login)
                }
                (AuthPopupMode::Login, ProviderId::OpenRouter) => {
                    self.popup = Some(PopupState::OpenRouterSecret);
                    Vec::new()
                }
                (AuthPopupMode::Logout, ProviderId::Codex) => {
                    self.popup = None;
                    self.reduce_intent(Intent::Logout)
                }
                (AuthPopupMode::Logout, ProviderId::OpenRouter) => {
                    self.popup = None;
                    vec![Effect::LogoutOpenRouter]
                }
            },
            PopupState::Model {
                choices,
                selected,
                search,
            } => {
                let choice = choices
                    .into_iter()
                    .filter(|key| model_search_matches(key, &search))
                    .nth(selected);
                self.popup = None;
                choice.map_or_else(Vec::new, |key| {
                    self.reduce_intent(Intent::SelectProviderModel(key))
                })
            }
            PopupState::OpenRouterCatalog { draft_enabled, .. } => {
                let previous_model = self.selected_model.clone();
                self.preferences.openrouter.enabled_model_ids = draft_enabled;
                self.popup = None;
                self.validate_openrouter_selection();
                if self.active_provider == ProviderId::OpenRouter
                    && previous_model != self.selected_model
                {
                    self.reset_context_window();
                }
                vec![Effect::Persist(self.preferences.clone())]
            }
            PopupState::OpenRouterSecret | PopupState::Conversation(_) => Vec::new(),
        }
    }
}

fn move_bounded(selected: usize, delta: isize, count: usize) -> usize {
    if count == 0 {
        return 0;
    }
    selected
        .saturating_add_signed(delta)
        .min(count.saturating_sub(1))
}
