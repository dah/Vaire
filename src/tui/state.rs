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

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct UiState {
    pub composer: String,
    pub overlay: Option<String>,
    /// Number of wrapped transcript rows held above the automatic bottom position.
    pub scroll_from_bottom: usize,
    pub(in crate::tui) activity: ActivityAnimation,
}

impl UiState {
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
        if state.thread_picker.is_none() {
            return self.handle_event(event);
        }
        match event {
            Event::Key(key) => self.handle_thread_picker_key(key, state),
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
        let picker = state.thread_picker.as_ref()?;
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
