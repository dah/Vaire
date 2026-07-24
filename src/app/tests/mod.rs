use super::thinking::{MAX_THINKING_BYTES, MAX_THINKING_ENTRIES};
use super::transcript::{
    MAX_MESSAGE_BYTES, MAX_TRANSCRIPT_BYTES, MAX_TRANSCRIPT_DISPLAY_COLUMNS,
    MAX_TRANSCRIPT_ENTRIES, MAX_TRANSCRIPT_NEWLINES,
};
use super::*;

fn conversation_popup(picker: ThreadPickerState) -> Option<PopupState> {
    Some(PopupState::Conversation(picker))
}

fn model(id: &str, default: bool, efforts: &[&str], default_effort: &str) -> ModelChoice {
    ModelChoice {
        id: id.to_owned(),
        display_name: id.to_owned(),
        is_default: default,
        default_reasoning_effort: default_effort.to_owned(),
        supported_reasoning_efforts: efforts.iter().map(|value| (*value).to_owned()).collect(),
    }
}

fn thread(id: &str, title: &str, updated_at: i64) -> ThreadChoice {
    ThreadChoice {
        provider: ProviderId::Codex,
        id: id.to_owned(),
        title: title.to_owned(),
        updated_at,
    }
}

fn seed_thinking(state: &mut AppState, text: &str) {
    state.thinking.visible = true;
    state.thinking.entries.push(ThinkingEntry {
        provider: crate::provider::ProviderId::Codex,
        turn_id: "turn-old".to_owned(),
        item_id: "thinking-old".to_owned(),
        kind: ThinkingKind::Summary,
        index: 0,
        text: text.to_owned(),
        completed: true,
    });
}

fn deliver_stale_old_turn_events(state: &mut AppState) {
    state.reduce(Action::Event(DomainEvent::ThinkingDelta {
        thread_id: "thr-active".to_owned(),
        turn_id: "turn-old".to_owned(),
        item_id: "thinking-old".to_owned(),
        kind: ThinkingKind::Summary,
        index: 0,
        delta: "stale delta".to_owned(),
    }));
    state.reduce(Action::Event(DomainEvent::ThinkingCompleted {
        thread_id: "thr-active".to_owned(),
        turn_id: "turn-old".to_owned(),
        item_id: "thinking-old".to_owned(),
        summary: vec!["stale final reasoning".to_owned()],
        content: vec!["stale emitted detail".to_owned()],
    }));
    state.reduce(Action::Event(DomainEvent::AgentDelta {
        thread_id: "thr-active".to_owned(),
        turn_id: "turn-old".to_owned(),
        item_id: "agent-old".to_owned(),
        delta: "stale assistant text".to_owned(),
    }));
    state.reduce(Action::Event(DomainEvent::TurnFinished {
        thread_id: "thr-active".to_owned(),
        turn_id: "turn-old".to_owned(),
        outcome: TurnOutcome::Failed("stale failure".to_owned()),
    }));
    state.reduce(Action::Event(DomainEvent::TokenUsageUpdated {
        thread_id: "thr-active".to_owned(),
        turn_id: "turn-old".to_owned(),
        context_tokens: 99,
        model_context_window: Some(100),
    }));
}

fn thread_ready_state() -> AppState {
    let scope = AccountScope::from_chatgpt_email("user@example.com");
    AppState {
        connection: ConnectionState::Ready { generation: 1 },
        auth: AuthState::SignedIn {
            scope: scope.clone(),
        },
        thread: ThreadState::Ready {
            id: "thr-active".to_owned(),
        },
        turn: TurnState::Completed {
            turn_id: "turn-old".to_owned(),
        },
        models: vec![model("m1", true, &["high"], "high")],
        selected_model: Some(ModelKey::codex("m1").unwrap()),
        selected_reasoning: Some("high".to_owned()),
        transcript: vec![TranscriptEntry {
            provider: crate::provider::ProviderId::Codex,
            role: TranscriptRole::Assistant,
            status: TranscriptEntryStatus::Normal,
            text: "old conversation".to_owned(),
            item_id: None,
            turn_id: None,
        }],
        preferences: PreferencesV3 {
            codex: CodexPreferencesV2 {
                account_scope: scope,
                auto_resume_thread_id: Some("thr-active".to_owned()),
                model_id: Some("m1".to_owned()),
                reasoning_effort: Some("high".to_owned()),
                thread_account_scopes: [
                    "thr-active",
                    "thr-old",
                    "thr-old-a",
                    "thr-old-b",
                    "thr-old-c",
                ]
                .into_iter()
                .map(|id| {
                    (
                        id.to_owned(),
                        AccountScope::from_chatgpt_email("user@example.com").unwrap(),
                    )
                })
                .collect(),
            },
            ..PreferencesV3::default()
        },
        ..AppState::default()
    }
}

fn waiting_turn_state() -> AppState {
    AppState {
        connection: ConnectionState::Ready { generation: 1 },
        auth: AuthState::SignedIn { scope: None },
        thread: ThreadState::Ready {
            id: "thr".to_owned(),
        },
        turn: TurnState::Streaming {
            thread_id: "thr".to_owned(),
            turn_id: "turn".to_owned(),
        },
        ..AppState::default()
    }
}

fn active_context_state() -> AppState {
    AppState {
        auth: AuthState::SignedIn {
            scope: AccountScope::from_chatgpt_email("user@example.com"),
        },
        thread: ThreadState::Ready {
            id: "thr".to_owned(),
        },
        turn: TurnState::Streaming {
            thread_id: "thr".to_owned(),
            turn_id: "turn-1".to_owned(),
        },
        ..AppState::default()
    }
}

mod account_auth;
mod claude_integration;
mod context_activity;
mod deletion;
mod openrouter_integration;
mod picker;
mod reasoning;
mod resume;
mod selection_new_thread;
mod shutdown_safety;
mod thinking;
mod transcript;
