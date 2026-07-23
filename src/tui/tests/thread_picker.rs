use super::*;

#[test]
fn thread_picker_keeps_modal_keys_while_activity_animation_ticks() {
    let mut state = waiting();
    state.context_remaining_percent = Some(100);
    state.popup = conversation_popup(ThreadPickerState {
        phase: ThreadPickerPhase::Ready,
        threads: vec![ThreadChoice {
            provider: crate::provider::ProviderId::Codex,
            id: "thr-old".to_owned(),
            title: "Old conversation".to_owned(),
            updated_at: 1,
        }],
        selected: 0,
        confirmation: None,
        message: None,
    });
    let mut ui = UiState {
        composer: "untouched draft".to_owned(),
        ..UiState::default()
    };
    assert!(ui.sync_activity_animation(&state));
    assert_eq!(ui.activity_frame(), ACTIVITY_FRAMES[0]);

    for _ in 0..ACTIVITY_TICKS_PER_FRAME {
        ui.advance_activity_animation(&state);
    }
    assert_eq!(ui.activity_frame(), ACTIVITY_FRAMES[1]);
    assert_eq!(
        ui.handle_event_for_state(
            Event::Key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE)),
            &state,
        ),
        Some(Intent::ThreadPickerMoveDown)
    );
    assert_eq!(ui.composer, "untouched draft");
    let rendered = screen(&state, &ui, 50, 14);
    assert!(rendered.contains("Saved threads"));
    assert!(header(&state, 50).ends_with("Context 100%"));
}

#[test]
fn thread_picker_renders_at_normal_and_narrow_supported_widths() {
    let mut state = ready();
    state.preferences.codex.auto_resume_thread_id = Some("thr-active".to_owned());
    state.popup = conversation_popup(ThreadPickerState {
        phase: ThreadPickerPhase::Ready,
        threads: vec![
            ThreadChoice {
                provider: crate::provider::ProviderId::Codex,
                id: "thr-active".to_owned(),
                title: "Current conversation".to_owned(),
                updated_at: 20,
            },
            ThreadChoice {
                provider: crate::provider::ProviderId::Codex,
                id: "thr-old".to_owned(),
                title: "An older conversation".to_owned(),
                updated_at: 10,
            },
            ThreadChoice {
                provider: crate::provider::ProviderId::OpenRouter,
                id: "or_00000000-0000-4000-8000-000000000001".to_owned(),
                title: "Local OpenRouter conversation".to_owned(),
                updated_at: 5,
            },
        ],
        selected: 1,
        confirmation: None,
        message: None,
    });
    let normal = screen(&state, &UiState::default(), 90, 24);
    assert!(normal.contains("Saved threads"));
    assert!(normal.contains("Current conversation"));
    assert!(normal.contains("[Codex]"));
    assert!(normal.contains("[OpenRouter]"));
    assert!(normal.contains("Local OpenRouter conversation"));
    assert!(normal.contains("ACTIVE"));
    assert!(normal.contains("D clear inactive"));

    let narrow = screen(&state, &UiState::default(), 36, 9);
    assert!(narrow.contains("Saved threads"));
    assert!(narrow.contains("Current conversation") || narrow.contains("An older"));

    state.conversation_popup_mut().unwrap().selected = usize::MAX;
    let corrupted_selection = screen(&state, &UiState::default(), 36, 9);
    assert!(corrupted_selection.contains("Saved threads"));
}

#[test]
fn thread_picker_keys_are_modal_and_confirmation_is_a_second_action() {
    let mut state = ready();
    state.thinking.visible = true;
    state.popup = conversation_popup(ThreadPickerState {
        phase: ThreadPickerPhase::Ready,
        threads: vec![ThreadChoice {
            provider: crate::provider::ProviderId::Codex,
            id: "thr-old".to_owned(),
            title: "Old".to_owned(),
            updated_at: 1,
        }],
        selected: 0,
        confirmation: None,
        message: None,
    });
    let mut ui = UiState {
        composer: "draft message".to_owned(),
        ..UiState::default()
    };
    assert_eq!(
        ui.handle_event_for_state(
            Event::Key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE)),
            &state,
        ),
        Some(Intent::ThreadPickerMoveDown)
    );
    assert_eq!(
        ui.handle_event_for_state(
            Event::Key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE)),
            &state,
        ),
        Some(Intent::ThreadPickerRequestDelete)
    );
    assert_eq!(
        ui.handle_event_for_state(
            Event::Key(KeyEvent::new(KeyCode::Char('D'), KeyModifiers::SHIFT)),
            &state,
        ),
        Some(Intent::ThreadPickerRequestClearInactive)
    );
    assert_eq!(ui.composer, "draft message");
    assert_eq!(
        ui.handle_event_for_state(
            Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL,)),
            &state,
        ),
        Some(Intent::Quit)
    );

    let combined = screen(&state, &ui, 36, 12);
    assert!(combined.contains("Saved threads"));
    assert!(!combined.contains("draft message"));

    state.conversation_popup_mut().unwrap().confirmation =
        Some(ThreadDeleteConfirmation::Selected {
            target: ThreadChoice {
                provider: crate::provider::ProviderId::Codex,
                id: "thr-old".to_owned(),
                title: "Old".to_owned(),
                updated_at: 1,
            },
        });
    assert_eq!(
        ui.handle_event_for_state(
            Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            &state,
        ),
        Some(Intent::ThreadPickerConfirmDelete)
    );
    assert_eq!(
        ui.handle_event_for_state(
            Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            &state,
        ),
        Some(Intent::ThreadPickerCancelDelete)
    );
    let confirmation = screen(&state, &ui, 70, 20);
    assert!(confirmation.contains("Confirm permanent deletion"));
    assert!(confirmation.contains("thr-old"));
    assert!(confirmation.contains("cannot be undone"));
}
