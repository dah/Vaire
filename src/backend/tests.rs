use super::lifecycle::openrouter_history;
use super::{
    load_notice_message, BackendCoordinator, CompletedItemTracker, Effect,
    MAX_TRACKED_COMPLETED_ITEMS_PER_TURN, MAX_TRACKED_COMPLETED_ITEM_ID_BYTES,
};
use crate::persistence::{FilePreferences, LoadNotice, PreferencesPort, PreferencesV2};
use crate::platform::{BrowserError, BrowserOpener};

#[test]
fn missing_preferences_are_a_quiet_first_run() {
    assert_eq!(load_notice_message(Some(LoadNotice::Missing)), None);
    assert!(load_notice_message(Some(LoadNotice::Corrupt)).is_some());
}

#[test]
fn shared_openrouter_history_restores_failed_partials_with_explicit_status() {
    use crate::app::{TranscriptEntryStatus, TranscriptRole};
    use crate::openrouter::{
        OpenRouterConversationV2, OpenRouterTurnOutcome, OpenRouterTurnRecord,
    };
    use crate::provider::{OpenRouterConversationId, OpenRouterTurnId};

    let mut conversation =
        OpenRouterConversationV2::new(OpenRouterConversationId::new(), 1, "history");
    conversation.turns = vec![
        OpenRouterTurnRecord {
            id: OpenRouterTurnId::new(),
            model_id: "vendor/model".to_owned(),
            user_text: "completed user".to_owned(),
            assistant_text: Some("completed assistant".to_owned()),
            incomplete_assistant_text: None,
            outcome: OpenRouterTurnOutcome::Completed,
        },
        OpenRouterTurnRecord {
            id: OpenRouterTurnId::new(),
            model_id: "vendor/model".to_owned(),
            user_text: "failed user".to_owned(),
            assistant_text: None,
            incomplete_assistant_text: Some("failed partial".to_owned()),
            outcome: OpenRouterTurnOutcome::Failed,
        },
        OpenRouterTurnRecord {
            id: OpenRouterTurnId::new(),
            model_id: "vendor/model".to_owned(),
            user_text: "interrupted user".to_owned(),
            assistant_text: None,
            incomplete_assistant_text: None,
            outcome: OpenRouterTurnOutcome::Interrupted,
        },
    ];

    let history = openrouter_history(&conversation);
    assert_eq!(
        history
            .iter()
            .map(|entry| (entry.role.clone(), entry.status, entry.text.as_str()))
            .collect::<Vec<_>>(),
        vec![
            (
                TranscriptRole::User,
                TranscriptEntryStatus::Normal,
                "completed user",
            ),
            (
                TranscriptRole::Assistant,
                TranscriptEntryStatus::Normal,
                "completed assistant",
            ),
            (
                TranscriptRole::User,
                TranscriptEntryStatus::Normal,
                "failed user",
            ),
            (
                TranscriptRole::Assistant,
                TranscriptEntryStatus::FailedIncomplete,
                "failed partial",
            ),
            (
                TranscriptRole::User,
                TranscriptEntryStatus::Normal,
                "interrupted user",
            ),
        ]
    );
}

#[derive(Clone, Debug)]
struct NoopBrowser;

impl BrowserOpener for NoopBrowser {
    fn open_login_url(&self, _value: &str) -> Result<(), BrowserError> {
        Ok(())
    }
}

#[tokio::test]
async fn future_preferences_remain_byte_for_byte_unchanged_through_all_backend_writes() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("preferences.json");
    let original = b"{\"version\":99,\"future\":{\"preserve\":true}}\n";
    std::fs::write(&path, original).unwrap();
    let preferences = FilePreferences::new(&path);
    let loaded = preferences.load().unwrap();
    assert!(!loaded.may_overwrite);

    let mut backend =
        BackendCoordinator::without_codex(preferences, NoopBrowser, "Codex unavailable".to_owned());
    backend.startup().await.unwrap();
    backend
        .execute_pending(vec![Effect::Persist(PreferencesV2::default())])
        .await
        .unwrap();
    backend.shutdown().await.unwrap();

    assert_eq!(std::fs::read(&path).unwrap(), original);
}

