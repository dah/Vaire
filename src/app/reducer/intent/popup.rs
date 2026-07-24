use super::*;

impl AppState {
    pub(super) fn move_popup_selection(&mut self, intent: Intent) {
        let Some(popup) = &mut self.popup else {
            return;
        };
        let movement = |selected: usize, count: usize| match intent {
            Intent::PopupMoveUp => move_bounded(selected, -1, count),
            Intent::PopupMoveDown => move_bounded(selected, 1, count),
            Intent::PopupPageUp => move_bounded(selected, -POPUP_PAGE_ROWS, count),
            Intent::PopupPageDown => move_bounded(selected, POPUP_PAGE_ROWS, count),
            Intent::PopupMoveFirst => 0,
            Intent::PopupMoveLast => count.saturating_sub(1),
            _ => selected,
        };
        match popup {
            PopupState::Auth { selected, .. } => {
                if matches!(intent, Intent::PopupMoveUp | Intent::PopupMoveDown) {
                    *selected = match (intent.clone(), *selected) {
                        (Intent::PopupMoveDown, ProviderId::Codex)
                        | (Intent::PopupMoveUp, ProviderId::Claude) => ProviderId::OpenRouter,
                        (Intent::PopupMoveDown, ProviderId::OpenRouter)
                        | (Intent::PopupMoveUp, ProviderId::Codex) => ProviderId::Claude,
                        (Intent::PopupMoveDown, ProviderId::Claude)
                        | (Intent::PopupMoveUp, ProviderId::OpenRouter) => ProviderId::Codex,
                        _ => *selected,
                    };
                }
            }
            PopupState::Model {
                choices,
                selected,
                search,
            } => {
                let count = choices
                    .iter()
                    .filter(|key| model_search_matches(key, search))
                    .count();
                *selected = movement(*selected, count);
            }
            PopupState::OpenRouterCatalog {
                models,
                selected,
                search,
                ..
            } => {
                let count = models
                    .iter()
                    .filter(|model| catalog_search_matches(model, search))
                    .count();
                *selected = movement(*selected, count);
            }
            PopupState::ProviderSecret { .. } | PopupState::Conversation(_) => {}
        }
    }

    pub(super) fn toggle_catalog_choice(&mut self) {
        let Some(PopupState::OpenRouterCatalog {
            models,
            draft_enabled,
            selected,
            search,
        }) = &mut self.popup
        else {
            return;
        };
        let Some(model) = models
            .iter()
            .filter(|model| catalog_search_matches(model, search))
            .nth(*selected)
        else {
            return;
        };
        if !draft_enabled.remove(&model.id) {
            draft_enabled.insert(model.id.clone());
        }
    }

    pub(super) fn select_popup(&mut self) -> Vec<Effect> {
        let Some(popup) = self.popup.clone() else {
            return Vec::new();
        };
        match popup {
            PopupState::Auth { mode, selected } => match (mode, selected) {
                (AuthPopupMode::Login, ProviderId::Codex) => {
                    self.popup = None;
                    self.reduce_intent(Intent::Login)
                }
                (
                    AuthPopupMode::Login,
                    provider @ (ProviderId::OpenRouter | ProviderId::Claude),
                ) => {
                    self.popup = Some(PopupState::ProviderSecret { provider });
                    Vec::new()
                }
                (AuthPopupMode::Logout, ProviderId::Codex) => {
                    self.popup = None;
                    self.reduce_intent(Intent::Logout)
                }
                (AuthPopupMode::Logout, ProviderId::OpenRouter) => {
                    self.popup = None;
                    vec![Effect::LogoutOpenRouter]
                }
                (AuthPopupMode::Logout, ProviderId::Claude) => {
                    self.popup = None;
                    vec![Effect::LogoutClaude]
                }
            },
            PopupState::Model {
                choices,
                selected,
                search,
            } => {
                let choice = choices
                    .into_iter()
                    .filter(|key| model_search_matches(key, &search))
                    .nth(selected);
                self.popup = None;
                choice.map_or_else(Vec::new, |key| {
                    self.reduce_intent(Intent::SelectProviderModel(key))
                })
            }
            PopupState::OpenRouterCatalog { draft_enabled, .. } => {
                let previous_model = self.selected_model.clone();
                self.preferences.openrouter.enabled_model_ids = draft_enabled;
                self.popup = None;
                self.validate_openrouter_selection();
                if self.active_provider == ProviderId::OpenRouter
                    && previous_model != self.selected_model
                {
                    self.reset_context_window();
                }
                vec![Effect::Persist(self.preferences.clone())]
            }
            PopupState::ProviderSecret { .. } | PopupState::Conversation(_) => Vec::new(),
        }
    }
}

fn move_bounded(selected: usize, delta: isize, count: usize) -> usize {
    if count == 0 {
        return 0;
    }
    selected
        .saturating_add_signed(delta)
        .min(count.saturating_sub(1))
}
