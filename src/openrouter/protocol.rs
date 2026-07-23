use serde::Deserialize;

use super::types::OpenRouterModel;

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
    pub choices: Vec<Choice>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct Choice {
    #[serde(default)]
    pub index: u32,
    #[serde(default)]
    pub delta: Delta,
    #[serde(default)]
    pub finish_reason: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct Delta {
    #[serde(default)]
    pub content: Option<String>,
}
