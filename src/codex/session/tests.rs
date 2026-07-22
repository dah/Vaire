use std::path::PathBuf;

use std::collections::HashSet;

use super::{
    history_entries, model_choices, next_cursor, thread_choices, validate_page_len,
    PaginationBudget, SessionError, MAX_CURSOR_BYTES, MAX_PAGINATION_RETAINED_BYTES,
    MAX_THREAD_PAGE_ITEMS,
};
use crate::codex::protocol::{
    ModelInfo, ReasoningEffortOption, SessionSource, SessionSourceName, ThreadItem,
    ThreadItemContent, ThreadListEntry, ThreadSnapshot, TurnSnapshot, TurnStatus, UserInput,
};

#[test]
fn converts_catalog_and_restores_only_conversation_history() {
    let choices = model_choices(&[ModelInfo {
        id: "m".to_owned(),
        display_name: "Model".to_owned(),
        is_default: true,
        default_reasoning_effort: "high".to_owned(),
        supported_reasoning_efforts: vec![ReasoningEffortOption {
            reasoning_effort: "high".to_owned(),
            description: "deep".to_owned(),
        }],
        hidden: false,
    }]);
    assert_eq!(choices[0].supported_reasoning_efforts, vec!["high"]);
    let thread = ThreadSnapshot {
        id: "thr".to_owned(),
        turns: vec![TurnSnapshot {
            id: "turn".to_owned(),
            status: TurnStatus::Completed,
            error: None,
            items: vec![
                ThreadItem {
                    id: "u".to_owned(),
                    kind: "userMessage".to_owned(),
                    text: None,
                    content: vec![UserInput::text("hello").into()],
                    summary: vec![],
                },
                ThreadItem {
                    id: "tool".to_owned(),
                    kind: "commandExecution".to_owned(),
                    text: None,
                    content: vec![],
                    summary: vec![],
                },
                ThreadItem {
                    id: "why".to_owned(),
                    kind: "reasoning".to_owned(),
                    text: None,
                    content: vec![ThreadItemContent::Text("emitted detail".to_owned())],
                    summary: vec!["checked facts".to_owned()],
                },
                ThreadItem {
                    id: "a".to_owned(),
                    kind: "agentMessage".to_owned(),
                    text: Some("hi".to_owned()),
                    content: vec![],
                    summary: vec![],
                },
            ],
        }],
    };
    let history = history_entries(&thread);
    assert_eq!(history.len(), 2);
    assert_eq!(history[1].text, "hi");
}

#[test]
fn thread_choices_prefer_names_then_preview_then_fallback() {
    let choices = thread_choices(vec![
        ThreadListEntry {
            id: "named".to_owned(),
            name: Some("  A name  ".to_owned()),
            preview: "ignored".to_owned(),
            created_at: 1,
            updated_at: 3,
            cwd: PathBuf::from("/tmp/conversation"),
            ephemeral: false,
            source: SessionSource::Named(SessionSourceName::AppServer),
        },
        ThreadListEntry {
            id: "preview".to_owned(),
            name: None,
            preview: "  First line\nsecond line".to_owned(),
            created_at: 1,
            updated_at: 2,
            cwd: PathBuf::from("/tmp/conversation"),
            ephemeral: false,
            source: SessionSource::Named(SessionSourceName::Vscode),
        },
        ThreadListEntry {
            id: "empty".to_owned(),
            name: Some(" ".to_owned()),
            preview: String::new(),
            created_at: 1,
            updated_at: 1,
            cwd: PathBuf::from("/tmp/conversation"),
            ephemeral: false,
            source: SessionSource::Named(SessionSourceName::AppServer),
        },
    ]);
    assert_eq!(choices[0].title, "A name");
    assert_eq!(choices[1].title, "First line");
    assert_eq!(choices[2].title, "Untitled thread");
}

#[test]
fn pagination_limits_bound_page_shape_cursor_and_retained_memory() {
    assert!(matches!(
        validate_page_len("thread/list", MAX_THREAD_PAGE_ITEMS + 1, MAX_THREAD_PAGE_ITEMS),
        Err(SessionError::Protocol(message)) if message.contains("page item limit")
    ));

    let mut seen = HashSet::new();
    assert!(matches!(
        next_cursor(
            "model/list",
            1,
            &mut seen,
            Some("x".repeat(MAX_CURSOR_BYTES + 1)),
        ),
        Err(SessionError::Protocol(message)) if message.contains("pagination cursor")
    ));

    let mut budget = PaginationBudget::default();
    budget
        .retain("model/list", MAX_PAGINATION_RETAINED_BYTES)
        .unwrap();
    assert!(matches!(
        budget.retain("model/list", 1),
        Err(SessionError::Protocol(message)) if message.contains("retained byte limit")
    ));
}
