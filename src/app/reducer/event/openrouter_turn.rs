use super::*;

impl AppState {
    pub(super) fn matches_openrouter_turn(
        &self,
        conversation_id: &OpenRouterConversationId,
        turn_id: &OpenRouterTurnId,
    ) -> bool {
        matches!(
            &self.turn,
            TurnState::OpenRouterStreaming {
                conversation_id: active_conversation,
                turn_id: active_turn,
            } if active_conversation == conversation_id && active_turn == turn_id
        )
    }

    pub(in crate::app) fn validate_openrouter_selection(&mut self) {
        if self.active_provider != ProviderId::OpenRouter {
            return;
        }
        let resolved = self.resolve_provider_selection(ProviderId::OpenRouter);
        if let Some((model, _)) = resolved {
            let _ = self.commit_provider_selection(ProviderId::OpenRouter, model, None);
        } else {
            self.selected_model = None;
            self.selected_reasoning = None;
            self.preferences.openrouter.selected_model_id = None;
        }
    }
}
