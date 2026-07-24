use super::*;

impl UiState {
    pub(super) fn handle_popup_event(&mut self, event: Event, state: &AppState) -> Option<Intent> {
        let Event::Key(key) = event else {
            return None;
        };
        if key.kind == KeyEventKind::Release {
            return None;
        }
        let popup = state.popup.as_ref()?;
        if matches!(popup, PopupState::Conversation(_)) {
            return self.handle_thread_picker_key(key, state);
        }
        match key.code {
            KeyCode::Esc => Some(Intent::PopupClose),
            KeyCode::Up | KeyCode::Char('k') => Some(Intent::PopupMoveUp),
            KeyCode::Down | KeyCode::Char('j') => Some(Intent::PopupMoveDown),
            KeyCode::PageUp => Some(Intent::PopupPageUp),
            KeyCode::PageDown => Some(Intent::PopupPageDown),
            KeyCode::Home => Some(Intent::PopupMoveFirst),
            KeyCode::End => Some(Intent::PopupMoveLast),
            KeyCode::Enter => Some(Intent::PopupSelect),
            KeyCode::Backspace
                if matches!(
                    popup,
                    PopupState::Model { .. } | PopupState::OpenRouterCatalog { .. }
                ) =>
            {
                Some(Intent::PopupSearchBackspace)
            }
            KeyCode::Char(' ') if matches!(popup, PopupState::OpenRouterCatalog { .. }) => {
                Some(Intent::PopupCatalogToggle)
            }
            KeyCode::Char('c') if matches!(popup, PopupState::Auth { .. }) => {
                Some(Intent::PopupOpenCatalog)
            }
            KeyCode::Char('r') if matches!(popup, PopupState::Auth { .. }) => {
                Some(Intent::PopupRefresh)
            }
            KeyCode::Char('d')
                if matches!(
                    popup,
                    PopupState::Auth {
                        mode: AuthPopupMode::Login,
                        selected: crate::provider::ProviderId::Codex,
                    }
                ) =>
            {
                Some(Intent::LoginDevice)
            }
            KeyCode::Char(character)
                if !key.modifiers.contains(KeyModifiers::CONTROL)
                    && !character.is_control()
                    && matches!(
                        popup,
                        PopupState::Model { .. } | PopupState::OpenRouterCatalog { .. }
                    ) =>
            {
                Some(Intent::PopupSearchAppend(character))
            }
            _ => None,
        }
    }

    pub(super) fn handle_thread_picker_key(
        &mut self,
        key: KeyEvent,
        state: &AppState,
    ) -> Option<Intent> {
        if key.kind == KeyEventKind::Release {
            return None;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Some(Intent::Quit);
        }
        if self.overlay.is_some() {
            if key.code == KeyCode::Esc {
                self.overlay = None;
            }
            return None;
        }
        let picker = state.conversation_popup()?;
        if picker.confirmation.is_some() {
            return match key.code {
                KeyCode::Enter => Some(Intent::ThreadPickerConfirmDelete),
                KeyCode::Esc => Some(Intent::ThreadPickerCancelDelete),
                _ => None,
            };
        }
        match key.code {
            KeyCode::Esc => Some(Intent::ThreadPickerClose),
            KeyCode::Up | KeyCode::Char('k') => Some(Intent::ThreadPickerMoveUp),
            KeyCode::Down | KeyCode::Char('j') => Some(Intent::ThreadPickerMoveDown),
            KeyCode::Enter => Some(Intent::ThreadPickerSelect),
            KeyCode::Char('D') => Some(Intent::ThreadPickerRequestClearInactive),
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::SHIFT) => {
                Some(Intent::ThreadPickerRequestClearInactive)
            }
            KeyCode::Char('d') => Some(Intent::ThreadPickerRequestDelete),
            _ => None,
        }
    }
}
