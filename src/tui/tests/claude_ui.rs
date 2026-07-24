use super::*;
use crate::app::{
    AuthPopupMode, ClaudeAuthOperation, ClaudeAuthRequest, ClaudeAvailability,
    ClaudeConversationState,
};
use crate::claude::{ClaudeAuthStatus, ClaudeModelMetadata};
use crate::provider::{ModelKey, ProviderId};

fn claude_ready() -> AppState {
    let mut state = ready();
    state.active_provider = ProviderId::Claude;
    state.claude.availability = ClaudeAvailability::Ready;
    state.claude.auth = ClaudeAuthStatus::Subscription;
    state.selected_model = Some(ModelKey::claude("sonnet").unwrap());
    state.selected_reasoning = None;
    state.context_remaining_percent = None;
    state
}

#[test]
fn auth_popup_navigates_and_renders_all_three_providers() {
    let mut state = claude_ready();
    state.popup = Some(PopupState::Auth {
        mode: AuthPopupMode::Login,
        selected: ProviderId::Claude,
    });
    let rendered = screen(&state, &UiState::default(), 64, 16);
    assert!(rendered.contains("Codex"));
    assert!(rendered.contains("OpenRouter"));
    assert!(rendered.contains("> Claude"));
    assert!(rendered.contains("subscription connected"));

    let mut ui = UiState::default();
    assert_eq!(
        ui.handle_event_for_state(
            Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE)),
            &state,
        ),
        None
    );
    assert_eq!(
        ui.handle_event_for_state(
            Event::Key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE)),
            &state,
        ),
        Some(Intent::PopupRefresh)
    );
}

#[test]
fn claude_login_uses_native_browser_flow_without_a_secret_editor() {
    let mut state = claude_ready();
    state.popup = Some(PopupState::Auth {
        mode: AuthPopupMode::Login,
        selected: ProviderId::Claude,
    });
    let mut ui = UiState::default();
    ui.sync_secret_editor(&state);
    assert_eq!(ui.secret_mask(), None);
    let rendered = screen(&state, &ui, 76, 18);
    assert!(rendered.contains("native browser login"));
    assert!(!rendered.contains("API key"));
    assert_eq!(
        ui.handle_event_for_state(
            Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            &state,
        ),
        Some(Intent::PopupSelect)
    );
    let effects = state.reduce(Action::Intent(Intent::PopupSelect));
    assert_eq!(effects, vec![crate::app::Effect::LoginClaude]);
    assert!(state.popup.is_none());
    assert!(ui.take_submitted_secret().is_none());

    state.claude.auth_operation = ClaudeAuthOperation::AwaitingTerminal {
        request: ClaudeAuthRequest {
            operation_id: 1,
            action: crate::claude::ClaudeAuthAction::Login,
        },
    };
    state.popup = Some(PopupState::Auth {
        mode: AuthPopupMode::Login,
        selected: ProviderId::Claude,
    });
    assert!(screen(&state, &ui, 76, 18).contains("signing in via browser"));

    state.claude.auth_operation = ClaudeAuthOperation::Idle;
    state.popup = Some(PopupState::Auth {
        mode: AuthPopupMode::Logout,
        selected: ProviderId::Claude,
    });
    let logout_popup = screen(&state, &ui, 76, 18);
    assert!(logout_popup.contains("Claude also signs out"));
    assert!(logout_popup.contains("system Claude"));
}

#[test]
fn claude_header_empty_states_reasoning_and_models_are_provider_specific() {
    let mut state = claude_ready();
    state.claude.resolved_model = Some(ClaudeModelMetadata {
        id: "claude-sonnet-resolved\nunsafe".to_owned(),
        display_name: None,
    });
    let rendered_header = header(&state, 120);
    assert!(rendered_header.contains("Claude"));
    assert!(rendered_header.contains("sonnet → claude-sonnet-resolved unsafe"));
    assert!(rendered_header.contains("reasoning n/a"));
    assert!(rendered_header.ends_with("Context --"));

    state.claude.auth = ClaudeAuthStatus::SignedOut;
    let signed_out = screen(&state, &UiState::default(), 90, 20);
    assert!(signed_out.contains("Claude subscription"));

    state.claude.auth = ClaudeAuthStatus::Subscription;
    state.claude.conversation = ClaudeConversationState::ResumeFailed {
        id: "00000000-0000-4000-8000-000000000000".parse().unwrap(),
        message: "missing".to_owned(),
    };
    let failed = screen(&state, &UiState::default(), 90, 20);
    assert!(failed.contains("saved Claude session could not be resumed"));
    assert!(failed.contains("/resume"));

    state.claude.conversation = ClaudeConversationState::None;
    state.thinking.visible = true;
    let reasoning = screen(&state, &UiState::default(), 120, 20);
    assert!(reasoning.contains("Claude reasoning is not collected"));

    state.thinking.visible = false;
    state.popup = Some(PopupState::Model {
        choices: vec![
            ModelKey::openrouter("anthropic/claude").unwrap(),
            ModelKey::claude("sonnet").unwrap(),
            ModelKey::claude("opus").unwrap(),
            ModelKey::claude("haiku").unwrap(),
        ],
        selected: 1,
        search: String::new(),
    });
    let models = screen(&state, &UiState::default(), 60, 18);
    assert!(models.contains("[Claude] sonnet"));
    assert!(models.contains("[Claude] opus"));
    assert!(models.contains("[Claude] haiku"));
    assert!(models.contains("[OpenRouter] anthropic/claude"));
}

#[test]
fn claude_picker_confirmation_explains_registration_only_deletion() {
    let mut state = claude_ready();
    let target = ThreadChoice {
        provider: ProviderId::Claude,
        id: "00000000-0000-4000-8000-000000000000".to_owned(),
        title: "Claude session".to_owned(),
        updated_at: 1,
    };
    state.popup = conversation_popup(ThreadPickerState {
        phase: ThreadPickerPhase::Ready,
        threads: vec![target.clone()],
        selected: 0,
        confirmation: Some(ThreadDeleteConfirmation::Selected { target }),
        message: None,
    });
    let rendered = screen(&state, &UiState::default(), 90, 24);
    assert!(rendered.contains("Vairë’s registration"));
    assert!(rendered.contains("private session data is not inspected or deleted"));
}
