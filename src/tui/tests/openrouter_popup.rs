use super::*;
use crate::app::{AuthPopupMode, OpenRouterCredentialValidation, PopupState, TurnState};
use crate::openrouter::{OpenRouterAuthStatus, OpenRouterModel};
use crate::provider::{ModelKey, OpenRouterConversationId, OpenRouterTurnId, ProviderId};

const FAKE_KEY: &str = "sk-or-v1-secret-ui-regression-key";

#[test]
fn secret_entry_is_masked_non_clonable_and_never_uses_the_composer() {
    let mut state = ready();
    state.popup = Some(PopupState::ProviderSecret {
        provider: ProviderId::OpenRouter,
    });
    let mut ui = UiState::default();
    ui.sync_secret_editor(&state);
    assert_eq!(ui.secret_mask(), Some("••••••••"));
    assert_eq!(
        ui.handle_event_for_state(Event::Paste(FAKE_KEY.to_owned()), &state),
        None
    );
    assert!(ui.composer.is_empty());
    let rendered = screen(&state, &ui, 70, 16);
    assert!(rendered.contains("••••••••"));
    assert!(!rendered.contains(FAKE_KEY));
    assert!(!format!("{ui:?}").contains(FAKE_KEY));

    assert_eq!(
        ui.handle_event_for_state(
            Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            &state,
        ),
        None
    );
    let (provider, submitted) = ui.take_submitted_secret().expect("submitted secret");
    assert_eq!(provider, ProviderId::OpenRouter);
    assert_eq!(submitted.expose_bytes(), FAKE_KEY.as_bytes());
    assert!(ui.composer.is_empty());

    // A full/closed runtime channel returns ownership to the UI for an exact retry.
    ui.restore_provider_secret(ProviderId::OpenRouter, submitted);
    ui.handle_event_for_state(
        Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        &state,
    );
    let (provider, retried) = ui.take_submitted_secret().expect("retried secret");
    assert_eq!(provider, ProviderId::OpenRouter);
    assert_eq!(retried.expose_bytes(), FAKE_KEY.as_bytes());
}

#[test]
fn active_openrouter_turn_keeps_the_candidate_in_the_masked_editor() {
    let mut state = ready();
    state.active_provider = ProviderId::OpenRouter;
    state.popup = Some(PopupState::ProviderSecret {
        provider: ProviderId::OpenRouter,
    });
    state.turn = TurnState::OpenRouterStreaming {
        conversation_id: OpenRouterConversationId::new(),
        turn_id: OpenRouterTurnId::new(),
    };
    let mut ui = UiState::default();
    ui.sync_secret_editor(&state);
    ui.handle_event_for_state(Event::Paste(FAKE_KEY.to_owned()), &state);
    ui.handle_event_for_state(
        Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        &state,
    );

    assert!(ui.take_submitted_secret().is_none());
    assert_eq!(ui.secret_mask(), Some("••••••••"));
    assert!(ui
        .overlay
        .as_deref()
        .unwrap()
        .contains("active OpenRouter turn"));
    assert!(!format!("{ui:?}").contains(FAKE_KEY));
}

#[test]
fn auth_and_model_popups_keep_provider_labels_at_narrow_width() {
    let mut state = ready();
    state.openrouter.auth = OpenRouterAuthStatus::Valid;
    state.popup = Some(PopupState::Auth {
        mode: AuthPopupMode::Login,
        selected: ProviderId::OpenRouter,
    });
    let auth = screen(&state, &UiState::default(), 36, 12);
    assert!(auth.contains("Codex"));
    assert!(auth.contains("OpenRouter"));

    state.popup = Some(PopupState::Model {
        choices: vec![
            ModelKey::codex("codex-model").unwrap(),
            ModelKey::openrouter("vendor/model").unwrap(),
        ],
        selected: 1,
        search: String::new(),
    });
    let models = screen(&state, &UiState::default(), 36, 15);
    assert!(models.contains("Codex"));
    assert!(models.contains("OpenRouter"));
    assert!(models.contains("/resume"));
}

#[test]
fn secret_paste_is_cumulatively_bounded_and_validation_does_not_recreate_editor() {
    let mut state = ready();
    state.popup = Some(PopupState::ProviderSecret {
        provider: ProviderId::OpenRouter,
    });
    let mut ui = UiState::default();
    ui.sync_secret_editor(&state);
    ui.handle_event_for_state(
        Event::Paste("a".repeat(crate::credentials::MAX_CREDENTIAL_BYTES)),
        &state,
    );
    ui.handle_event_for_state(Event::Paste("b".to_owned()), &state);
    assert!(ui.overlay.is_some());
    ui.handle_event_for_state(
        Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        &state,
    );
    let (provider, submitted) = ui.take_submitted_secret().unwrap();
    assert_eq!(provider, ProviderId::OpenRouter);
    assert_eq!(
        submitted.expose_bytes().len(),
        crate::credentials::MAX_CREDENTIAL_BYTES
    );

    ui.sync_secret_editor(&state);
    assert_eq!(ui.secret_mask(), None);
    state.openrouter.credential_validation = OpenRouterCredentialValidation::Validating {
        operation_id: 1,
        candidate_saved: false,
    };
    ui.sync_secret_editor(&state);
    assert_eq!(ui.secret_mask(), None);
    assert!(screen(&state, &ui, 70, 16).contains("Validating OpenRouter credential"));

    state.openrouter.credential_validation = OpenRouterCredentialValidation::Idle;
    state.notice = Some("candidate rejected".to_owned());
    ui.sync_secret_editor(&state);
    assert_eq!(ui.secret_mask(), Some("••••••••"));

    state.popup = None;
    ui.sync_secret_editor(&state);
    assert_eq!(ui.secret_mask(), None);
}

#[test]
fn model_and_catalog_popups_center_long_filtered_lists_and_use_real_navigation_keys() {
    let mut state = ready();
    let choices = (0..40)
        .map(|index| ModelKey::openrouter(format!("vendor/model-{index:02}")).unwrap())
        .collect::<Vec<_>>();
    state.popup = Some(PopupState::Model {
        choices,
        selected: 0,
        search: String::new(),
    });
    let mut ui = UiState::default();

    for (key, expected) in [
        (KeyCode::End, 39),
        (KeyCode::PageUp, 29),
        (KeyCode::Home, 0),
        (KeyCode::PageDown, 10),
    ] {
        let intent = ui
            .handle_event_for_state(Event::Key(KeyEvent::new(key, KeyModifiers::NONE)), &state)
            .unwrap();
        state.reduce(crate::app::Action::Intent(intent));
        let Some(PopupState::Model { selected, .. }) = &state.popup else {
            panic!("model popup");
        };
        assert_eq!(*selected, expected);
    }

    if let Some(PopupState::Model {
        selected, search, ..
    }) = &mut state.popup
    {
        *selected = 3;
        *search = "model-3".to_owned();
    }
    let filtered = screen(&state, &ui, 60, 12);
    assert!(filtered.contains("> [OpenRouter] vendor/model-33"));
    assert!(!filtered.contains("vendor/model-00"));

    state.popup = Some(PopupState::OpenRouterCatalog {
        models: (0..100)
            .map(|index| OpenRouterModel {
                id: format!("vendor/catalog-{index:03}"),
                name: Some(format!("Catalog {index:03}")),
                context_length: Some(4096),
            })
            .collect(),
        draft_enabled: Default::default(),
        selected: 75,
        search: String::new(),
    });
    let catalog = screen(&state, &ui, 60, 12);
    assert!(catalog.contains("> [ ] Catalog 075"));
    assert!(!catalog.contains("Catalog 000"));
}
