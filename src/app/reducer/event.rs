use super::*;

impl AppState {
    pub(in crate::app) fn reduce_event(&mut self, event: DomainEvent) -> Vec<Effect> {
        match event {
            event @ (DomainEvent::PreferencesLoaded(_)
            | DomainEvent::Connecting
            | DomainEvent::Connected { .. }
            | DomainEvent::ConnectionFailed(_)
            | DomainEvent::ProcessExited(_)
            | DomainEvent::AccountLoaded(_)
            | DomainEvent::UnsupportedAccount(_)
            | DomainEvent::LoginStarted { .. }
            | DomainEvent::LoginFailed(_)
            | DomainEvent::LoggedOut
            | DomainEvent::CatalogLoaded(_)) => self.reduce_account_event(event),
            event @ (DomainEvent::ResumeStarted { .. }
            | DomainEvent::ResumeSucceeded { .. }
            | DomainEvent::ResumeFailed { .. }
            | DomainEvent::NewThreadSucceeded { .. }
            | DomainEvent::NewThreadFailed(_)
            | DomainEvent::ThreadListLoaded(_)
            | DomainEvent::ThreadListFailed(_)
            | DomainEvent::ThreadSwitchSucceeded { .. }
            | DomainEvent::ThreadSwitchFailed { .. }
            | DomainEvent::ThreadDeletionFinished { .. }
            | DomainEvent::ThreadStarted { .. }) => self.reduce_thread_event(event),
            event => self.reduce_turn_event(event),
        }
    }
}
