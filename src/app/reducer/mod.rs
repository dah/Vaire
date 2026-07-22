use super::*;

impl AppState {
    pub fn reduce(&mut self, action: Action) -> Vec<Effect> {
        if self.shutting_down {
            return Vec::new();
        }
        match action {
            Action::Intent(intent) => self.reduce_intent(intent),
            Action::Event(event) => self.reduce_event(event),
        }
    }
}

mod event;
mod intent;
