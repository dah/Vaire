use super::*;

#[test]
fn thinking_toggle_stream_scope_and_completion_are_deterministic() {
    let mut state = AppState {
        turn: TurnState::Streaming {
            thread_id: "thr".to_owned(),
            turn_id: "turn".to_owned(),
        },
        ..AppState::default()
    };
    assert!(!state.thinking.visible);
    assert!(state
        .reduce(Action::Intent(Intent::ToggleThinking))
        .is_empty());
    assert!(state.thinking.visible);

    state.reduce(Action::Event(DomainEvent::ThinkingDelta {
        thread_id: "stale".to_owned(),
        turn_id: "turn".to_owned(),
        item_id: "why".to_owned(),
        kind: ThinkingKind::Summary,
        index: 0,
        delta: "ignore".to_owned(),
    }));
    state.reduce(Action::Event(DomainEvent::ThinkingDelta {
        thread_id: "thr".to_owned(),
        turn_id: "old-turn".to_owned(),
        item_id: "why".to_owned(),
        kind: ThinkingKind::Summary,
        index: 0,
        delta: "ignore".to_owned(),
    }));
    state.reduce(Action::Event(DomainEvent::ThinkingDelta {
        thread_id: "thr".to_owned(),
        turn_id: "turn".to_owned(),
        item_id: "why".to_owned(),
        kind: ThinkingKind::Summary,
        index: -1,
        delta: "ignore".to_owned(),
    }));
    state.reduce(Action::Event(DomainEvent::ThinkingSummaryPartAdded {
        thread_id: "thr".to_owned(),
        turn_id: "turn".to_owned(),
        item_id: "why".to_owned(),
        summary_index: 0,
    }));
    state.reduce(Action::Event(DomainEvent::ThinkingDelta {
        thread_id: "thr".to_owned(),
        turn_id: "turn".to_owned(),
        item_id: "why".to_owned(),
        kind: ThinkingKind::Summary,
        index: 0,
        delta: "check\u{1b}[31m\ting".to_owned(),
    }));
    state.reduce(Action::Event(DomainEvent::ThinkingDelta {
        thread_id: "thr".to_owned(),
        turn_id: "turn".to_owned(),
        item_id: "why".to_owned(),
        kind: ThinkingKind::EmittedText,
        index: 0,
        delta: "detail".to_owned(),
    }));
    assert_eq!(state.thinking.entries.len(), 2);
    assert_eq!(state.thinking.entries[0].text, "check[31m    ing");
    assert!(!state.thinking.entries[0].text.contains('\u{1b}'));

    state.reduce(Action::Event(DomainEvent::ThinkingCompleted {
        thread_id: "thr".to_owned(),
        turn_id: "turn".to_owned(),
        item_id: "why".to_owned(),
        summary: vec!["checking facts".to_owned()],
        content: vec!["detail complete".to_owned()],
    }));
    assert_eq!(state.thinking.entries[0].text, "checking facts");
    assert_eq!(state.thinking.entries[1].text, "detail complete");
    assert!(state.thinking.entries.iter().all(|entry| entry.completed));

    state.reduce(Action::Event(DomainEvent::TurnFinished {
        thread_id: "thr".to_owned(),
        turn_id: "turn".to_owned(),
        outcome: TurnOutcome::Completed,
    }));
    state.reduce(Action::Event(DomainEvent::ThinkingDelta {
        thread_id: "thr".to_owned(),
        turn_id: "turn".to_owned(),
        item_id: "why".to_owned(),
        kind: ThinkingKind::Summary,
        index: 0,
        delta: " stale suffix".to_owned(),
    }));
    assert_eq!(state.thinking.entries[0].text, "checking facts");
    state.reduce(Action::Intent(Intent::ToggleThinking));
    assert!(!state.thinking.visible);
}

#[test]
fn thinking_retention_is_bounded_and_new_turn_clears_only_content() {
    let mut state = AppState {
        connection: ConnectionState::Ready { generation: 1 },
        auth: AuthState::SignedIn { scope: None },
        thread: ThreadState::Ready {
            id: "thr".to_owned(),
        },
        turn: TurnState::Streaming {
            thread_id: "thr".to_owned(),
            turn_id: "turn".to_owned(),
        },
        models: vec![model("m", true, &["high"], "high")],
        selected_model: Some("m".to_owned()),
        selected_reasoning: Some("high".to_owned()),
        ..AppState::default()
    };
    state.thinking.visible = true;
    let oversized = format!(
        "discard\u{0007}{}tail",
        "界".repeat(MAX_THINKING_BYTES / 3 + 20)
    );
    state.reduce(Action::Event(DomainEvent::ThinkingDelta {
        thread_id: "thr".to_owned(),
        turn_id: "turn".to_owned(),
        item_id: "why".to_owned(),
        kind: ThinkingKind::Summary,
        index: 0,
        delta: oversized,
    }));
    let retained = &state.thinking.entries[0].text;
    assert!(retained.len() <= MAX_THINKING_BYTES);
    assert!(retained.len() >= MAX_THINKING_BYTES.saturating_sub(2));
    assert!(retained.ends_with("tail"));
    assert!(!retained.contains('\u{0007}'));

    state.turn = TurnState::Completed {
        turn_id: "turn".to_owned(),
    };
    assert_eq!(
        state.reduce(Action::Intent(Intent::SendMessage("next".to_owned()))),
        vec![Effect::SendMessage {
            text: "next".to_owned()
        }]
    );
    assert!(state.thinking.entries.is_empty());
    assert!(state.thinking.visible);
}

#[test]
fn thinking_entry_count_evicts_exactly_the_oldest_active_turn_entries() {
    let mut state = AppState {
        turn: TurnState::Streaming {
            thread_id: "thr".to_owned(),
            turn_id: "turn".to_owned(),
        },
        ..AppState::default()
    };

    for index in 0..=(MAX_THINKING_ENTRIES as i64 + 1) {
        state.reduce(Action::Event(DomainEvent::ThinkingDelta {
            thread_id: "thr".to_owned(),
            turn_id: "turn".to_owned(),
            item_id: format!("why-{index}"),
            kind: ThinkingKind::Summary,
            index,
            delta: index.to_string(),
        }));
    }

    assert_eq!(state.thinking.entries.len(), MAX_THINKING_ENTRIES);
    assert_eq!(state.thinking.entries[0].item_id, "why-2");
    assert_eq!(state.thinking.entries[0].text, "2");
    assert_eq!(
        state.thinking.entries.last().unwrap().item_id,
        format!("why-{}", MAX_THINKING_ENTRIES + 1)
    );
    assert!(state
        .thinking
        .entries
        .iter()
        .all(|entry| entry.turn_id == "turn"));
}
