use super::*;

pub(super) fn openrouter_model(id: &str) -> OpenRouterModel {
    OpenRouterModel {
        id: id.to_owned(),
        name: Some(id.to_owned()),
        context_length: Some(4096),
    }
}

pub(super) fn streaming_openrouter_state() -> (AppState, OpenRouterConversationId, OpenRouterTurnId)
{
    let conversation_id = OpenRouterConversationId::new();
    let turn_id = OpenRouterTurnId::new();
    (
        AppState {
            active_provider: ProviderId::OpenRouter,
            turn: TurnState::OpenRouterStreaming {
                conversation_id: conversation_id.clone(),
                turn_id: turn_id.clone(),
            },
            ..AppState::default()
        },
        conversation_id,
        turn_id,
    )
}
