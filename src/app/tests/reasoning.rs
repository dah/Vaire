use super::*;

fn non_codex_reasoning_state(provider: ProviderId) -> AppState {
    let selected_model = match provider {
        ProviderId::OpenRouter => ModelKey::openrouter("vendor/model").unwrap(),
        ProviderId::Claude => ModelKey::claude("sonnet").unwrap(),
        ProviderId::Codex => unreachable!("helper is only for non-Codex providers"),
    };
    AppState {
        active_provider: provider,
        selected_model: Some(selected_model),
        selected_reasoning: None,
        ..AppState::default()
    }
}

#[test]
fn non_codex_show_reasoning_reports_provider_without_state_change_or_effects() {
    for provider in [ProviderId::OpenRouter, ProviderId::Claude] {
        let mut state = non_codex_reasoning_state(provider);
        let selected_model = state.selected_model.clone();
        let selected_reasoning = state.selected_reasoning.clone();
        let preferences = state.preferences.clone();
        let turn = state.turn.clone();

        let effects = state.reduce(Action::Intent(Intent::ShowReasoning));

        assert!(effects.is_empty());
        assert_eq!(
            state.notice,
            Some(format!("{provider} reasoning effort is unsupported"))
        );
        assert!(state.popup.is_none());
        assert_eq!(state.selected_model, selected_model);
        assert_eq!(state.selected_reasoning, selected_reasoning);
        assert_eq!(state.preferences, preferences);
        assert_eq!(state.turn, turn);
    }
}

#[test]
fn non_codex_select_reasoning_reports_provider_without_state_change_or_effects() {
    for provider in [ProviderId::OpenRouter, ProviderId::Claude] {
        let mut state = non_codex_reasoning_state(provider);
        let selected_model = state.selected_model.clone();
        let selected_reasoning = state.selected_reasoning.clone();
        let preferences = state.preferences.clone();
        let turn = state.turn.clone();

        let effects = state.reduce(Action::Intent(Intent::SelectReasoning("high".to_owned())));

        assert!(effects.is_empty());
        assert_eq!(
            state.notice,
            Some(format!("{provider} reasoning effort is unsupported"))
        );
        assert!(state.popup.is_none());
        assert_eq!(state.selected_model, selected_model);
        assert_eq!(state.selected_reasoning, selected_reasoning);
        assert_eq!(state.preferences, preferences);
        assert_eq!(state.turn, turn);
    }
}
