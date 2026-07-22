use super::*;

pub fn model_choices(models: &[ModelInfo]) -> Vec<ModelChoice> {
    models
        .iter()
        .map(|model| ModelChoice {
            id: model.id.clone(),
            display_name: model.display_name.clone(),
            is_default: model.is_default,
            default_reasoning_effort: model.default_reasoning_effort.clone(),
            supported_reasoning_efforts: model
                .supported_reasoning_efforts
                .iter()
                .map(|option| option.reasoning_effort.clone())
                .collect(),
        })
        .collect()
}

pub fn thread_choices(threads: Vec<ThreadListEntry>) -> Vec<ThreadChoice> {
    threads
        .into_iter()
        .map(|thread| {
            let title = thread
                .name
                .as_deref()
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .or_else(|| {
                    thread
                        .preview
                        .lines()
                        .next()
                        .map(str::trim)
                        .filter(|preview| !preview.is_empty())
                })
                .unwrap_or("Untitled thread")
                .to_owned();
            ThreadChoice {
                id: thread.id,
                title,
                updated_at: thread.updated_at,
            }
        })
        .collect()
}

pub fn history_entries(thread: &ThreadSnapshot) -> Vec<TranscriptEntry> {
    let mut entries = Vec::new();
    for turn in &thread.turns {
        for item in &turn.items {
            match item.kind.as_str() {
                "userMessage" => {
                    for input in &item.content {
                        if let ThreadItemContent::UserInput(input) = input {
                            if input.kind == "text" {
                                if let Some(text) = &input.text {
                                    entries.push(TranscriptEntry {
                                        role: TranscriptRole::User,
                                        text: text.clone(),
                                        item_id: Some(item.id.clone()),
                                        turn_id: Some(turn.id.clone()),
                                    });
                                }
                            }
                        }
                    }
                }
                "agentMessage" => {
                    if let Some(text) = &item.text {
                        entries.push(TranscriptEntry {
                            role: TranscriptRole::Assistant,
                            text: text.clone(),
                            item_id: Some(item.id.clone()),
                            turn_id: Some(turn.id.clone()),
                        });
                    }
                }
                _ => {}
            }
        }
    }
    entries
}
