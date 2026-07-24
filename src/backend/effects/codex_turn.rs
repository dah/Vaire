use super::*;

impl<P: PreferencesPort, B: BrowserOpener> BackendCoordinator<P, B> {
    pub(super) async fn send_message(&mut self, text: &str) -> Result<Vec<Effect>, BackendError> {
        let model = self.selected_model(ProviderId::Codex)?;
        let effort =
            self.state.selected_reasoning.clone().ok_or_else(|| {
                SessionError::Protocol("no reasoning effort is selected".to_owned())
            })?;
        let thread_id = match &self.state.thread {
            ThreadState::Ready { id } => id.clone(),
            ThreadState::None => {
                let thread = self.codex_mut()?.start_thread(&model.id).await?;
                let id = thread.id;
                let effects = self
                    .state
                    .reduce(Action::Event(DomainEvent::ThreadStarted { id: id.clone() }));
                for effect in effects {
                    if let Effect::Persist(preferences) = effect {
                        let _ = self.persist_preferences(&preferences)?;
                    }
                }
                id
            }
            _ => {
                return Err(SessionError::Protocol(
                    "message effect reached a non-sendable thread state".to_owned(),
                )
                .into())
            }
        };
        let response = self
            .codex_mut()?
            .start_turn(&thread_id, text, &model.id, &effort)
            .await?;
        let turn_id = response.turn.id;
        self.completed_items.begin_turn(&thread_id, &turn_id);
        Ok(self.state.reduce(Action::Event(DomainEvent::TurnStarted {
            thread_id,
            turn_id,
        })))
    }
    pub(super) async fn interrupt_codex_turn_effect(
        &mut self,
        thread_id: String,
        turn_id: String,
    ) -> Result<Vec<Effect>, BackendError> {
        Ok({
            match self.codex_mut()?.interrupt_turn(&thread_id, &turn_id).await {
                Ok(()) => Vec::new(),
                Err(error) => self.reduce_mutating_error(error.into()),
            }
        })
    }
}
