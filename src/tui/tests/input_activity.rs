use super::*;

#[test]
fn activity_indicator_is_ephemeral_and_disappears_on_first_text() {
    let mut state = waiting();
    let original_transcript = state.transcript.clone();
    let mut ui = UiState::default();
    assert!(ui.sync_activity_animation(&state));

    let initial = screen(&state, &ui, 70, 16);
    assert!(initial.contains("Agent: ~"));
    assert_eq!(state.transcript, original_transcript);

    state.reduce(Action::Event(DomainEvent::TurnStarted {
        thread_id: "thread".to_owned(),
        turn_id: "turn".to_owned(),
    }));
    state.reduce(Action::Event(DomainEvent::AgentDelta {
        thread_id: "thread".to_owned(),
        turn_id: "turn".to_owned(),
        item_id: "item".to_owned(),
        delta: "first text".to_owned(),
    }));
    assert!(ui.sync_activity_animation(&state));

    let with_text = screen(&state, &ui, 70, 16);
    assert!(with_text.contains("first text"));
    assert!(!with_text.contains('~'));
    assert_eq!(state.transcript.len(), original_transcript.len() + 1);
    assert_eq!(state.transcript.last().unwrap().text, "first text");
}

#[test]
fn activity_animation_has_fixed_width_frames_and_bounded_tick_cadence() {
    for frame in ACTIVITY_FRAMES {
        assert!(frame.is_ascii());
        assert_eq!(UnicodeWidthStr::width(frame), 5);
    }

    let mut state = waiting();
    let transcript = state.transcript.clone();
    let mut ui = UiState::default();
    assert!(ui.sync_activity_animation(&state));
    assert_eq!(ui.activity_frame(), ACTIVITY_FRAMES[0]);

    for _ in 1..ACTIVITY_TICKS_PER_FRAME {
        assert!(!ui.advance_activity_animation(&state));
        assert_eq!(ui.activity_frame(), ACTIVITY_FRAMES[0]);
    }
    assert!(ui.advance_activity_animation(&state));
    assert_eq!(ui.activity_frame(), ACTIVITY_FRAMES[1]);
    assert!(screen(&state, &ui, 70, 16).contains("Agent: ~~"));
    assert_eq!(state.transcript, transcript);

    state.turn = TurnState::Completed {
        turn_id: "turn".to_owned(),
    };
    assert!(ui.sync_activity_animation(&state));
    assert_eq!(ui.activity_frame(), ACTIVITY_FRAMES[0]);
    assert!(!ui.advance_activity_animation(&state));
    assert!(!ui.advance_activity_animation(&state));
}

#[test]
fn activity_frames_preserve_scrolled_history_and_fit_narrow_terminals() {
    let mut state = waiting();
    state.turn = TurnState::Streaming {
        thread_id: "thread".to_owned(),
        turn_id: "turn".to_owned(),
    };
    state.transcript = (0..24)
        .map(|index| TranscriptEntry {
            provider: crate::provider::ProviderId::Codex,
            role: TranscriptRole::User,
            status: TranscriptEntryStatus::Normal,
            text: format!("historical message {index}"),
            item_id: None,
            turn_id: None,
        })
        .collect();
    let mut ui = UiState {
        scroll_from_bottom: 5,
        ..UiState::default()
    };
    ui.sync_activity_animation(&state);
    let before = screen(&state, &ui, 50, 12);
    for _ in 0..ACTIVITY_TICKS_PER_FRAME {
        ui.advance_activity_animation(&state);
    }
    let after = screen(&state, &ui, 50, 12);
    assert_eq!(before, after);
    assert_eq!(ui.scroll_from_bottom, 5);

    ui.scroll_from_bottom = usize::MAX;
    let oldest = screen(&state, &ui, 50, 12);
    assert!(oldest.contains("historical message 0"));

    ui.scroll_from_bottom = 0;
    let narrow = screen(&state, &ui, 36, 9);
    assert!(narrow.contains("Agent:"));
    assert!(narrow.contains('~'));
    assert!(screen(&state, &ui, 35, 8).contains("Terminal too small"));
}

#[test]
fn input_supports_multiline_help_interrupt_scroll_and_quit() {
    let mut ui = UiState::default();
    ui.handle_event(Event::Key(KeyEvent::new(
        KeyCode::Char('h'),
        KeyModifiers::NONE,
    )));
    ui.handle_event(Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT)));
    assert_eq!(ui.composer, "h\n");
    ui.composer = "/help".to_owned();
    assert!(ui
        .handle_event(Event::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE
        )))
        .is_none());
    assert!(ui.overlay.is_some());
    let visible_overlay = ui.overlay.clone();
    assert!(ui
        .handle_event(Event::Paste("hidden paste".to_owned()))
        .is_none());
    assert!(ui
        .handle_event(Event::Key(KeyEvent::new(
            KeyCode::Char('x'),
            KeyModifiers::NONE,
        )))
        .is_none());
    assert!(ui
        .handle_event(Event::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        )))
        .is_none());
    assert_eq!(ui.overlay, visible_overlay);
    assert!(ui.composer.is_empty());
    assert_eq!(
        ui.handle_event(Event::Key(KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL,
        ))),
        Some(Intent::Quit)
    );
    assert_eq!(ui.overlay, visible_overlay);
    assert!(ui
        .handle_event(Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)))
        .is_none());
    assert!(matches!(
        ui.handle_event(Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))),
        Some(crate::app::Intent::Interrupt)
    ));
    ui.handle_event(Event::Key(KeyEvent::new(
        KeyCode::PageUp,
        KeyModifiers::NONE,
    )));
    assert_eq!(ui.scroll_from_bottom, 1);
    assert!(matches!(
        ui.handle_event(Event::Key(KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL
        ))),
        Some(crate::app::Intent::Quit)
    ));
}

#[test]
fn composer_input_is_bounded_without_splitting_utf8() {
    let mut ui = UiState {
        composer: "a".repeat(MAX_COMPOSER_BYTES - 1),
        ..UiState::default()
    };
    assert!(ui
        .handle_event(Event::Paste("界 tail".to_owned()))
        .is_none());
    assert_eq!(ui.composer.len(), MAX_COMPOSER_BYTES - 1);
    assert!(ui
        .overlay
        .as_deref()
        .is_some_and(|message| message.contains("message size limit")));

    ui.handle_event(Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
    ui.handle_event(Event::Key(KeyEvent::new(
        KeyCode::Backspace,
        KeyModifiers::NONE,
    )));
    ui.handle_event(Event::Key(KeyEvent::new(
        KeyCode::Backspace,
        KeyModifiers::NONE,
    )));
    ui.handle_event(Event::Paste("界".to_owned()));
    assert_eq!(ui.composer.len(), MAX_COMPOSER_BYTES);
    assert!(ui.overlay.is_none());

    ui.handle_event(Event::Key(KeyEvent::new(
        KeyCode::Char('x'),
        KeyModifiers::NONE,
    )));
    assert_eq!(ui.composer.len(), MAX_COMPOSER_BYTES);
    assert!(ui.overlay.is_some());

    let mut newline_heavy = UiState::default();
    newline_heavy.handle_event(Event::Paste(format!("{}TAIL", "\n".repeat(70_000))));
    let rendered = screen(&ready(), &newline_heavy, 36, 9);
    assert!(rendered.contains("TAIL"));
    assert!(newline_heavy.overlay.is_none());
}
