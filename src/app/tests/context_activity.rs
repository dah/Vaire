use super::*;

#[test]
fn remaining_context_arithmetic_is_honest_clamped_and_overflow_safe() {
    assert_eq!(remaining_context_percent(25, Some(100)), Some(75));
    assert_eq!(remaining_context_percent(1, Some(200)), Some(100));
    assert_eq!(remaining_context_percent(199, Some(200)), Some(1));
    assert_eq!(remaining_context_percent(100, Some(100)), Some(0));
    assert_eq!(remaining_context_percent(150, Some(100)), Some(0));
    assert_eq!(remaining_context_percent(0, Some(i64::MAX)), Some(100));
    assert_eq!(
        remaining_context_percent(i64::MAX - 1, Some(i64::MAX)),
        Some(0)
    );
    assert_eq!(remaining_context_percent(1, None), None);
    assert_eq!(remaining_context_percent(1, Some(0)), None);
    assert_eq!(remaining_context_percent(1, Some(-1)), None);
    assert_eq!(remaining_context_percent(-1, Some(100)), None);
}

#[test]
fn token_usage_is_scoped_and_completed_values_survive_until_a_newer_update() {
    let mut state = active_context_state();
    for (thread_id, turn_id) in [("stale", "turn-1"), ("thr", "stale-turn")] {
        state.reduce(Action::Event(DomainEvent::TokenUsageUpdated {
            thread_id: thread_id.to_owned(),
            turn_id: turn_id.to_owned(),
            context_tokens: 90,
            model_context_window: Some(100),
        }));
    }
    assert_eq!(state.context_remaining_percent, None);

    state.reduce(Action::Event(DomainEvent::TokenUsageUpdated {
        thread_id: "thr".to_owned(),
        turn_id: "turn-1".to_owned(),
        context_tokens: 25,
        model_context_window: Some(100),
    }));
    assert_eq!(state.context_remaining_percent, Some(75));

    state.reduce(Action::Event(DomainEvent::TurnFinished {
        thread_id: "thr".to_owned(),
        turn_id: "turn-1".to_owned(),
        outcome: TurnOutcome::Completed,
    }));
    state.reduce(Action::Event(DomainEvent::TokenUsageUpdated {
        thread_id: "thr".to_owned(),
        turn_id: "turn-1".to_owned(),
        context_tokens: 26,
        model_context_window: Some(100),
    }));
    assert_eq!(state.context_remaining_percent, Some(74));

    state.turn = TurnState::Starting;
    state.reduce(Action::Event(DomainEvent::TokenUsageUpdated {
        thread_id: "thr".to_owned(),
        turn_id: "turn-1".to_owned(),
        context_tokens: 99,
        model_context_window: Some(100),
    }));
    assert_eq!(state.context_remaining_percent, Some(74));
    state.reduce(Action::Event(DomainEvent::TurnStarted {
        thread_id: "thr".to_owned(),
        turn_id: "turn-2".to_owned(),
    }));
    state.reduce(Action::Event(DomainEvent::TokenUsageUpdated {
        thread_id: "thr".to_owned(),
        turn_id: "turn-1".to_owned(),
        context_tokens: 99,
        model_context_window: Some(100),
    }));
    assert_eq!(state.context_remaining_percent, Some(74));
    state.reduce(Action::Event(DomainEvent::TokenUsageUpdated {
        thread_id: "thr".to_owned(),
        turn_id: "turn-2".to_owned(),
        context_tokens: 30,
        model_context_window: Some(100),
    }));
    assert_eq!(state.context_remaining_percent, Some(70));
}

#[test]
fn unusable_relevant_usage_becomes_unknown() {
    let mut state = active_context_state();
    for (context_tokens, model_context_window) in
        [(10, None), (10, Some(0)), (10, Some(-1)), (-1, Some(100))]
    {
        state.context_remaining_percent = Some(80);
        state.reduce(Action::Event(DomainEvent::TokenUsageUpdated {
            thread_id: "thr".to_owned(),
            turn_id: "turn-1".to_owned(),
            context_tokens,
            model_context_window,
        }));
        assert_eq!(state.context_remaining_percent, None);
    }
}

