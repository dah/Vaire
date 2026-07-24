use super::*;

impl OpenRouterService {
    pub fn launch_prepared_turn(&mut self, prepared: PreparedOpenRouterTurn) {
        let PreparedOpenRouterTurn {
            conversation_id,
            turn_id,
            mut conversation,
            request,
        } = prepared;

        let cancel = CancellationToken::new();
        let task_cancel = cancel.clone();
        let callback_cancel = cancel.clone();
        let client = self.client.clone();
        let store = self.store.clone();
        let events = self.chat_events_tx.clone();
        let task_conversation_id = conversation_id.clone();
        let task_turn_id = turn_id.clone();
        self.chat_task = Some(tokio::spawn(async move {
            let mut final_text = None;
            let mut final_usage = None;
            let mut streamed_text = String::new();
            let mut stream_bound_exceeded = false;
            let mut delivery_failed = false;
            let result = client
                .chat(&request, task_cancel, |event| match event {
                    ChatStreamEvent::TextDelta(delta) => {
                        if streamed_text
                            .len()
                            .checked_add(delta.len())
                            .is_none_or(|length| length > MAX_ASSISTANT_BYTES)
                        {
                            stream_bound_exceeded = true;
                            callback_cancel.cancel();
                            return;
                        }
                        streamed_text.push_str(&delta);
                        if events
                            .try_send(OpenRouterServiceEvent::TextDelta {
                                conversation_id: task_conversation_id.clone(),
                                turn_id: task_turn_id.clone(),
                                delta,
                            })
                            .is_err()
                        {
                            delivery_failed = true;
                            callback_cancel.cancel();
                        }
                    }
                    ChatStreamEvent::Usage(usage) => {
                        final_usage = Some(usage);
                        let _ = events.try_send(OpenRouterServiceEvent::Usage {
                            conversation_id: task_conversation_id.clone(),
                            turn_id: task_turn_id.clone(),
                            usage,
                        });
                    }
                    ChatStreamEvent::Finished {
                        assistant_text,
                        usage,
                    } => {
                        final_text = Some(assistant_text);
                        final_usage = usage.or(final_usage);
                    }
                })
                .await;
            let (outcome, failure, failure_stage) = match result {
                _ if delivery_failed => (OpenRouterTurnOutcome::Interrupted, None, None),
                _ if stream_bound_exceeded => (
                    OpenRouterTurnOutcome::Failed,
                    Some(OpenRouterFailureCategory::ResourceLimit),
                    None,
                ),
                Ok(()) => (OpenRouterTurnOutcome::Completed, None, None),
                Err(error) if error.category() == OpenRouterFailureCategory::Cancelled => {
                    (OpenRouterTurnOutcome::Interrupted, None, None)
                }
                Err(error) => (
                    OpenRouterTurnOutcome::Failed,
                    Some(error.category()),
                    error.stage(),
                ),
            };
            let assistant_text = (outcome == OpenRouterTurnOutcome::Completed)
                .then(|| final_text.take())
                .flatten();
            let incomplete_assistant_text = (outcome == OpenRouterTurnOutcome::Failed
                && !streamed_text.is_empty())
            .then_some(streamed_text);
            if let Some(record) = conversation.turns.last_mut() {
                record.outcome = outcome;
                record.assistant_text = assistant_text.clone();
                record.incomplete_assistant_text = incomplete_assistant_text.clone();
            }
            conversation.updated_at_ms = now_ms();
            let persisted = tokio::task::spawn_blocking(move || {
                store.save_conversation_with_commit(&conversation)
            })
            .await;
            let (outcome, failure, failure_stage, assistant_text, incomplete_assistant_text) =
                if matches!(persisted, Ok(Ok(_))) {
                    (
                        outcome,
                        failure,
                        failure_stage,
                        assistant_text,
                        incomplete_assistant_text,
                    )
                } else {
                    (
                        OpenRouterTurnOutcome::Failed,
                        Some(OpenRouterFailureCategory::CredentialStore),
                        None,
                        None,
                        None,
                    )
                };
            let _ = events
                .send(OpenRouterServiceEvent::TurnFinished {
                    conversation_id: task_conversation_id,
                    turn_id: task_turn_id,
                    outcome,
                    assistant_text,
                    incomplete_assistant_text,
                    usage: final_usage,
                    failure,
                    failure_stage,
                })
                .await;
        }));
        self.chat_cancel = Some(cancel);
    }

    #[cfg(test)]
    pub async fn start_turn(
        &mut self,
        conversation_id: Option<OpenRouterConversationId>,
        model_id: String,
        user_text: String,
    ) -> Result<(OpenRouterConversationId, OpenRouterTurnId), OpenRouterFailure> {
        let prepared = self
            .prepare_turn(conversation_id, model_id, user_text)
            .await?;
        let conversation_id = prepared.conversation_id().clone();
        let turn_id = prepared.turn_id().clone();
        let _ = self
            .chat_events_tx
            .send(OpenRouterServiceEvent::TurnStarted {
                conversation_id: conversation_id.clone(),
                turn_id: turn_id.clone(),
            })
            .await;
        self.launch_prepared_turn(prepared);
        Ok((conversation_id, turn_id))
    }

    pub fn interrupt_turn(&self) {
        if let Some(cancel) = &self.chat_cancel {
            cancel.cancel();
        }
    }
}