#[tokio::test]
async fn v1_preferences_are_still_resaved_through_the_load_derived_gate() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("preferences.json");
    std::fs::write(
        &path,
        br#"{
          "version": 1,
          "account_scope": {"kind":"chatgpt_email","value":"user@example.com"},
          "thread_id": "thr-v1",
          "model_id": "model-v1",
          "reasoning_effort": "high",
          "thread_account_scopes": {
            "thr-v1": {"kind":"chatgpt_email","value":"user@example.com"}
          }
        }"#,
    )
    .unwrap();
    let mut backend = BackendCoordinator::without_codex(
        FilePreferences::new(&path),
        NoopBrowser,
        "Codex unavailable".to_owned(),
    );

    backend.startup().await.unwrap();

    let migrated: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(migrated["version"], 2);
    assert_eq!(migrated["codex"]["auto_resume_thread_id"], "thr-v1");
}

#[test]
fn active_openrouter_turn_blocks_credential_replacement_and_stale_401_is_inert() {
    let temp = tempfile::tempdir().unwrap();
    let mut backend = BackendCoordinator::without_codex(
        FilePreferences::new(temp.path().join("preferences.json")),
        NoopBrowser,
        "Codex unavailable".to_owned(),
    );
    let conversation_id = crate::provider::OpenRouterConversationId::new();
    let turn_id = crate::provider::OpenRouterTurnId::new();
    backend.state.openrouter.auth = crate::openrouter::OpenRouterAuthStatus::Valid;
    backend.state.turn = crate::app::TurnState::OpenRouterStreaming {
        conversation_id: conversation_id.clone(),
        turn_id: turn_id.clone(),
    };

    let candidate = crate::credentials::SecretValue::from_input("candidate-secret-value").unwrap();
    assert!(backend.accept_openrouter_credential(candidate).is_err());
    assert!(backend
        .state
        .notice
        .as_deref()
        .unwrap()
        .contains("active OpenRouter turn"));

    let stale_conversation = crate::provider::OpenRouterConversationId::new();
    let _ = backend.reduce_openrouter_service_event(
        crate::openrouter::OpenRouterServiceEvent::TurnFinished {
            conversation_id: stale_conversation,
            turn_id,
            outcome: crate::openrouter::OpenRouterTurnOutcome::Failed,
            assistant_text: None,
            incomplete_assistant_text: None,
            usage: None,
            failure: Some(crate::openrouter::OpenRouterFailureCategory::Unauthorized),
            failure_stage: None,
        },
    );
    assert_eq!(
        backend.state.openrouter.auth,
        crate::openrouter::OpenRouterAuthStatus::Valid
    );
}

#[test]
fn service_failed_partial_reaches_the_app_with_failed_incomplete_status() {
    let temp = tempfile::tempdir().unwrap();
    let mut backend = BackendCoordinator::without_codex(
        FilePreferences::new(temp.path().join("preferences.json")),
        NoopBrowser,
        "Codex unavailable".to_owned(),
    );
    let conversation_id = crate::provider::OpenRouterConversationId::new();
    let turn_id = crate::provider::OpenRouterTurnId::new();
    backend.state.active_provider = crate::provider::ProviderId::OpenRouter;
    backend.state.turn = crate::app::TurnState::OpenRouterStreaming {
        conversation_id: conversation_id.clone(),
        turn_id: turn_id.clone(),
    };

    let _ = backend.reduce_openrouter_service_event(
        crate::openrouter::OpenRouterServiceEvent::TurnFinished {
            conversation_id,
            turn_id,
            outcome: crate::openrouter::OpenRouterTurnOutcome::Failed,
            assistant_text: None,
            incomplete_assistant_text: Some("durable partial".to_owned()),
            usage: None,
            failure: Some(crate::openrouter::OpenRouterFailureCategory::InvalidResponse),
            failure_stage: Some(crate::openrouter::OpenRouterStreamStage::CompletionShape),
        },
    );

    assert_eq!(backend.state.transcript.len(), 1);
    assert_eq!(backend.state.transcript[0].text, "durable partial");
    assert_eq!(
        backend.state.transcript[0].status,
        crate::app::TranscriptEntryStatus::FailedIncomplete
    );
    assert!(matches!(
        &backend.state.turn,
        crate::app::TurnState::Failed { message, .. }
            if message
                == "OpenRouter turn failed (InvalidResponse); stream stage CompletionShape"
    ));
}

