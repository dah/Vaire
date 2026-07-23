use super::*;

pub(in crate::tui) const MAX_COMPOSER_BYTES: usize = 128 * 1_024;
pub(in crate::tui) const ACTIVITY_TICKS_PER_FRAME: u8 = 4;
pub(in crate::tui) const ACTIVITY_FRAMES: [&str; 10] = [
    "~    ", "~~   ", "~~~  ", " ~~~ ", "  ~~~", "   ~~", "    ~", "   ~~", "  ~~~", " ~~~ ",
];

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(in crate::tui) struct ActivityAnimation {
    active: bool,
    frame_index: usize,
    ticks_in_frame: u8,
}

#[derive(Debug, Default)]
pub struct UiState {
    pub composer: String,
    pub overlay: Option<String>,
    /// Number of wrapped transcript rows held above the automatic bottom position.
    pub scroll_from_bottom: usize,
    pub(in crate::tui) activity: ActivityAnimation,
    pub(in crate::tui) secret_editor: Option<SecretEditor>,
    pub(in crate::tui) submitted_secret: Option<crate::credentials::SecretValue>,
    pub(in crate::tui) secret_submission_pending: bool,
}

#[derive(Default)]
pub(in crate::tui) struct SecretEditor {
    value: Zeroizing<String>,
}

impl std::fmt::Debug for SecretEditor {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("SecretEditor([REDACTED])")
    }
}

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
    /// Synchronizes ephemeral animation state after an application-state update.
    ///
    /// The return value tells an event loop whether the visible animation changed.
    pub fn sync_activity_animation(&mut self, state: &AppState) -> bool {
        let needed = state.is_waiting_for_assistant_text();
        match (self.activity.active, needed) {
            (false, true) => {
                self.activity.active = true;
                self.activity.frame_index = 0;
                self.activity.ticks_in_frame = 0;
                true
            }
            (true, false) => {
                self.activity = ActivityAnimation::default();
                true
            }
            _ => false,
        }
    }

    /// Advances the animation from the event loop's existing 33 ms tick.
    ///
    /// It returns `true` only when a redraw is necessary: animation activation, a new frame, or
    /// removal after the surrounding state stopped needing the indicator.
    pub fn advance_activity_animation(&mut self, state: &AppState) -> bool {
        if !state.is_waiting_for_assistant_text() {
            return if self.activity.active {
                self.activity = ActivityAnimation::default();
                true
            } else {
                false
            };
        }
        if !self.activity.active {
            self.activity.active = true;
            self.activity.frame_index = 0;
            self.activity.ticks_in_frame = 0;
            return true;
        }

        self.activity.ticks_in_frame = self.activity.ticks_in_frame.saturating_add(1);
        if self.activity.ticks_in_frame < ACTIVITY_TICKS_PER_FRAME {
            return false;
        }
        self.activity.ticks_in_frame = 0;
        self.activity.frame_index = (self.activity.frame_index + 1) % ACTIVITY_FRAMES.len();
        true
    }

    pub(in crate::tui) fn activity_frame(&self) -> &'static str {
        ACTIVITY_FRAMES[self.activity.frame_index]
    }

    pub fn handle_event(&mut self, event: Event) -> Option<Intent> {
        match event {
            Event::Key(key) => self.handle_key(key),
            Event::Paste(text) if self.overlay.is_none() => {
                self.append_composer_text(&text);
                None
            }
            _ => None,
        }
    }

    pub fn handle_event_for_state(&mut self, event: Event, state: &AppState) -> Option<Intent> {
        self.sync_secret_editor(state);
        if is_ctrl_c(&event) {
            return Some(Intent::Quit);
        }
        if self.secret_editor.is_some() {
            return self.handle_secret_event(event, state);
        }
        if self.overlay.is_some() {
            return match event {
                Event::Key(key)
                    if key.kind != KeyEventKind::Release && key.code == KeyCode::Esc =>
                {
                    self.overlay = None;
                    None
                }
                _ => None,
            };
        }
        if state.popup.is_some() {
            return self.handle_popup_event(event, state);
        }
        self.handle_event(event)
    }

    fn handle_secret_event(&mut self, event: Event, state: &AppState) -> Option<Intent> {
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

    fn handle_popup_event(&mut self, event: Event, state: &AppState) -> Option<Intent> {
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

    fn handle_thread_picker_key(&mut self, key: KeyEvent, state: &AppState) -> Option<Intent> {
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

    pub fn handle_key(&mut self, key: KeyEvent) -> Option<Intent> {
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
        match key.code {
            KeyCode::Esc => Some(Intent::Interrupt),
            KeyCode::Enter if key.modifiers.contains(KeyModifiers::ALT) => {
                self.append_composer_text("\n");
                None
            }
            KeyCode::Enter => self.submit(),
            KeyCode::Backspace => {
                self.composer.pop();
                None
            }
            KeyCode::PageUp | KeyCode::Up => {
                self.scroll_from_bottom = self.scroll_from_bottom.saturating_add(1);
                None
            }
            KeyCode::PageDown | KeyCode::Down => {
                self.scroll_from_bottom = self.scroll_from_bottom.saturating_sub(1);
                None
            }
            KeyCode::Home => {
                self.scroll_from_bottom = usize::MAX / 2;
                None
            }
            KeyCode::End => {
                self.scroll_from_bottom = 0;
                None
            }
            KeyCode::Char(character)
                if !key.modifiers.contains(KeyModifiers::CONTROL) && !character.is_control() =>
            {
                let mut encoded = [0_u8; 4];
                self.append_composer_text(character.encode_utf8(&mut encoded));
                None
            }
            _ => None,
        }
    }

    fn submit(&mut self) -> Option<Intent> {
        match parse(&self.composer) {
            Ok(Intent::Help) => {
                self.overlay = Some(HELP_TEXT.to_owned());
                self.composer.clear();
                None
            }
            Ok(intent) => {
                self.composer.clear();
                self.scroll_from_bottom = 0;
                Some(intent)
            }
            Err(error) => {
                self.overlay = Some(error.to_string());
                None
            }
        }
    }

    fn append_composer_text(&mut self, value: &str) {
        if !append_sanitized_terminal_text(&mut self.composer, value, MAX_COMPOSER_BYTES) {
            self.overlay = Some(format!(
                "The message size limit is {} KiB. Press Esc, shorten the draft, and try again.",
                MAX_COMPOSER_BYTES / 1_024
            ));
        }
    }
}

fn is_ctrl_c(event: &Event) -> bool {
    matches!(
        event,
        Event::Key(key)
            if key.kind != KeyEventKind::Release
                && key.modifiers.contains(KeyModifiers::CONTROL)
                && key.code == KeyCode::Char('c')
    )
}
