use super::*;

#[test]
fn catalog_falls_back_and_model_change_revalidates_reasoning() {
    let mut state = AppState {
        selected_model: Some(ModelKey::codex("missing").unwrap()),
        selected_reasoning: Some("max".to_owned()),
        ..AppState::default()
    };
    state.reduce(Action::Event(DomainEvent::CatalogLoaded(vec![
        model("m1", true, &["low", "high"], "high"),
        model("m2", false, &["low"], "low"),
    ])));
    assert_eq!(state.selected_model, Some(ModelKey::codex("m1").unwrap()));
    assert_eq!(state.selected_reasoning.as_deref(), Some("high"));
    state.reduce(Action::Intent(Intent::SelectModel("m2".to_owned())));
    assert_eq!(state.selected_reasoning.as_deref(), Some("low"));
}

#[test]
fn new_thread_is_eager_and_only_replaces_state_after_success() {
    let mut state = thread_ready_state();
    seed_thinking(&mut state, "old reasoning");
    state.context_remaining_percent = Some(68);
    assert_eq!(
        state.reduce(Action::Intent(Intent::NewThread)),
        vec![Effect::StartNewThread]
    );
    assert_eq!(
        state.preferences.codex.auto_resume_thread_id.as_deref(),
        Some("thr-active")
    );
    assert_eq!(state.transcript[0].text, "old conversation");
    assert_eq!(state.thinking.entries[0].text, "old reasoning");
    assert_eq!(state.context_remaining_percent, Some(68));

    state.reduce(Action::Event(DomainEvent::NewThreadFailed(
        "server rejected it".to_owned(),
    )));
    assert!(matches!(&state.thread, ThreadState::Ready { id } if id == "thr-active"));
    assert_eq!(
        state.preferences.codex.auto_resume_thread_id.as_deref(),
        Some("thr-active")
    );
    assert_eq!(state.transcript.len(), 1);
    assert_eq!(state.thinking.entries[0].text, "old reasoning");
    assert_eq!(state.context_remaining_percent, Some(68));

    assert_eq!(
        state.reduce(Action::Intent(Intent::NewThread)),
        vec![Effect::StartNewThread]
    );
    let effects = state.reduce(Action::Event(DomainEvent::NewThreadSucceeded {
        id: "thr-new".to_owned(),
    }));
    assert!(matches!(&state.thread, ThreadState::Ready { id } if id == "thr-new"));
    assert!(state.transcript.is_empty());
    assert!(state.thinking.entries.is_empty());
    assert!(state.thinking.visible);
    assert_eq!(state.context_remaining_percent, None);
    assert!(matches!(state.turn, TurnState::Idle));
    assert_eq!(
        state.preferences.codex.auto_resume_thread_id.as_deref(),
        Some("thr-new")
    );
    assert!(matches!(effects.as_slice(), [Effect::Persist(_)]));
}

#[test]
fn new_thread_creation_is_single_flight_and_scoped_to_the_starting_account() {
    let mut state = thread_ready_state();
    assert_eq!(
        state.reduce(Action::Intent(Intent::NewThread)),
        vec![Effect::StartNewThread]
    );
    assert!(state.reduce(Action::Intent(Intent::NewThread)).is_empty());
    assert!(state
        .reduce(Action::Intent(Intent::SendMessage("race".to_owned())))
        .is_empty());
    assert_eq!(state.transcript.len(), 1);

    state.reduce(Action::Event(DomainEvent::AccountLoaded(
        AccountScope::from_chatgpt_email("other@example.com"),
    )));
    let switched = state.clone();
    assert!(state
        .reduce(Action::Event(DomainEvent::NewThreadSucceeded {
            id: "thr-created-for-old-account".to_owned(),
        }))
        .is_empty());
    assert_eq!(state, switched);
    assert!(state
        .reduce(Action::Event(DomainEvent::NewThreadFailed(
            "late failure from old account".to_owned(),
        )))
        .is_empty());
    assert_eq!(state, switched);

    assert_eq!(
        state.reduce(Action::Intent(Intent::NewThread)),
        vec![Effect::StartNewThread]
    );
    assert!(matches!(
        state
            .reduce(Action::Event(DomainEvent::NewThreadSucceeded {
                id: "thr-new-account".to_owned(),
            }))
            .as_slice(),
        [Effect::Persist(_)]
    ));
    assert!(matches!(
        state.thread,
        ThreadState::Ready { ref id } if id == "thr-new-account"
    ));
}

#[test]
fn implicit_thread_start_only_attaches_to_the_expected_first_message() {
    let mut state = thread_ready_state();
    let ready = state.clone();
    assert!(state
        .reduce(Action::Event(DomainEvent::ThreadStarted {
            id: "thr-stale".to_owned(),
        }))
        .is_empty());
    assert_eq!(state, ready);

    state.thread = ThreadState::None;
    state.turn = TurnState::Idle;
    let idle = state.clone();
    assert!(state
        .reduce(Action::Event(DomainEvent::ThreadStarted {
            id: "thr-unrequested".to_owned(),
        }))
        .is_empty());
    assert_eq!(state, idle);

    state.turn = TurnState::Starting;
    assert!(matches!(
        state
            .reduce(Action::Event(DomainEvent::ThreadStarted {
                id: "thr-expected".to_owned(),
            }))
            .as_slice(),
        [Effect::Persist(_)]
    ));
    assert!(matches!(
        state.thread,
        ThreadState::Ready { ref id } if id == "thr-expected"
    ));
}

#[test]
fn successful_new_thread_rejects_every_stale_old_turn_event() {
    let mut state = thread_ready_state();
    seed_thinking(&mut state, "old reasoning");
    state.context_remaining_percent = Some(68);

    assert_eq!(
        state.reduce(Action::Intent(Intent::NewThread)),
        vec![Effect::StartNewThread]
    );
    state.reduce(Action::Event(DomainEvent::NewThreadSucceeded {
        id: "thr-new".to_owned(),
    }));
    let replaced = state.clone();

    deliver_stale_old_turn_events(&mut state);

    assert_eq!(state, replaced);
    assert!(matches!(&state.thread, ThreadState::Ready { id } if id == "thr-new"));
    assert!(matches!(state.turn, TurnState::Idle));
    assert!(state.transcript.is_empty());
    assert!(state.thinking.entries.is_empty());
    assert_eq!(state.context_remaining_percent, None);
}

#[test]
fn first_catalog_default_does_not_claim_a_saved_selection_failed() {
    let mut state = AppState::default();
    state.reduce(Action::Event(DomainEvent::CatalogLoaded(vec![model(
        "m1",
        true,
        &["low"],
        "low",
    )])));
    assert_eq!(state.selected_model, Some(ModelKey::codex("m1").unwrap()));
    assert_eq!(state.notice, None);
}