#[tokio::test]
async fn stale_catalog_result_cannot_restore_a_pending_openrouter_conversation() {
    let temp = tempfile::tempdir().unwrap();
    let mut backend = BackendCoordinator::without_codex(
        FilePreferences::new(temp.path().join("preferences.json")),
        NoopBrowser,
        "Codex unavailable".to_owned(),
    );
    let conversation_id = crate::provider::OpenRouterConversationId::new();
    backend.state.openrouter.credential_validation =
        crate::app::OpenRouterCredentialValidation::Refreshing { operation_id: 9 };
    backend.pending_openrouter_auto_resume = Some(super::PendingOpenRouterAutoResume {
        operation_id: 9,
        conversation_id,
        model_id: Some("vendor/model".to_owned()),
    });

    let effects = backend
        .process_openrouter_service_event(
            crate::openrouter::OpenRouterServiceEvent::CatalogLoaded {
                operation_id: 9,
                catalog: vec![crate::openrouter::OpenRouterModel {
                    id: "vendor/model".to_owned(),
                    name: None,
                    context_length: None,
                }],
            },
        )
        .await;

    assert!(effects.is_empty());
    assert!(backend.pending_openrouter_auto_resume.is_none());
    assert_eq!(
        backend.state.openrouter.conversation,
        crate::app::OpenRouterConversationState::None
    );
}

#[test]
fn completed_items_reset_for_every_local_turn_even_when_server_ids_repeat() {
    let mut tracker = CompletedItemTracker::default();
    tracker.begin_turn("thread", "turn-one");
    tracker.record("thread", "turn-one", "reused-item");
    tracker.observe_turn("thread", "turn-one");
    assert!(tracker.should_ignore("thread", "turn-one", "reused-item"));

    tracker.begin_turn("thread", "turn-one");
    assert!(!tracker.should_ignore("thread", "turn-one", "reused-item"));

    tracker.record("thread", "turn-one", "reused-item");
    tracker.begin_turn("thread", "turn-two");
    assert!(!tracker.should_ignore("thread", "turn-two", "reused-item"));
    assert!(!tracker.should_ignore("thread", "turn-one", "reused-item"));
}

#[test]
fn completed_item_tracking_saturates_closed_at_count_and_byte_bounds() {
    let mut tracker = CompletedItemTracker::default();
    tracker.begin_turn("thread", "count-bound");
    for index in 0..=MAX_TRACKED_COMPLETED_ITEMS_PER_TURN {
        tracker.record("thread", "count-bound", &format!("item-{index}"));
    }
    assert_eq!(tracker.ids.len(), MAX_TRACKED_COMPLETED_ITEMS_PER_TURN);
    assert!(tracker.should_ignore("thread", "count-bound", "untracked-late-item"));
    assert!(!tracker.should_ignore("other-thread", "count-bound", "untracked-late-item"));

    tracker.begin_turn("thread", "byte-bound");
    let at_limit = "x".repeat(MAX_TRACKED_COMPLETED_ITEM_ID_BYTES);
    tracker.record("thread", "byte-bound", &at_limit);
    tracker.record("thread", "byte-bound", "over-limit");
    assert_eq!(tracker.ids.len(), 1);
    assert_eq!(tracker.id_bytes, MAX_TRACKED_COMPLETED_ITEM_ID_BYTES);
    assert!(tracker.should_ignore("thread", "byte-bound", "another-untracked-item"));

    tracker.begin_turn("thread", "fresh-turn");
    assert!(!tracker.should_ignore("thread", "fresh-turn", "new-item"));
}
