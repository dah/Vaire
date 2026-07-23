use super::*;

#[test]
fn renders_ready_streaming_completed_and_error_states() {
    let mut state = ready();
    assert!(screen(&state, &UiState::default(), 100, 20).contains("thread ready"));
    state.transcript.push(TranscriptEntry {
        provider: crate::provider::ProviderId::Codex,
        role: TranscriptRole::Assistant,
        text: "partial reply".to_owned(),
        item_id: None,
        turn_id: None,
    });
    state.turn = TurnState::Streaming {
        thread_id: "thread".to_owned(),
        turn_id: "turn".to_owned(),
    };
    let streaming = screen(&state, &UiState::default(), 100, 20);
    assert!(streaming.contains("streaming"));
    assert!(streaming.contains("partial reply"));
    state.turn = TurnState::Completed {
        turn_id: "turn".to_owned(),
    };
    assert!(screen(&state, &UiState::default(), 100, 20).contains("completed"));
    state.connection = ConnectionState::Failed("upgrade Codex".to_owned());
    state.turn = TurnState::Failed {
        turn_id: None,
        message: "failed".to_owned(),
    };
    let failed = screen(&state, &UiState::default(), 100, 20);
    assert!(failed.contains("upgrade Codex"));
    assert!(failed.contains("failed"));
}

#[test]
fn activity_indicator_stays_in_conversation_when_thinking_panel_is_open() {
    let mut state = waiting();
    state.thinking.visible = true;
    state.context_remaining_percent = Some(73);
    let mut ui = UiState::default();
    assert!(ui.sync_activity_animation(&state));

    let normal = screen(&state, &ui, 100, 20);
    let activity_column = normal
        .lines()
        .find_map(|line| line.find('~'))
        .expect("activity frame should be visible");
    assert!(
        activity_column < 67,
        "activity must stay in the conversation pane"
    );
    assert!(normal.contains("Reasoning"));
    assert!(normal.contains("Awaiting emitted reasoning"));
    assert!(header(&state, 100).ends_with("Context 73%"));

    let narrow = screen(&state, &ui, 36, 12);
    assert!(narrow.contains("Conversation"));
    assert!(narrow.contains("Agent:"));
    assert!(narrow.contains('~'));
    assert!(narrow.contains("Reasoning"));
    assert!(header(&state, 36).ends_with("Context 73%"));
}

#[test]
fn composer_wraps_long_wide_input_and_keeps_tail_cursor_visible() {
    let exact_width_ui = UiState {
        composer: "a".repeat(38),
        ..UiState::default()
    };
    let mut exact_width = draw(&ready(), &exact_width_ui, 40, 16);
    assert_eq!(
        exact_width.backend_mut().get_cursor_position().unwrap(),
        Position::new(1, 13)
    );

    let normal_ui = UiState {
        composer: format!("{}{}", "a".repeat(50), "界".repeat(20)),
        ..UiState::default()
    };
    let mut normal = draw(&ready(), &normal_ui, 40, 16);
    assert_eq!(
        normal.backend_mut().get_cursor_position().unwrap(),
        Position::new(15, 13)
    );

    let small_ui = UiState {
        composer: "界".repeat(70),
        ..UiState::default()
    };
    let mut small = draw(&ready(), &small_ui, 36, 9);
    assert_eq!(
        small.backend_mut().get_cursor_position().unwrap(),
        Position::new(5, 6)
    );
    let rendered = (0..9)
        .map(|y| {
            (0..36)
                .map(|x| small.backend().buffer()[(x, y)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(rendered.contains('界'));
}

#[test]
fn wraps_long_catalog_notices_and_shows_actual_turn_failure() {
    let mut state = ready();
    state.notice = Some(
        "Available models: model-alpha, model-beta, model-gamma, model-delta, \
             model-epsilon, model-zeta, model-eta, model-theta, model-iota, model-kappa"
            .to_owned(),
    );
    let catalog = screen(&state, &UiState::default(), 50, 18);
    assert!(catalog.contains("Available models"));
    assert!(catalog.contains("model-kappa"));

    state.notice = None;
    state.turn = TurnState::Failed {
        turn_id: Some("turn".to_owned()),
        message: "The selected model\u{1b}[31m rejected this request; choose /model and retry."
            .to_owned(),
    };
    let failure = screen(&state, &UiState::default(), 54, 18);
    assert!(failure.contains("Turn failed"));
    assert!(failure.contains("selected model[31m rejected"));
    assert!(failure.contains("/model and retry."));
    assert!(!failure.contains('\u{1b}'));
}

#[test]
fn thinking_panel_renders_closed_open_narrow_streaming_and_error_states() {
    let mut state = ready();
    state.context_remaining_percent = Some(73);
    let closed = screen(&state, &UiState::default(), 100, 20);
    assert!(!closed.contains("Only reasoning content"));
    assert!(header(&state, 100).ends_with("Context 73%"));

    state.thinking.visible = true;
    state.turn = TurnState::Streaming {
        thread_id: "thread".to_owned(),
        turn_id: "turn".to_owned(),
    };
    let awaiting = screen(&state, &UiState::default(), 100, 20);
    assert!(awaiting.contains("Reasoning"));
    assert!(awaiting.contains("Awaiting emitted reasoning"));

    state.thinking.entries.push(ThinkingEntry {
        provider: crate::provider::ProviderId::Codex,
        turn_id: "turn".to_owned(),
        item_id: "why".to_owned(),
        kind: ThinkingKind::Summary,
        index: 0,
        text: "Checking facts safely".to_owned(),
        completed: false,
    });
    let normal = screen(&state, &UiState::default(), 100, 20);
    assert!(normal.contains("Only reasoning content"));
    assert!(normal.contains("Summary:"));
    assert!(normal.contains("Checking facts safely"));
    assert!(header(&state, 100).ends_with("Context 73%"));

    let narrow = screen(&state, &UiState::default(), 52, 16);
    assert!(narrow.contains("Conversation"));
    assert!(narrow.contains("Reasoning"));
    assert!(narrow.contains("Summary:"));
    assert!(narrow.contains("Message"));

    let minimum_width = screen(&state, &UiState::default(), 36, 12);
    assert!(minimum_width.contains("Conversation"));
    assert!(minimum_width.contains("Reasoning"));
    assert!(minimum_width.contains("Message"));
    assert!(header(&state, 36).ends_with("Context 73%"));

    state.thinking.entries[0].text = format!("{}REASONING-TAIL", "界界界界界界e\u{301} ".repeat(8));
    let wrapped_reasoning = screen(&state, &UiState::default(), 52, 12);
    assert!(wrapped_reasoning.contains("REASONING-TAIL"));
    state.thinking.entries[0].text = "Checking facts safely".to_owned();

    state.turn = TurnState::Failed {
        turn_id: Some("turn".to_owned()),
        message: "model failed".to_owned(),
    };
    let failed = screen(&state, &UiState::default(), 100, 20);
    assert!(failed.contains("Turn failed."));
    assert!(failed.contains("Checking facts safely"));

    state.thinking.entries.clear();
    let empty_failure = screen(&state, &UiState::default(), 100, 20);
    assert!(empty_failure.contains("No reasoning content"));
    assert!(empty_failure.contains("before this turn"));
}
