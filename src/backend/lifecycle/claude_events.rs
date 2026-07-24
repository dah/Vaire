use super::*;

impl<P: PreferencesPort, B: BrowserOpener> BackendCoordinator<P, B> {
    pub(in crate::backend) fn reduce_claude_service_event(
        &mut self,
        event: ClaudeServiceEvent,
    ) -> Vec<Effect> {
        let event = match event {
            ClaudeServiceEvent::TurnStarted {
                session_id,
                turn_id,
            } => DomainEvent::ClaudeTurnStarted {
                session_id,
                turn_id,
            },
            ClaudeServiceEvent::Initialized {
                session_id,
                turn_id,
                model,
            } => DomainEvent::ClaudeInitialized {
                session_id,
                turn_id,
                model,
            },
            ClaudeServiceEvent::TextDelta {
                session_id,
                turn_id,
                delta,
            } => DomainEvent::ClaudeDelta {
                session_id,
                turn_id,
                delta,
            },
            ClaudeServiceEvent::TurnFinished {
                session_id,
                turn_id,
                outcome,
                assistant_text,
                incomplete_assistant_text,
                creation_uncertain,
                failure,
            } => DomainEvent::ClaudeTurnFinished {
                session_id,
                turn_id,
                outcome,
                assistant_text,
                incomplete_assistant_text,
                creation_uncertain,
                failure,
            },
        };
        self.state.reduce(Action::Event(event))
    }
}
