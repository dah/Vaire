use super::*;
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
                effects.push(Effect::Shutdown);
                return effects;
            }
            Intent::ShowModels => {
                self.notice = Some(if self.models.is_empty() {
                    "model catalog is not available".to_owned()
                } else {
                    self.models
                        .iter()
                        .map(|model| model.id.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                });
            }
            Intent::ShowReasoning => {
                self.notice = Some(self.current_model().map_or_else(
                    || "select a model first".to_owned(),
                    |model| model.supported_reasoning_efforts.join(", "),
                ));
            }
            Intent::ToggleThinking => {
                self.thinking.visible = !self.thinking.visible;
            }
            Intent::SelectModel(id) => {
                let Some(model) = self.models.iter().find(|model| model.id == id).cloned() else {
                    self.notice = Some(format!("unknown model {id}; use /model"));
                    return Vec::new();
                };
                let model_changed = self.selected_model.as_deref() != Some(model.id.as_str());
                let old_reasoning = self.selected_reasoning.clone();
                self.selected_model = Some(model.id.clone());
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
                if let Some(reason) = self.thread_action_block_reason(true) {
                    self.notice = Some(reason);
                } else {
                    self.pending_thread_deletions = None;
                    self.thread_picker = Some(ThreadPickerState::loading());
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
                if let Some(picker) = &mut self.thread_picker {
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
                        role: TranscriptRole::User,
                        text: text.clone(),
                        item_id: None,
                        turn_id: None,
                    });
                    self.enforce_transcript_bound();
                    return vec![Effect::SendMessage { text }];
                }
            }
            Intent::Interrupt => {
                if let TurnState::Streaming { thread_id, turn_id } = &self.turn {
                    return vec![Effect::InterruptTurn {
                        thread_id: thread_id.clone(),
                        turn_id: turn_id.clone(),
                    }];
                }
                self.notice = Some("there is no active turn to interrupt".to_owned());
            }
        }
        Vec::new()
    }
}
