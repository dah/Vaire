use serde::Deserialize;

use super::types::{OpenRouterModel, TokenUsage};

#[derive(Deserialize)]
pub(crate) struct KeyResponse {
    pub data: serde_json::Value,
}

impl KeyResponse {
    pub(crate) fn is_valid(&self) -> bool {
        self.data.is_object()
    }
}

#[derive(Deserialize)]
pub(crate) struct ModelsResponse {
    pub data: Vec<ModelWire>,
}

#[derive(Deserialize)]
pub(crate) struct ModelWire {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub context_length: Option<u64>,
}

impl From<ModelWire> for OpenRouterModel {
    fn from(value: ModelWire) -> Self {
        Self {
            id: value.id,
            name: value.name,
            context_length: value.context_length,
        }
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct ChatChunk {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub choices: Vec<Choice>,
    #[serde(default)]
    pub usage: Option<TokenUsage>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct Choice {
    #[serde(default)]
    pub index: u32,
    #[serde(default)]
    pub delta: Delta,
    #[serde(default)]
    pub finish_reason: Option<serde_json::Value>,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct Delta {
    #[serde(default)]
    pub content: Option<String>,
}
