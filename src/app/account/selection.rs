use super::*;

impl AppState {
    pub(in crate::app) fn current_model(&self) -> Option<&ModelChoice> {
        if self.active_provider != ProviderId::Codex {
            return None;
        }
        self.selected_model
            .as_ref()
            .filter(|key| key.provider == ProviderId::Codex)
            .and_then(|key| self.models.iter().find(|model| model.id == key.id))
    }

    pub(in crate::app) fn validate_selection(&mut self) {
        if self.active_provider != ProviderId::Codex {
            return;
        }
        let had_saved_selection = self.preferences.codex.model_id.is_some()
            || self.preferences.codex.reasoning_effort.is_some();
        let selected = self
            .selected_model
            .as_ref()
            .filter(|key| key.provider == ProviderId::Codex)
            .and_then(|key| self.models.iter().find(|model| model.id == key.id))
            .cloned()
            .or_else(|| self.models.iter().find(|model| model.is_default).cloned())
            .or_else(|| self.models.first().cloned());
        let Some(model) = selected else {
            self.selected_model = None;
            self.selected_reasoning = None;
            return;
        };
        self.selected_model = Some(model.key());
        if !self
            .selected_reasoning
            .as_ref()
            .is_some_and(|effort| model.supported_reasoning_efforts.contains(effort))
        {
            self.selected_reasoning = Some(model.default_reasoning_effort.clone());
            if had_saved_selection {
                self.notice = Some(
                    "saved model or reasoning was unavailable; using the server default".to_owned(),
                );
            }
        }
        self.sync_selection_preferences();
    }

    pub(in crate::app) fn sync_selection_preferences(&mut self) {
        self.preferences.codex.model_id = self
            .selected_model
            .as_ref()
            .filter(|key| key.provider == ProviderId::Codex)
            .map(|key| key.id.clone());
        self.preferences.codex.reasoning_effort = self.selected_reasoning.clone();
    }

    pub(in crate::app) fn sync_active_selection_preferences(&mut self) {
        self.preferences.active_provider = self.active_provider;
        match self.active_provider {
            ProviderId::Codex => self.sync_selection_preferences(),
            ProviderId::OpenRouter => {
                self.preferences.openrouter.selected_model_id = self
                    .selected_model
                    .as_ref()
                    .filter(|key| key.provider == ProviderId::OpenRouter)
                    .map(|key| key.id.clone());
            }
            ProviderId::Claude => {
                self.preferences.claude.selected_model_alias = self
                    .selected_model
                    .as_ref()
                    .filter(|key| key.provider == ProviderId::Claude)
                    .and_then(|key| key.id.parse().ok());
            }
        }
    }

    pub(in crate::app) fn available_model_keys(&self) -> Vec<ModelKey> {
        let mut keys = self.models.iter().map(ModelChoice::key).collect::<Vec<_>>();
        keys.extend(self.openrouter.catalog.iter().filter_map(|model| {
            self.preferences
                .openrouter
                .enabled_model_ids
                .contains(&model.id)
                .then(|| ModelKey::openrouter(model.id.clone()).ok())
                .flatten()
        }));
        if matches!(self.claude.availability, ClaudeAvailability::Ready) {
            keys.extend(
                CLAUDE_MODEL_ALIASES
                    .into_iter()
                    .filter_map(|alias| ModelKey::claude(alias.as_str()).ok()),
            );
        }
        keys
    }

    pub(crate) fn model_key_is_available(&self, key: &ModelKey) -> bool {
        match key.provider {
            ProviderId::Codex => self.models.iter().any(|model| model.id == key.id),
            ProviderId::OpenRouter => {
                self.preferences
                    .openrouter
                    .enabled_model_ids
                    .contains(&key.id)
                    && self
                        .openrouter
                        .catalog
                        .iter()
                        .any(|model| model.id == key.id)
            }
            ProviderId::Claude => {
                matches!(self.claude.availability, ClaudeAvailability::Ready)
                    && key.id.parse::<ClaudeModelAlias>().is_ok()
            }
        }
    }

