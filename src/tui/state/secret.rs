use super::*;
use crate::provider::ProviderId;

impl UiState {
    pub fn sync_secret_editor(&mut self, state: &AppState) {
        let Some(PopupState::ProviderSecret {
            provider: ProviderId::OpenRouter,
        }) = state.popup.as_ref()
        else {
            self.clear_secret_state();
            return;
        };
        let busy = !matches!(
            state.openrouter.credential_validation,
            OpenRouterCredentialValidation::Idle
        );
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
            self.secret_editor = Some(SecretEditor::new(ProviderId::OpenRouter));
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
        if provider != ProviderId::OpenRouter {
            self.clear_secret_state();
            return;
        }
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
                    self.overlay = Some(
                        "OpenRouter key must be printable ASCII without whitespace".to_owned(),
                    );
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
                    if openrouter_turn_active(state) {
                        self.overlay = Some(
                            "wait for or interrupt the active OpenRouter turn before replacing its credential"
                                .to_owned(),
                        );
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
                            self.overlay = Some("Enter a valid OpenRouter API key".to_owned());
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

fn openrouter_turn_active(state: &AppState) -> bool {
    matches!(state.turn, TurnState::OpenRouterStreaming { .. })
        || (state.active_provider == ProviderId::OpenRouter && state.turn == TurnState::Starting)
}
