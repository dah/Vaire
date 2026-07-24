use super::*;
use crate::openrouter::{ChatRole, OpenRouterTurnRecord};
use crate::provider::OpenRouterTurnId;
use crate::storage::ScriptedDirectorySync;
use serde_json::{json, Value};
use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;

fn turn(
    outcome: OpenRouterTurnOutcome,
    assistant_text: Option<&str>,
    incomplete_assistant_text: Option<&str>,
) -> OpenRouterTurnRecord {
    OpenRouterTurnRecord {
        id: OpenRouterTurnId::new(),
        model_id: "vendor/model".to_owned(),
        user_text: "hello".to_owned(),
        assistant_text: assistant_text.map(str::to_owned),
        incomplete_assistant_text: incomplete_assistant_text.map(str::to_owned),
        outcome,
    }
}

fn completed_conversation() -> OpenRouterConversationV2 {
    let mut conversation =
        OpenRouterConversationV2::new(OpenRouterConversationId::new(), 1, "Test");
    conversation.updated_at_ms = 2;
    conversation
        .turns
        .push(turn(OpenRouterTurnOutcome::Completed, Some("world"), None));
    conversation
}

fn conversation_path(root: &Path, id: &OpenRouterConversationId) -> PathBuf {
    root.join("conversations")
        .join(format!("{}.json", id.as_str()))
}

fn write_owner_only(path: &Path, bytes: &[u8]) {
    fs::write(path, bytes).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}

fn legacy_fixture(id: &OpenRouterConversationId) -> Vec<u8> {
    serde_json::to_vec_pretty(&json!({
        "version": 1,
        "id": id,
        "created_at_ms": 11,
        "updated_at_ms": 22,
        "title": "Legacy",
        "turns": [
            {
                "id": OpenRouterTurnId::new(),
                "model_id": "vendor/model",
                "user_text": "completed user",
                "assistant_text": "completed assistant",
                "outcome": "completed"
            },
            {
                "id": OpenRouterTurnId::new(),
                "model_id": "vendor/model",
                "user_text": "failed user",
                "assistant_text": null,
                "outcome": "failed"
            },
            {
                "id": OpenRouterTurnId::new(),
                "model_id": "vendor/model",
                "user_text": "interrupted user",
                "assistant_text": null,
                "outcome": "interrupted"
            },
            {
                "id": OpenRouterTurnId::new(),
                "model_id": "vendor/model",
                "user_text": "in progress user",
                "assistant_text": null,
                "outcome": "in_progress"
            }
        ]
    }))
    .unwrap()
}

mod durability;
mod migration;
mod validation;