#[test]
fn context_resets_only_when_model_or_account_identity_actually_changes() {
    let scope = AccountScope::from_chatgpt_email("user@example.com");
    let mut state = active_context_state();
    state.connection = ConnectionState::Ready { generation: 1 };
    state.models = vec![
        model("m1", true, &["high"], "high"),
        model("m2", false, &["high"], "high"),
    ];
    state.selected_model = Some(ModelKey::codex("m1").unwrap());
    state.selected_reasoning = Some("high".to_owned());

    state.context_remaining_percent = Some(70);
    state.reduce(Action::Intent(Intent::SelectModel("m1".to_owned())));
    assert_eq!(state.context_remaining_percent, Some(70));
    state.reduce(Action::Intent(Intent::SelectModel("m2".to_owned())));
    assert_eq!(state.context_remaining_percent, None);
    state.reduce(Action::Event(DomainEvent::TokenUsageUpdated {
        thread_id: "thr".to_owned(),
        turn_id: "turn-1".to_owned(),
        context_tokens: 10,
        model_context_window: Some(100),
    }));
    assert_eq!(state.context_remaining_percent, None);
    state.turn = TurnState::Starting;
    state.reduce(Action::Event(DomainEvent::TurnStarted {
        thread_id: "thr".to_owned(),
        turn_id: "turn-2".to_owned(),
    }));
    state.reduce(Action::Event(DomainEvent::TokenUsageUpdated {
        thread_id: "thr".to_owned(),
        turn_id: "turn-2".to_owned(),
        context_tokens: 20,
        model_context_window: Some(100),
    }));
    assert_eq!(state.context_remaining_percent, Some(80));

    state.auth = AuthState::SignedIn {
        scope: scope.clone(),
    };
    state.preferences.codex.auto_resume_thread_id = None;
    state.context_remaining_percent = Some(69);
    seed_thinking(&mut state, "keep me");
    state.reduce(Action::Event(DomainEvent::AccountLoaded(scope)));
    assert_eq!(state.context_remaining_percent, Some(69));
    assert_eq!(state.thinking.entries[0].text, "keep me");
    state.reduce(Action::Event(DomainEvent::AccountLoaded(
        AccountScope::from_chatgpt_email("other@example.com"),
    )));
    assert_eq!(state.context_remaining_percent, None);
    assert!(state.thinking.entries.is_empty());

    state.context_remaining_percent = Some(68);
    state.reduce(Action::Event(DomainEvent::LoggedOut));
    assert_eq!(state.context_remaining_percent, None);
}

