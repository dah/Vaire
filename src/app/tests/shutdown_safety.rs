use super::*;

#[test]
fn quitting_interrupts_before_the_ordered_shutdown_path() {
    let mut state = AppState {
        turn: TurnState::Streaming {
            thread_id: "thr".to_owned(),
            turn_id: "turn".to_owned(),
        },
        ..AppState::default()
    };
    let effects = state.reduce(Action::Intent(Intent::Quit));
    assert!(matches!(
        effects.as_slice(),
        [Effect::InterruptTurn { .. }, Effect::Shutdown]
    ));
    assert!(state.shutting_down);
}

#[test]
fn settled_turns_and_shutdown_ignore_late_mutating_results() {
    let mut state = thread_ready_state();
    let settled = state.clone();
    assert!(state
        .reduce(Action::Event(DomainEvent::TurnOperationFailed(
            "late interrupt failure".to_owned(),
        )))
        .is_empty());
    assert_eq!(state, settled);

    let effects = state.reduce(Action::Intent(Intent::Quit));
    assert_eq!(effects, vec![Effect::Shutdown]);
    let shutting_down = state.clone();
    for action in [
        Action::Intent(Intent::Quit),
        Action::Intent(Intent::NewThread),
        Action::Intent(Intent::SendMessage("too late".to_owned())),
        Action::Event(DomainEvent::NewThreadSucceeded {
            id: "thr-too-late".to_owned(),
        }),
        Action::Event(DomainEvent::ResumeSucceeded {
            id: "thr-too-late".to_owned(),
            history: Vec::new(),
        }),
    ] {
        assert!(state.reduce(action).is_empty());
        assert_eq!(state, shutting_down);
    }
}

#[test]
fn safety_violation_settles_busy_picker_and_pending_thread_work() {
    let mut state = thread_ready_state();
    assert_eq!(
        state.reduce(Action::Intent(Intent::NewThread)),
        vec![Effect::StartNewThread]
    );
    state.popup = conversation_popup(ThreadPickerState {
        phase: ThreadPickerPhase::Deleting { requested: 1 },
        threads: vec![thread("thr-old", "Old", 1)],
        selected: 0,
        confirmation: None,
        message: None,
    });

    state.reduce(Action::Event(DomainEvent::SafetyViolation(
        "unknown/request".to_owned(),
    )));

    let picker = state.conversation_popup().unwrap();
    assert!(matches!(picker.phase, ThreadPickerPhase::Failed));
    assert!(picker.message.as_deref().unwrap().contains("denied"));
    let snapshot = state.clone();
    state.reduce(Action::Event(DomainEvent::NewThreadSucceeded {
        id: "thr-too-late".to_owned(),
    }));
    assert_eq!(state, snapshot);
}
