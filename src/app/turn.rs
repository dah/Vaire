use super::*;

impl AppState {
    pub(in crate::app) fn reduce_turn_event(&mut self, event: DomainEvent) -> Vec<Effect> {
        match event {
            DomainEvent::TurnStarted { thread_id, turn_id } => {
                let thread_matches = matches!(
                    &self.thread,
                    ThreadState::Ready { id } if id == &thread_id
                );
                let turn_matches = match &self.turn {
                    TurnState::Starting => true,
                    TurnState::Streaming {
                        thread_id: active_thread,
                        turn_id: active_turn,
                    } => active_thread == &thread_id && active_turn == &turn_id,
                    _ => false,
                };
                if thread_matches && turn_matches {
                    let is_suppressed_turn = self.context_suppressed_turn.as_ref().is_some_and(
                        |(suppressed_thread, suppressed_turn)| {
                            suppressed_thread == &thread_id && suppressed_turn == &turn_id
                        },
                    );
                    if !is_suppressed_turn {
                        self.context_suppressed_turn = None;
                    }
                    self.turn = TurnState::Streaming { thread_id, turn_id };
                }
            }
            DomainEvent::AgentDelta {
                thread_id,
                turn_id,
                item_id,
                delta,
            } => {
                if self.matches_active(&thread_id, &turn_id) {
                    self.append_delta(&turn_id, &item_id, &delta);
                }
            }
            DomainEvent::AgentCompleted {
                thread_id,
                turn_id,
                item_id,
                text,
            } => {
                if self.matches_active(&thread_id, &turn_id) {
                    if let Err(message) = self.reconcile_final(&turn_id, &item_id, &text) {
                        self.turn = TurnState::Failed {
                            turn_id: Some(turn_id),
                            message: message.clone(),
                        };
                        self.notice = Some(message);
                    }
                }
            }
            DomainEvent::ThinkingSummaryPartAdded {
                thread_id,
                turn_id,
                item_id,
                summary_index,
            } => {
                if self.matches_active(&thread_id, &turn_id) {
                    self.thinking.add_part(&turn_id, &item_id, summary_index);
                }
            }
            DomainEvent::ThinkingDelta {
                thread_id,
                turn_id,
                item_id,
                kind,
                index,
                delta,
            } => {
                if self.matches_active(&thread_id, &turn_id) {
                    self.thinking
                        .append_delta(&turn_id, &item_id, kind, index, &delta);
                }
            }
            DomainEvent::ThinkingCompleted {
                thread_id,
                turn_id,
                item_id,
                summary,
                content,
            } => {
                if self.matches_active(&thread_id, &turn_id) {
                    self.thinking
                        .reconcile_item(&turn_id, &item_id, &summary, &content);
                }
            }
            DomainEvent::TurnFinished {
                thread_id,
                turn_id,
                outcome,
            } => {
                if self.matches_active(&thread_id, &turn_id) {
                    self.turn = match outcome {
                        TurnOutcome::Completed => TurnState::Completed { turn_id },
                        TurnOutcome::Interrupted => TurnState::Interrupted { turn_id },
                        TurnOutcome::Failed(message) => TurnState::Failed {
                            turn_id: Some(turn_id),
                            message,
                        },
                    };
                }
            }
            DomainEvent::TokenUsageUpdated {
                thread_id,
                turn_id,
                context_tokens,
                model_context_window,
            } => {
                let suppressed = self.context_suppressed_turn.as_ref().is_some_and(
                    |(suppressed_thread, suppressed_turn)| {
                        suppressed_thread == &thread_id && suppressed_turn == &turn_id
                    },
                );
                if !suppressed && self.matches_relevant_turn(&thread_id, &turn_id) {
                    self.context_remaining_percent =
                        remaining_context_percent(context_tokens, model_context_window);
                }
            }
            DomainEvent::TurnOperationFailed(message) => {
                if !self.turn.is_active() {
                    return Vec::new();
                }
                let turn_id = match &self.turn {
                    TurnState::Streaming { turn_id, .. } => Some(turn_id.clone()),
                    _ => None,
                };
                self.turn = TurnState::Failed {
                    turn_id,
                    message: message.clone(),
                };
                self.notice = Some(message);
            }
            DomainEvent::SafetyViolation(method) => {
                self.connection =
                    ConnectionState::Failed("runtime request boundary was triggered".to_owned());
                self.pending_new_thread_scope = None;
                self.pending_thread_deletions = None;
                if let Some(picker) = self.conversation_popup_mut() {
                    picker.phase = ThreadPickerPhase::Failed;
                    picker.confirmation = None;
                    picker.message = Some("Unexpected server request was denied".to_owned());
                }
                if self.turn.is_active() {
                    let turn_id = match &self.turn {
                        TurnState::Streaming { turn_id, .. } => Some(turn_id.clone()),
                        _ => None,
                    };
                    self.turn = TurnState::Failed {
                        turn_id,
                        message: "unexpected server request was denied".to_owned(),
                    };
                }
                self.notice = Some(format!("unexpected server request denied: {method}"));
            }
            _ => unreachable!("event routed to the wrong reducer"),
        }
        Vec::new()
    }
}

