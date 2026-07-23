use super::*;

#[test]
fn scoped_streaming_reconciles_utf8_suffix_without_duplication() {
    let mut state = AppState {
        turn: TurnState::Streaming {
            thread_id: "thr".to_owned(),
            turn_id: "turn".to_owned(),
        },
        ..AppState::default()
    };
    state.reduce(Action::Event(DomainEvent::AgentDelta {
        thread_id: "other".to_owned(),
        turn_id: "turn".to_owned(),
        item_id: "item".to_owned(),
        delta: "ignored".to_owned(),
    }));
    state.reduce(Action::Event(DomainEvent::AgentDelta {
        thread_id: "thr".to_owned(),
        turn_id: "turn".to_owned(),
        item_id: "item".to_owned(),
        delta: "hé".to_owned(),
    }));
    state.reduce(Action::Event(DomainEvent::AgentCompleted {
        thread_id: "thr".to_owned(),
        turn_id: "turn".to_owned(),
        item_id: "item".to_owned(),
        text: "héllo".to_owned(),
    }));
    assert_eq!(state.transcript[0].text, "héllo");
    state.reduce(Action::Event(DomainEvent::TurnFinished {
        thread_id: "thr".to_owned(),
        turn_id: "turn".to_owned(),
        outcome: TurnOutcome::Interrupted,
    }));
    assert!(matches!(state.turn, TurnState::Interrupted { .. }));
}

#[test]
fn transcript_retention_is_bounded_without_breaking_stream_reconciliation() {
    let mut state = AppState {
        thread: ThreadState::Ready {
            id: "thr".to_owned(),
        },
        turn: TurnState::Streaming {
            thread_id: "thr".to_owned(),
            turn_id: "turn".to_owned(),
        },
        ..AppState::default()
    };
    let streamed = format!("prefix-{}", "界".repeat(MAX_TRANSCRIPT_BYTES / 3 + 100));
    state.reduce(Action::Event(DomainEvent::AgentDelta {
        thread_id: "thr".to_owned(),
        turn_id: "turn".to_owned(),
        item_id: "item".to_owned(),
        delta: streamed.clone(),
    }));
    assert!(
        state
            .transcript
            .iter()
            .map(|entry| entry.text.len())
            .sum::<usize>()
            <= MAX_TRANSCRIPT_BYTES
    );
    assert!(state
        .transcript_dropped_prefix_bytes
        .contains_key(&("turn".to_owned(), "item".to_owned())));
    assert_eq!(state.transcript_dropped_prefix_bytes.len(), 1);

    let mut contradicted = state.clone();
    let mut contradictory_final = streamed.clone();
    contradictory_final.replace_range(..1, "X");
    contradicted.reduce(Action::Event(DomainEvent::AgentCompleted {
        thread_id: "thr".to_owned(),
        turn_id: "turn".to_owned(),
        item_id: "item".to_owned(),
        text: contradictory_final,
    }));
    assert!(matches!(contradicted.turn, TurnState::Failed { .. }));

    let final_text = format!("{streamed}-tail");
    state.reduce(Action::Event(DomainEvent::AgentCompleted {
        thread_id: "thr".to_owned(),
        turn_id: "turn".to_owned(),
        item_id: "item".to_owned(),
        text: final_text,
    }));
    assert!(matches!(state.turn, TurnState::Streaming { .. }));
    assert!(state.transcript.last().unwrap().text.ends_with("-tail"));
    assert!(
        state
            .transcript
            .iter()
            .map(|entry| entry.text.len())
            .sum::<usize>()
            <= MAX_TRANSCRIPT_BYTES
    );

    state.thread = ThreadState::Resuming {
        id: "thr-history".to_owned(),
    };
    let history = (0..=MAX_TRANSCRIPT_ENTRIES)
        .map(|index| TranscriptEntry {
            provider: crate::provider::ProviderId::Codex,
            role: TranscriptRole::User,
            status: TranscriptEntryStatus::Normal,
            text: format!("history-{index}"),
            item_id: None,
            turn_id: None,
        })
        .collect();
    state.reduce(Action::Event(DomainEvent::ResumeSucceeded {
        id: "thr-history".to_owned(),
        history,
    }));
    assert_eq!(state.transcript.len(), MAX_TRANSCRIPT_ENTRIES);
    assert_eq!(state.transcript.first().unwrap().text, "history-1");
}

