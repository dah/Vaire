use std::collections::BTreeSet;

use super::thread_picker::ThreadPickerState;
use crate::openrouter::OpenRouterModel;
use crate::provider::{ModelKey, ProviderId};

pub(crate) const POPUP_PAGE_ROWS: isize = 10;

pub(crate) fn model_search_matches(key: &ModelKey, search: &str) -> bool {
    let search = search.to_ascii_lowercase();
    search.is_empty()
        || key.id.to_ascii_lowercase().contains(&search)
        || key
            .provider
            .to_string()
            .to_ascii_lowercase()
            .contains(&search)
}

pub(crate) fn catalog_search_matches(model: &OpenRouterModel, search: &str) -> bool {
    let search = search.to_ascii_lowercase();
    search.is_empty()
        || model.id.to_ascii_lowercase().contains(&search)
        || model
            .name
            .as_deref()
            .is_some_and(|name| name.to_ascii_lowercase().contains(&search))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthPopupMode {
    Login,
    Logout,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PopupState {
    Auth {
        mode: AuthPopupMode,
        selected: ProviderId,
    },
    ProviderSecret {
        provider: ProviderId,
    },
    Conversation(ThreadPickerState),
    Model {
        choices: Vec<ModelKey>,
        selected: usize,
        search: String,
    },
    OpenRouterCatalog {
        models: Vec<OpenRouterModel>,
        draft_enabled: BTreeSet<String>,
        selected: usize,
        search: String,
    },
}

impl PopupState {
    pub fn selected_provider(&self) -> Option<ProviderId> {
        match self {
            Self::Auth { selected, .. } => Some(*selected),
            _ => None,
        }
    }
}