impl AppState {
    pub(in crate::app) fn matches_active(&self, thread_id: &str, turn_id: &str) -> bool {
        matches!(&self.turn, TurnState::Streaming { thread_id: expected_thread, turn_id: expected_turn } if expected_thread == thread_id && expected_turn == turn_id)
    }

    pub(in crate::app) fn matches_relevant_turn(&self, thread_id: &str, turn_id: &str) -> bool {
        let thread_matches = matches!(&self.thread, ThreadState::Ready { id } if id == thread_id);
        thread_matches
            && match &self.turn {
                TurnState::Streaming {
                    thread_id: active_thread,
                    turn_id: active_turn,
                } => active_thread == thread_id && active_turn == turn_id,
                TurnState::OpenRouterStreaming { .. } | TurnState::ClaudeStreaming { .. } => false,
                TurnState::Completed {
                    turn_id: active_turn,
                }
                | TurnState::Interrupted {
                    turn_id: active_turn,
                } => active_turn == turn_id,
                TurnState::Failed {
                    turn_id: Some(active_turn),
                    ..
                } => active_turn == turn_id,
                TurnState::Idle | TurnState::Starting | TurnState::Failed { turn_id: None, .. } => {
                    false
                }
            }
    }

    pub(in crate::app) fn reset_context_window(&mut self) {
        self.context_remaining_percent = None;
        self.context_suppressed_turn = None;
    }

    pub(in crate::app) fn invalidate_context_for_current_turn(&mut self) {
        self.context_remaining_percent = None;
        self.context_suppressed_turn = self.current_turn_key();
    }

    pub(in crate::app) fn current_turn_key(&self) -> Option<(String, String)> {
        let ThreadState::Ready { id: thread_id } = &self.thread else {
            return None;
        };
        let turn_id = match &self.turn {
            TurnState::Streaming { turn_id, .. }
            | TurnState::Completed { turn_id }
            | TurnState::Interrupted { turn_id }
            | TurnState::Failed {
                turn_id: Some(turn_id),
                ..
            } => turn_id,
            TurnState::OpenRouterStreaming { .. } | TurnState::ClaudeStreaming { .. } => {
                return None
            }
            TurnState::Idle | TurnState::Starting | TurnState::Failed { turn_id: None, .. } => {
                return None;
            }
        };
        Some((thread_id.clone(), turn_id.clone()))
    }
}

pub fn remaining_context_percent(
    context_tokens: i64,
    model_context_window: Option<i64>,
) -> Option<u8> {
    let context_window = u128::try_from(model_context_window?).ok()?;
    let consumed = u128::try_from(context_tokens).ok()?;
    if context_window == 0 {
        return None;
    }
    let remaining = context_window.saturating_sub(consumed.min(context_window));
    let rounded = (remaining * 100 + context_window / 2) / context_window;
    u8::try_from(rounded).ok()
}
