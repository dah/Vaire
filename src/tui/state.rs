use super::*;

mod activity;
mod composer;
mod popup_input;
mod secret;

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
    pub(in crate::tui) submitted_secret: Option<SubmittedSecret>,
    pub(in crate::tui) secret_submission_pending: bool,
}

#[derive(Debug)]
pub(in crate::tui) struct SubmittedSecret {
    pub(in crate::tui) provider: crate::provider::ProviderId,
    pub(in crate::tui) value: crate::credentials::SecretValue,
}

pub(in crate::tui) struct SecretEditor {
    provider: crate::provider::ProviderId,
    value: Zeroizing<String>,
}

impl SecretEditor {
    fn new(provider: crate::provider::ProviderId) -> Self {
        Self {
            provider,
            value: Zeroizing::new(String::new()),
        }
    }
}

impl std::fmt::Debug for SecretEditor {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("SecretEditor([REDACTED])")
    }
}
impl UiState {
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