#[test]
fn transcript_retention_bounds_newline_and_display_width_floods() {
    let mut state = AppState {
        thread: ThreadState::Ready {
            id: "thr".to_owned(),
        },
        turn: TurnState::Streaming {
            thread_id: "thr".to_owned(),
            turn_id: "turn".to_owned(),
        },
        ..AppState::default()
    };
    state.reduce(Action::Event(DomainEvent::AgentDelta {
        thread_id: "thr".to_owned(),
        turn_id: "turn".to_owned(),
        item_id: "newlines".to_owned(),
        delta: format!("HEAD{}TAIL", "\n".repeat(MAX_TRANSCRIPT_NEWLINES + 70_000)),
    }));
    let retained = &state.transcript.last().unwrap().text;
    assert!(retained.bytes().filter(|byte| *byte == b'\n').count() <= MAX_TRANSCRIPT_NEWLINES);
    assert!(retained.ends_with("TAIL"));

    state.reduce(Action::Event(DomainEvent::AgentDelta {
        thread_id: "thr".to_owned(),
        turn_id: "turn".to_owned(),
        item_id: "wide".to_owned(),
        delta: format!(
            "{}WIDTH-TAIL",
            "x".repeat(MAX_TRANSCRIPT_DISPLAY_COLUMNS + 1_000)
        ),
    }));
    let columns = state.transcript.iter().fold(0usize, |total, entry| {
        total.saturating_add(UnicodeWidthStr::width(entry.text.as_str()))
    });
    assert!(columns <= MAX_TRANSCRIPT_DISPLAY_COLUMNS);
    assert!(state
        .transcript
        .last()
        .unwrap()
        .text
        .ends_with("WIDTH-TAIL"));
    assert!(state.transcript_dropped_prefix_bytes.len() <= 1);
}

#[test]
fn restored_failed_incomplete_status_survives_sanitization_and_bounds() {
    let history = (0..=MAX_TRANSCRIPT_ENTRIES)
        .map(|index| TranscriptEntry {
            provider: ProviderId::OpenRouter,
            role: TranscriptRole::Assistant,
            status: if index == MAX_TRANSCRIPT_ENTRIES {
                TranscriptEntryStatus::FailedIncomplete
            } else {
                TranscriptEntryStatus::Normal
            },
            text: if index == MAX_TRANSCRIPT_ENTRIES {
                "partial\u{1b}[31m".to_owned()
            } else {
                format!("history-{index}")
            },
            item_id: Some("openrouter-assistant".to_owned()),
            turn_id: Some(format!("turn-{index}")),
        })
        .collect();
    let mut state = AppState::default();
    state.replace_transcript(history);

    assert_eq!(state.transcript.len(), MAX_TRANSCRIPT_ENTRIES);
    let retained = state.transcript.last().unwrap();
    assert_eq!(retained.status, TranscriptEntryStatus::FailedIncomplete);
    assert_eq!(retained.text, "partial[31m");
}

#[test]
fn unrelated_turn_started_cannot_replace_the_active_turn() {
    let mut state = AppState {
        thread: ThreadState::Ready {
            id: "thr-active".to_owned(),
        },
        turn: TurnState::Streaming {
            thread_id: "thr-active".to_owned(),
            turn_id: "turn-active".to_owned(),
        },
        ..AppState::default()
    };
    state.reduce(Action::Event(DomainEvent::TurnStarted {
        thread_id: "thr-other".to_owned(),
        turn_id: "turn-other".to_owned(),
    }));
    assert_eq!(
        state.turn,
        TurnState::Streaming {
            thread_id: "thr-active".to_owned(),
            turn_id: "turn-active".to_owned(),
        }
    );
}

#[test]
fn contradictory_final_snapshot_fails_the_turn() {
    let mut state = AppState {
        turn: TurnState::Streaming {
            thread_id: "thr".to_owned(),
            turn_id: "turn".to_owned(),
        },
        ..AppState::default()
    };
    state.append_delta("turn", "item", "alpha");
    state.reduce(Action::Event(DomainEvent::AgentCompleted {
        thread_id: "thr".to_owned(),
        turn_id: "turn".to_owned(),
        item_id: "item".to_owned(),
        text: "beta".to_owned(),
    }));
    assert!(matches!(state.turn, TurnState::Failed { .. }));
}
