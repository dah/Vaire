use serde::{Deserialize, Serialize};

use crate::provider::{ModelKey, ProviderId};
use crate::text::sanitize_terminal_text;

use super::{MAX_MODEL_ID_BYTES, MAX_MODEL_NAME_BYTES};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct OpenRouterModel {
    pub id: String,
    pub name: Option<String>,
    pub context_length: Option<u64>,
}

impl OpenRouterModel {
    pub(crate) fn validate(&self) -> bool {
        ModelKey::new(ProviderId::OpenRouter, self.id.clone()).is_ok()
            && self.id.len() <= MAX_MODEL_ID_BYTES
            && self.name.as_ref().is_none_or(|name| {
                name.len() <= MAX_MODEL_NAME_BYTES
                    && sanitize_terminal_text(name) == *name
                    && !name.contains(['\n', '\r'])
            })
            && self.context_length.is_none_or(|length| length > 0)
    }
}
