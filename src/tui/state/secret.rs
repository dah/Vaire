use super::*;
use crate::provider::ProviderId;

impl UiState {
    pub fn sync_secret_editor(&mut self, state: &AppState) {
        let Some(PopupState::ProviderSecret { provider }) = state.popup.as_ref() else {
            self.clear_secret_state();
            return;
        };
        let provider = *provider;
        let busy = match provider {
            ProviderId::OpenRouter => !matches!(
                state.openrouter.credential_validation,
                OpenRouterCredentialValidation::Idle
            ),
            ProviderId::Claude => !matches!(
                state.claude.credential_validation,
                crate::app::ClaudeCredentialValidation::Idle
            ),
            ProviderId::Codex => true,
        };
        if busy {
            self.clear_secret_state();
            return;
        }
        if self.secret_submission_pending && state.notice.is_some() {
            self.secret_submission_pending = false;
        }
        if !self.secret_submission_pending
            && self.secret_editor.is_none()
            && self.submitted_secret.is_none()
        {
            self.secret_editor = Some(SecretEditor::new(provider));
        }
    }

    fn clear_secret_state(&mut self) {
        self.secret_editor = None;
        self.submitted_secret = None;
        self.secret_submission_pending = false;
    }

    pub fn secret_mask(&self) -> Option<&'static str> {
        self.secret_editor.as_ref().map(|_| "••••••••")
    }

    pub fn take_submitted_secret(
        &mut self,
    ) -> Option<(ProviderId, crate::credentials::SecretValue)> {
        self.submitted_secret
            .take()
            .map(|submitted| (submitted.provider, submitted.value))
    }

    pub fn restore_provider_secret(
        &mut self,
        provider: ProviderId,
        value: crate::credentials::SecretValue,
    ) {
        self.secret_submission_pending = false;
        self.secret_editor = Some(SecretEditor {
            provider,
            value: value.into_input(),
        });
    }

    pub(super) fn handle_secret_event(&mut self, event: Event, state: &AppState) -> Option<Intent> {
        let Some(editor) = &mut self.secret_editor else {
            return None;
        };
        let provider = editor.provider;
        let provider_name = provider_secret_name(provider);
        match event {
            Event::Paste(value) => {
                let value = value.trim();
                if editor
                    .value
                    .len()
                    .checked_add(value.len())
                    .is_some_and(|length| length <= crate::credentials::MAX_CREDENTIAL_BYTES)
                    && value.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
                {
                    editor.value.push_str(value);
                } else {
                    self.overlay = Some(format!(
                        "{provider_name} key must be printable ASCII without whitespace"
                    ));
                }
                None
            }
            Event::Key(key) if key.kind != KeyEventKind::Release => match key.code {
                KeyCode::Esc => {
                    editor.value.zeroize();
                    self.secret_editor = None;
                    Some(Intent::PopupClose)
                }
                KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    editor.value.zeroize();
                    None
                }
                KeyCode::Backspace => {
                    editor.value.pop();
                    None
                }
                KeyCode::Enter => {
                    if provider_turn_active(state, provider) {
                        self.overlay = Some(format!(
                            "wait for or interrupt the active {provider_name} turn before replacing its credential"
                        ));
                        return None;
                    }
                    let value = std::mem::take(&mut *editor.value);
                    match crate::credentials::SecretValue::from_input(value) {
                        Ok(value) => {
                            self.secret_editor = None;
                            self.submitted_secret = Some(SubmittedSecret { provider, value });
                            self.secret_submission_pending = true;
                        }
                        Err(_) => {
                            self.overlay = Some(format!("Enter a valid {provider_name} API key"));
                        }
                    }
                    None
                }
                KeyCode::Char(character)
                    if !key.modifiers.contains(KeyModifiers::CONTROL)
                        && character.is_ascii_graphic()
                        && editor.value.len() < crate::credentials::MAX_CREDENTIAL_BYTES =>
                {
                    editor.value.push(character);
                    None
                }
                _ => None,
            },
            _ => None,
        }
    }
}

fn provider_turn_active(state: &AppState, provider: ProviderId) -> bool {
    match provider {
        ProviderId::OpenRouter => {
            matches!(state.turn, TurnState::OpenRouterStreaming { .. })
                || (state.active_provider == provider && state.turn == TurnState::Starting)
        }
        ProviderId::Claude => {
            matches!(state.turn, TurnState::ClaudeStreaming { .. })
                || (state.active_provider == provider && state.turn == TurnState::Starting)
        }
        ProviderId::Codex => false,
    }
}

fn provider_secret_name(provider: ProviderId) -> &'static str {
    match provider {
        ProviderId::OpenRouter => "OpenRouter",
        ProviderId::Claude => "Anthropic Console",
        ProviderId::Codex => "Codex",
    }
}
