use super::*;

#[test]
fn deletion_confirmation_protects_active_and_supports_cancellation() {
    let mut state = thread_ready_state();
    state.thread_picker = Some(ThreadPickerState {
        phase: ThreadPickerPhase::Ready,
        threads: vec![
            thread("thr-active", "Current", 30),
            thread("thr-old", "Older", 20),
        ],
        selected: 0,
        confirmation: None,
        message: None,
    });
    assert!(state
        .reduce(Action::Intent(Intent::ThreadPickerRequestDelete))
        .is_empty());
    assert!(state.thread_picker.as_ref().unwrap().confirmation.is_none());
    assert!(state
        .thread_picker
        .as_ref()
        .unwrap()
        .message
        .as_deref()
        .unwrap()
        .contains("active"));

    state.reduce(Action::Intent(Intent::ThreadPickerMoveDown));
    state.reduce(Action::Intent(Intent::ThreadPickerRequestDelete));
    assert!(matches!(
        state
            .thread_picker
            .as_ref()
            .and_then(|picker| picker.confirmation.as_ref()),
        Some(ThreadDeleteConfirmation::Selected { target }) if target.id == "thr-old"
    ));
    assert!(state
        .reduce(Action::Intent(Intent::ThreadPickerCancelDelete))
        .is_empty());
    assert!(state.thread_picker.as_ref().unwrap().confirmation.is_none());

    state.reduce(Action::Intent(Intent::ThreadPickerRequestDelete));
    assert_eq!(
        state.reduce(Action::Intent(Intent::ThreadPickerConfirmDelete)),
        vec![Effect::DeleteThreads {
            ids: vec!["thr-old".to_owned()]
        }]
    );
    state.reduce(Action::Event(DomainEvent::ThreadDeletionFinished {
        requested: 1,
        deleted: vec!["thr-old".to_owned()],
        failures: vec![],
    }));
    assert_eq!(state.preferences.thread_id.as_deref(), Some("thr-active"));
    assert_eq!(state.thread_picker.as_ref().unwrap().threads.len(), 1);
}

#[test]
fn clear_inactive_reports_partial_failures_and_never_removes_active_saved_id() {
    let mut state = thread_ready_state();
    state.thread_picker = Some(ThreadPickerState {
        phase: ThreadPickerPhase::Ready,
        threads: vec![
            thread("thr-active", "Current", 30),
            thread("thr-old-a", "Old A", 20),
            thread("thr-old-b", "Old B", 10),
        ],
        selected: 0,
        confirmation: None,
        message: None,
    });
    state.reduce(Action::Intent(Intent::ThreadPickerRequestClearInactive));
    let targets = state
        .thread_picker
        .as_ref()
        .unwrap()
        .confirmation
        .as_ref()
        .unwrap()
        .targets();
    assert_eq!(
        targets
            .iter()
            .map(|target| target.id.as_str())
            .collect::<Vec<_>>(),
        vec!["thr-old-a", "thr-old-b"]
    );
    assert_eq!(
        state.reduce(Action::Intent(Intent::ThreadPickerConfirmDelete)),
        vec![Effect::DeleteThreads {
            ids: vec!["thr-old-a".to_owned(), "thr-old-b".to_owned()]
        }]
    );
    state.reduce(Action::Event(DomainEvent::ThreadDeletionFinished {
        requested: 2,
        deleted: vec!["thr-old-a".to_owned()],
        failures: vec![ThreadDeletionFailure {
            id: "thr-old-b".to_owned(),
            message: "permission denied".to_owned(),
        }],
    }));
    let picker = state.thread_picker.as_ref().unwrap();
    assert_eq!(state.preferences.thread_id.as_deref(), Some("thr-active"));
    assert_eq!(
        picker
            .threads
            .iter()
            .map(|thread| thread.id.as_str())
            .collect::<Vec<_>>(),
        vec!["thr-active", "thr-old-b"]
    );
    assert!(picker.message.as_deref().unwrap().contains("failed"));
    assert!(!picker
        .message
        .as_deref()
        .unwrap()
        .contains("active saved thread"));
}

#[test]
fn deletion_results_must_exactly_match_the_confirmed_target_set() {
    let mut state = thread_ready_state();
    state.thread_picker = Some(ThreadPickerState {
        phase: ThreadPickerPhase::Ready,
        threads: vec![
            thread("thr-active", "Current", 30),
            thread("thr-old-a", "Old A", 20),
            thread("thr-old-b", "Old B", 10),
        ],
        selected: 1,
        confirmation: None,
        message: None,
    });
    state.reduce(Action::Intent(Intent::ThreadPickerRequestDelete));
    assert_eq!(
        state.reduce(Action::Intent(Intent::ThreadPickerConfirmDelete)),
        vec![Effect::DeleteThreads {
            ids: vec!["thr-old-a".to_owned()]
        }]
    );
    let preferences = state.preferences.clone();

    state.reduce(Action::Event(DomainEvent::ThreadDeletionFinished {
        requested: 1,
        deleted: vec!["thr-old-b".to_owned()],
        failures: vec![],
    }));

    let picker = state.thread_picker.as_ref().unwrap();
    assert!(matches!(picker.phase, ThreadPickerPhase::Failed));
    assert_eq!(picker.threads.len(), 3);
    assert_eq!(state.preferences, preferences);
    assert!(picker.message.as_deref().unwrap().contains("did not match"));
}