#[test]
fn assistant_activity_starts_with_the_turn_and_stops_on_first_nonempty_text() {
    let mut state = AppState {
        connection: ConnectionState::Ready { generation: 1 },
        auth: AuthState::SignedIn { scope: None },
        thread: ThreadState::Ready {
            id: "thr".to_owned(),
        },
        models: vec![model("m1", true, &["high"], "high")],
        selected_model: Some(ModelKey::codex("m1").unwrap()),
        selected_reasoning: Some("high".to_owned()),
        ..AppState::default()
    };
    let preferences = state.preferences.clone();

    assert_eq!(
        state.reduce(Action::Intent(Intent::SendMessage("hello".to_owned()))),
        vec![Effect::SendMessage {
            text: "hello".to_owned(),
        }]
    );
    assert!(state.is_waiting_for_assistant_text());
    assert_eq!(state.transcript.len(), 1);
    assert_eq!(state.transcript[0].role, TranscriptRole::User);

    state.reduce(Action::Event(DomainEvent::TurnStarted {
        thread_id: "thr".to_owned(),
        turn_id: "turn".to_owned(),
    }));
    assert!(state.is_waiting_for_assistant_text());

    state.reduce(Action::Event(DomainEvent::AgentDelta {
        thread_id: "other".to_owned(),
        turn_id: "turn".to_owned(),
        item_id: "stale".to_owned(),
        delta: "ignore me".to_owned(),
    }));
    assert!(state.is_waiting_for_assistant_text());

    state.reduce(Action::Event(DomainEvent::AgentDelta {
        thread_id: "thr".to_owned(),
        turn_id: "turn".to_owned(),
        item_id: "item".to_owned(),
        delta: String::new(),
    }));
    assert!(state.is_waiting_for_assistant_text());

    state.reduce(Action::Event(DomainEvent::AgentDelta {
        thread_id: "thr".to_owned(),
        turn_id: "turn".to_owned(),
        item_id: "item".to_owned(),
        delta: "\u{1b}\u{0007}".to_owned(),
    }));
    assert!(state.is_waiting_for_assistant_text());
    assert_eq!(state.transcript.len(), 1);

    state.reduce(Action::Event(DomainEvent::AgentDelta {
        thread_id: "thr".to_owned(),
        turn_id: "turn".to_owned(),
        item_id: "item".to_owned(),
        delta: "reply".to_owned(),
    }));
    assert!(!state.is_waiting_for_assistant_text());
    assert_eq!(state.transcript.last().unwrap().text, "reply");
    assert_eq!(state.preferences, preferences);
    assert!(state
        .transcript
        .iter()
        .all(|entry| !entry.text.contains('~')));
}

#[test]
fn assistant_activity_stops_on_all_turn_terminal_states() {
    for outcome in [
        TurnOutcome::Completed,
        TurnOutcome::Interrupted,
        TurnOutcome::Failed("model failed".to_owned()),
    ] {
        let mut state = waiting_turn_state();
        assert!(state.is_waiting_for_assistant_text());
        state.reduce(Action::Event(DomainEvent::TurnFinished {
            thread_id: "thr".to_owned(),
            turn_id: "turn".to_owned(),
            outcome,
        }));
        assert!(!state.is_waiting_for_assistant_text());
    }

    let mut operation_failed = waiting_turn_state();
    operation_failed.reduce(Action::Event(DomainEvent::TurnOperationFailed(
        "request failed".to_owned(),
    )));
    assert!(!operation_failed.is_waiting_for_assistant_text());

    for event in [
        DomainEvent::ConnectionFailed("disconnected".to_owned()),
        DomainEvent::ProcessExited("exited".to_owned()),
        DomainEvent::SafetyViolation("tool/request".to_owned()),
    ] {
        let mut state = waiting_turn_state();
        state.reduce(Action::Event(event));
        assert!(!state.is_waiting_for_assistant_text());
    }
}

#[test]
fn assistant_activity_stops_on_account_thread_and_shutdown_transitions() {
    let mut logged_out = waiting_turn_state();
    logged_out.reduce(Action::Event(DomainEvent::LoggedOut));
    assert!(!logged_out.is_waiting_for_assistant_text());

    let mut unsupported_account = waiting_turn_state();
    unsupported_account.reduce(Action::Event(DomainEvent::UnsupportedAccount(
        "api key".to_owned(),
    )));
    assert!(!unsupported_account.is_waiting_for_assistant_text());

    let mut resuming = waiting_turn_state();
    resuming.thread = ThreadState::Resuming {
        id: "other-thread".to_owned(),
    };
    assert!(!resuming.is_waiting_for_assistant_text());

    let mut shutting_down = waiting_turn_state();
    shutting_down.reduce(Action::Intent(Intent::Quit));
    assert!(!shutting_down.is_waiting_for_assistant_text());

    let mut first_thread = waiting_turn_state();
    first_thread.thread = ThreadState::None;
    first_thread.turn = TurnState::Starting;
    assert!(first_thread.is_waiting_for_assistant_text());
    first_thread.reduce(Action::Event(DomainEvent::ThreadStarted {
        id: "thr-new".to_owned(),
    }));
    assert!(first_thread.is_waiting_for_assistant_text());
}
