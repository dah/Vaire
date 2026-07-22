use super::*;

#[test]
fn transcript_bottom_scroll_accounts_for_word_wrap_slack() {
    let mut state = ready();
    state.transcript.push(TranscriptEntry {
        role: TranscriptRole::Assistant,
        text: format!("{}TAIL", "abcdefghijklmnopqr ".repeat(8)),
        item_id: Some("item".to_owned()),
        turn_id: Some("turn".to_owned()),
    });

    let rendered = screen(&state, &UiState::default(), 36, 9);
    assert!(rendered.contains("TAIL"));

    state.transcript[0].text = format!("{}WIDE-TAIL", "界界界界界界界界e\u{301} ".repeat(8));
    let unicode = screen(&state, &UiState::default(), 36, 9);
    assert!(unicode.contains("WIDE-TAIL"));
}

#[test]
fn newline_heavy_streams_remain_bounded_and_render_the_tail() {
    let mut state = ready();
    state.turn = TurnState::Streaming {
        thread_id: "thread".to_owned(),
        turn_id: "turn".to_owned(),
    };
    state.reduce(Action::Event(DomainEvent::AgentDelta {
        thread_id: "thread".to_owned(),
        turn_id: "turn".to_owned(),
        item_id: "item".to_owned(),
        delta: format!("{}TRANSCRIPT-TAIL", "\n".repeat(70_000)),
    }));

    let rendered = screen(&state, &UiState::default(), 36, 9);
    assert!(rendered.contains("TRANSCRIPT-TAIL"));
    assert!(state.transcript[0].text.len() < 70_000);
}

#[test]
fn transcript_and_reasoning_window_past_u16_logical_rows() {
    let mut state = ready();
    state.transcript.push(TranscriptEntry {
        role: TranscriptRole::Assistant,
        text: format!("{}TRANSCRIPT-TAIL", "\n".repeat(usize::from(u16::MAX) + 32)),
        item_id: Some("item".to_owned()),
        turn_id: Some("turn".to_owned()),
    });
    let bottom = screen(&state, &UiState::default(), 52, 12);
    assert!(bottom.contains("TRANSCRIPT-TAIL"));

    let oldest = screen(
        &state,
        &UiState {
            scroll_from_bottom: usize::MAX,
            ..UiState::default()
        },
        52,
        12,
    );
    assert!(oldest.contains("Agent:"));
    assert!(!oldest.contains("TRANSCRIPT-TAIL"));

    state.transcript.clear();
    state.thinking.visible = true;
    state.thinking.entries.push(ThinkingEntry {
        turn_id: "turn".to_owned(),
        item_id: "why".to_owned(),
        kind: ThinkingKind::Summary,
        index: 0,
        text: format!("{}REASONING-TAIL", "\n".repeat(usize::from(u16::MAX) + 32)),
        completed: false,
    });
    let reasoning = screen(&state, &UiState::default(), 52, 12);
    assert!(reasoning.contains("REASONING-TAIL"));
}

#[test]
fn display_wrapping_and_truncation_keep_grapheme_clusters_intact() {
    let wrapped = wrap_for_display("aa👩‍💻", 4);
    assert_eq!(wrapped.text, "aa👩‍💻\n");
    assert_eq!(wrapped.rows, 2);
    assert!(!wrapped.text.contains("👩‍\n💻"));

    let truncated = truncate_for_display("ab👩‍💻cd", 5);
    assert_eq!(truncated, "ab👩‍💻…");
    assert_eq!(UnicodeWidthStr::width(truncated.as_str()), 5);
}
