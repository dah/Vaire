use super::*;

impl UiState {
    pub fn sync_secret_editor(&mut self, state: &AppState) {
        if matches!(state.popup, Some(PopupState::OpenRouterSecret)) {
            match state.openrouter.credential_validation {
                OpenRouterCredentialValidation::Refreshing { .. }
                | OpenRouterCredentialValidation::Validating { .. } => {
                    self.secret_editor = None;
                    self.submitted_secret = None;
                    self.secret_submission_pending = false;
                }
                OpenRouterCredentialValidation::Idle => {
                    if self.secret_submission_pending && state.notice.is_some() {
                        self.secret_submission_pending = false;
                    }
                    if !self.secret_submission_pending
                        && self.secret_editor.is_none()
                        && self.submitted_secret.is_none()
                    {
                        self.secret_editor = Some(SecretEditor::default());
                    }
                }
            }
        } else {
            self.secret_editor = None;
            self.submitted_secret = None;
            self.secret_submission_pending = false;
        }
    }

    pub fn secret_mask(&self) -> Option<&'static str> {
        self.secret_editor.as_ref().map(|_| "••••••••")
    }

    pub fn take_submitted_secret(&mut self) -> Option<crate::credentials::SecretValue> {
        self.submitted_secret.take()
    }

    pub fn restore_openrouter_secret(&mut self, value: crate::credentials::SecretValue) {
        self.secret_submission_pending = false;
        self.secret_editor = Some(SecretEditor {
            value: value.into_input(),
        });
    }

    pub(super) fn handle_secret_event(&mut self, event: Event, state: &AppState) -> Option<Intent> {
        let Some(editor) = &mut self.secret_editor else {
            return None;
        };
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
                    let openrouter_turn_active = matches!(
                        state.turn,
                        crate::app::TurnState::OpenRouterStreaming { .. }
                    ) || (state.active_provider
                        == crate::provider::ProviderId::OpenRouter
                        && state.turn == crate::app::TurnState::Starting);
                    if openrouter_turn_active {
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
                            self.submitted_secret = Some(value);
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