    pub(crate) fn resolve_provider_selection(
        &self,
        provider: ProviderId,
    ) -> Option<(ModelKey, Option<String>)> {
        match provider {
            ProviderId::Codex => {
                let model = self
                    .preferences
                    .codex
                    .model_id
                    .as_ref()
                    .and_then(|id| self.models.iter().find(|model| &model.id == id))
                    .or_else(|| {
                        self.selected_model
                            .as_ref()
                            .filter(|key| key.provider == ProviderId::Codex)
                            .and_then(|key| self.models.iter().find(|model| model.id == key.id))
                    })
                    .or_else(|| self.models.iter().find(|model| model.is_default))
                    .or_else(|| self.models.first())?;
                let reasoning = self
                    .preferences
                    .codex
                    .reasoning_effort
                    .as_ref()
                    .filter(|effort| model.supported_reasoning_efforts.contains(*effort))
                    .cloned()
                    .unwrap_or_else(|| model.default_reasoning_effort.clone());
                Some((model.key(), Some(reasoning)))
            }
            ProviderId::OpenRouter => {
                let preferred = self
                    .preferences
                    .openrouter
                    .selected_model_id
                    .as_ref()
                    .and_then(|id| ModelKey::openrouter(id.clone()).ok())
                    .filter(|key| self.model_key_is_available(key))
                    .or_else(|| {
                        self.selected_model
                            .as_ref()
                            .filter(|key| {
                                key.provider == ProviderId::OpenRouter
                                    && self.model_key_is_available(key)
                            })
                            .cloned()
                    });
                if let Some(key) = preferred {
                    return Some((key, None));
                }
                let mut enabled = self
                    .openrouter
                    .catalog
                    .iter()
                    .filter(|model| {
                        self.preferences
                            .openrouter
                            .enabled_model_ids
                            .contains(&model.id)
                    })
                    .collect::<Vec<_>>();
                enabled.sort_by(|left, right| {
                    left.name
                        .as_deref()
                        .unwrap_or(&left.id)
                        .cmp(right.name.as_deref().unwrap_or(&right.id))
                        .then_with(|| left.id.cmp(&right.id))
                });
                enabled
                    .first()
                    .and_then(|model| ModelKey::openrouter(model.id.clone()).ok())
                    .map(|key| (key, None))
            }
            ProviderId::Claude => {
                if !matches!(self.claude.availability, ClaudeAvailability::Ready) {
                    return None;
                }
                let alias = self
                    .preferences
                    .claude
                    .selected_model_alias
                    .or_else(|| {
                        self.selected_model
                            .as_ref()
                            .filter(|key| key.provider == ProviderId::Claude)
                            .and_then(|key| key.id.parse().ok())
                    })
                    .unwrap_or(ClaudeModelAlias::Default);
                ModelKey::claude(alias.as_str()).ok().map(|key| (key, None))
            }
        }
    }

    pub(in crate::app) fn commit_provider_selection(
        &mut self,
        provider: ProviderId,
        model: ModelKey,
        reasoning: Option<String>,
    ) -> bool {
        if model.provider != provider || !self.model_key_is_available(&model) {
            return false;
        }
        let reasoning = match provider {
            ProviderId::Codex => {
                let Some(choice) = self.models.iter().find(|choice| choice.id == model.id) else {
                    return false;
                };
                Some(
                    reasoning
                        .filter(|effort| choice.supported_reasoning_efforts.contains(effort))
                        .unwrap_or_else(|| choice.default_reasoning_effort.clone()),
                )
            }
            ProviderId::OpenRouter | ProviderId::Claude => None,
        };
        self.active_provider = provider;
        self.selected_model = Some(model);
        self.selected_reasoning = reasoning;
        self.sync_active_selection_preferences();
        true
    }
}
