use super::*;

impl<P: PreferencesPort, B: BrowserOpener> BackendCoordinator<P, B> {
    pub async fn shutdown(&mut self) -> Result<(), BackendError> {
        let claude_drained = if let Some(claude) = &mut self.claude {
            claude.service.shutdown().await
        } else {
            Vec::new()
        };
        for event in claude_drained {
            let _ = self.reduce_claude_service_event(event);
        }
        let active_openrouter_turn = match &self.state.turn {
            crate::app::TurnState::OpenRouterStreaming {
                conversation_id,
                turn_id,
            } => Some((conversation_id.clone(), turn_id.clone())),
            _ => None,
        };
        let drained = if let Some(openrouter) = &mut self.openrouter {
            openrouter.shutdown().await
        } else {
            Vec::new()
        };
        for event in drained {
            let _ = self.reduce_openrouter_service_event(event);
        }
        if let Some((conversation_id, turn_id)) =
            active_openrouter_turn.filter(|(conversation_id, turn_id)| {
                matches!(
                    &self.state.turn,
                    crate::app::TurnState::OpenRouterStreaming {
                        conversation_id: active_conversation,
                        turn_id: active_turn,
                    } if active_conversation == conversation_id && active_turn == turn_id
                )
            })
        {
            self.state
                .reduce(Action::Event(DomainEvent::OpenRouterTurnFinished {
                    conversation_id,
                    turn_id,
                    outcome: TurnOutcome::Interrupted,
                    assistant_text: None,
                    incomplete_assistant_text: None,
                    failure_stage: None,
                }));
        }
        let settled_preferences = self.state.preferences.clone();
        let persistence_result = self
            .persist_preferences(&settled_preferences)
            .map(|_| ())
            .map_err(BackendError::from);
        let session_result = if let Some(session) = &mut self.session {
            session.shutdown().await.map_err(BackendError::from)
        } else {
            Ok(())
        };
        persistence_result?;
        session_result
    }
}
