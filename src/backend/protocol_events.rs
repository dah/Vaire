use super::*;

impl<P: PreferencesPort, B: BrowserOpener> BackendCoordinator<P, B> {
    pub(in crate::backend) async fn reduce_protocol_event(
        &mut self,
        event: ProtocolEvent,
    ) -> Result<Vec<Effect>, BackendError> {
        Ok(match event {
            ProtocolEvent::AccountLoginCompleted(completed) => {
                let expected_login_id = match &self.state.auth {
                    crate::app::AuthState::SigningIn { login_id } => login_id.clone(),
                    _ => return Ok(Vec::new()),
                };
                let correlated = completed
                    .login_id
                    .as_deref()
                    .is_none_or(|completed| completed == expected_login_id.as_str());
                if !correlated {
                    self.state.notice =
                        Some("ignored a stale or mismatched ChatGPT login completion".to_owned());
                    Vec::new()
                } else if completed.success {
                    let account = self.session.read_account().await?;
                    self.reduce_account(account)
                } else {
                    self.state.reduce(Action::Event(DomainEvent::LoginFailed(
                        completed
                            .error
                            .unwrap_or_else(|| "ChatGPT login was cancelled".to_owned()),
                    )))
                }
            }
            ProtocolEvent::AccountUpdated => {
                let account = self.session.read_account().await?;
                self.reduce_account(account)
            }
            ProtocolEvent::ThreadStarted(_) => Vec::new(),
            ProtocolEvent::TurnStarted(notification) => {
                let thread_id = notification.thread_id;
                let turn_id = notification.turn.id;
                let effects = self.state.reduce(Action::Event(DomainEvent::TurnStarted {
                    thread_id: thread_id.clone(),
                    turn_id: turn_id.clone(),
                }));
                if matches!(
                    &self.state.turn,
                    crate::app::TurnState::Streaming {
                        thread_id: active_thread,
                        turn_id: active_turn,
                    } if active_thread == &thread_id && active_turn == &turn_id
                ) {
                    self.completed_items.observe_turn(&thread_id, &turn_id);
                }
                effects
            }
            ProtocolEvent::AgentMessageDelta(delta) => {
                if self.completed_items.should_ignore(
                    &delta.thread_id,
                    &delta.turn_id,
                    &delta.item_id,
                ) {
                    return Ok(Vec::new());
                }
                self.state.reduce(Action::Event(DomainEvent::AgentDelta {
                    thread_id: delta.thread_id,
                    turn_id: delta.turn_id,
                    item_id: delta.item_id,
                    delta: delta.delta,
                }))
            }
            ProtocolEvent::ReasoningSummaryTextDelta(delta) => {
                if self.completed_items.should_ignore(
                    &delta.thread_id,
                    &delta.turn_id,
                    &delta.item_id,
                ) {
                    return Ok(Vec::new());
                }
                self.state.reduce(Action::Event(DomainEvent::ThinkingDelta {
                    thread_id: delta.thread_id,
                    turn_id: delta.turn_id,
                    item_id: delta.item_id,
                    kind: ThinkingKind::Summary,
                    index: delta.summary_index,
                    delta: delta.delta,
                }))
            }
            ProtocolEvent::ReasoningSummaryPartAdded(part) => {
                if self
                    .completed_items
                    .should_ignore(&part.thread_id, &part.turn_id, &part.item_id)
                {
                    return Ok(Vec::new());
                }
                self.state
                    .reduce(Action::Event(DomainEvent::ThinkingSummaryPartAdded {
                        thread_id: part.thread_id,
                        turn_id: part.turn_id,
                        item_id: part.item_id,
                        summary_index: part.summary_index,
                    }))
            }
            ProtocolEvent::ReasoningTextDelta(delta) => {
                if self.completed_items.should_ignore(
                    &delta.thread_id,
                    &delta.turn_id,
                    &delta.item_id,
                ) {
                    return Ok(Vec::new());
                }
                self.state.reduce(Action::Event(DomainEvent::ThinkingDelta {
                    thread_id: delta.thread_id,
                    turn_id: delta.turn_id,
                    item_id: delta.item_id,
                    kind: ThinkingKind::EmittedText,
                    index: delta.content_index,
                    delta: delta.delta,
                }))
            }
            ProtocolEvent::ItemCompleted(completed) => {
                let recognized =
                    matches!(completed.item.kind.as_str(), "agentMessage" | "reasoning");
                if recognized
                    && self.completed_items.should_ignore(
                        &completed.thread_id,
                        &completed.turn_id,
                        &completed.item.id,
                    )
                {
                    return Ok(Vec::new());
                }
                if recognized {
                    self.completed_items.record(
                        &completed.thread_id,
                        &completed.turn_id,
                        &completed.item.id,
                    );
                }
                if completed.item.kind == "agentMessage" {
                    self.state
                        .reduce(Action::Event(DomainEvent::AgentCompleted {
                            thread_id: completed.thread_id,
                            turn_id: completed.turn_id,
                            item_id: completed.item.id,
                            text: completed.item.text.unwrap_or_default(),
                        }))
                } else if completed.item.kind == "reasoning" {
                    let content = completed
                        .item
                        .content
                        .into_iter()
                        .filter_map(|content| match content {
                            crate::codex::protocol::ThreadItemContent::Text(text) => Some(text),
                            _ => None,
                        })
                        .collect();
                    self.state
                        .reduce(Action::Event(DomainEvent::ThinkingCompleted {
                            thread_id: completed.thread_id,
                            turn_id: completed.turn_id,
                            item_id: completed.item.id,
                            summary: completed.item.summary,
                            content,
                        }))
                } else {
                    Vec::new()
                }
            }
            ProtocolEvent::TurnCompleted(completed) => {
                let outcome = match completed.turn.status {
                    TurnStatus::Completed => TurnOutcome::Completed,
                    TurnStatus::Interrupted => TurnOutcome::Interrupted,
                    TurnStatus::Failed => TurnOutcome::Failed(
                        completed
                            .turn
                            .error
                            .map_or_else(|| "turn failed".to_owned(), |error| error.message),
                    ),
                    TurnStatus::InProgress | TurnStatus::Unknown => {
                        TurnOutcome::Failed("turn/completed had a non-terminal status".to_owned())
                    }
                };
                self.state.reduce(Action::Event(DomainEvent::TurnFinished {
                    thread_id: completed.thread_id,
                    turn_id: completed.turn.id,
                    outcome,
                }))
            }
            ProtocolEvent::ThreadTokenUsageUpdated(updated) => {
                self.state
                    .reduce(Action::Event(DomainEvent::TokenUsageUpdated {
                        thread_id: updated.thread_id,
                        turn_id: updated.turn_id,
                        context_tokens: updated.token_usage.last.total_tokens,
                        model_context_window: updated.token_usage.model_context_window,
                    }))
            }
            ProtocolEvent::Error(error) => {
                if error.will_retry {
                    Vec::new()
                } else {
                    self.state.reduce(Action::Event(DomainEvent::TurnFinished {
                        thread_id: error.thread_id,
                        turn_id: error.turn_id,
                        outcome: TurnOutcome::Failed(error.error.message),
                    }))
                }
            }
        })
    }
}
