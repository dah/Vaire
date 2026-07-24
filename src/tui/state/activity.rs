use super::*;

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
}
