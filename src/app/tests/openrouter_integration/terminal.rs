use super::*;

#[test]
fn failed_openrouter_partial_reconciles_and_marks_only_the_matching_live_entry() {
    let (mut state, conversation_id, turn_id) = streaming_openrouter_state();
    state.reduce(Action::Event(DomainEvent::OpenRouterDelta {
        conversation_id: conversation_id.clone(),
        turn_id: turn_id.clone(),
        delta: "partial".to_owned(),
    }));
    state.reduce(Action::Event(DomainEvent::OpenRouterTurnFinished {
        conversation_id,
        turn_id: turn_id.clone(),
        outcome: TurnOutcome::Failed("provider failed".to_owned()),
        assistant_text: None,
        incomplete_assistant_text: Some("partial tail".to_owned()),
        failure_stage: Some(OpenRouterStreamStage::CompletionShape),
    }));

    assert!(matches!(
        &state.turn,
        TurnState::Failed { message, .. }
            if message == "provider failed; stream stage CompletionShape"
    ));
    assert_eq!(state.transcript.len(), 1);
    assert_eq!(state.transcript[0].text, "partial tail");
    assert_eq!(
        state.transcript[0].status,
        TranscriptEntryStatus::FailedIncomplete
    );
    assert_eq!(
        state.transcript[0].turn_id.as_deref(),
        Some(turn_id.as_str())
    );
}

#[test]
fn completed_and_interrupted_openrouter_terminals_keep_normal_status() {
    let (mut completed, conversation_id, turn_id) = streaming_openrouter_state();
    completed.reduce(Action::Event(DomainEvent::OpenRouterDelta {
        conversation_id: conversation_id.clone(),
        turn_id: turn_id.clone(),
        delta: "done".to_owned(),
    }));
    completed.reduce(Action::Event(DomainEvent::OpenRouterTurnFinished {
        conversation_id,
        turn_id,
        outcome: TurnOutcome::Completed,
        assistant_text: Some("done final".to_owned()),
        incomplete_assistant_text: Some("ignored invalid partial".to_owned()),
        failure_stage: None,
    }));
    assert_eq!(completed.transcript[0].text, "done final");
    assert_eq!(
        completed.transcript[0].status,
        TranscriptEntryStatus::Normal
    );

    let (mut interrupted, conversation_id, turn_id) = streaming_openrouter_state();
    interrupted.reduce(Action::Event(DomainEvent::OpenRouterDelta {
        conversation_id: conversation_id.clone(),
        turn_id: turn_id.clone(),
        delta: "live only".to_owned(),
    }));
    interrupted.reduce(Action::Event(DomainEvent::OpenRouterTurnFinished {
        conversation_id,
        turn_id,
        outcome: TurnOutcome::Interrupted,
        assistant_text: Some("ignored completed".to_owned()),
        incomplete_assistant_text: Some("ignored incomplete".to_owned()),
        failure_stage: None,
    }));
    assert_eq!(interrupted.transcript[0].text, "live only");
    assert_eq!(
        interrupted.transcript[0].status,
        TranscriptEntryStatus::Normal
    );
}

#[test]
fn stale_or_contradictory_failed_terminal_cannot_mark_streamed_text_authoritative() {
    let (mut stale, conversation_id, turn_id) = streaming_openrouter_state();
    stale.reduce(Action::Event(DomainEvent::OpenRouterDelta {
        conversation_id: conversation_id.clone(),
        turn_id: turn_id.clone(),
        delta: "active".to_owned(),
    }));
    stale.reduce(Action::Event(DomainEvent::OpenRouterTurnFinished {
        conversation_id: OpenRouterConversationId::new(),
        turn_id: turn_id.clone(),
        outcome: TurnOutcome::Failed("stale".to_owned()),
        assistant_text: None,
        incomplete_assistant_text: Some("active stale".to_owned()),
        failure_stage: Some(OpenRouterStreamStage::AfterDone),
    }));
    assert!(matches!(stale.turn, TurnState::OpenRouterStreaming { .. }));
    assert_eq!(stale.transcript[0].text, "active");
    assert_eq!(stale.transcript[0].status, TranscriptEntryStatus::Normal);

    stale.reduce(Action::Event(DomainEvent::OpenRouterTurnFinished {
        conversation_id,
        turn_id,
        outcome: TurnOutcome::Failed("provider failed".to_owned()),
        assistant_text: None,
        incomplete_assistant_text: Some("contradiction".to_owned()),
        failure_stage: None,
    }));
    assert!(matches!(stale.turn, TurnState::Failed { .. }));
    assert_eq!(stale.transcript[0].text, "active");
    assert_eq!(stale.transcript[0].status, TranscriptEntryStatus::Normal);
    assert_eq!(
        stale.notice.as_deref(),
        Some("OpenRouter final response contradicted streamed text")
    );
}
