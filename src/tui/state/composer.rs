use super::*;

impl UiState {
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

    pub(super) fn submit(&mut self) -> Option<Intent> {
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

    pub(super) fn append_composer_text(&mut self, value: &str) {
        if !append_sanitized_terminal_text(&mut self.composer, value, MAX_COMPOSER_BYTES) {
            self.overlay = Some(format!(
                "The message size limit is {} KiB. Press Esc, shorten the draft, and try again.",
                MAX_COMPOSER_BYTES / 1_024
            ));
        }
    }
}
