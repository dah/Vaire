use super::*;

mod claude;
mod openrouter;
mod openrouter_turn;

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
            event @ (DomainEvent::ClaudeStartup { .. }
            | DomainEvent::ClaudeAuthChanged(_)
            | DomainEvent::ClaudeOperationFailed(_)
            | DomainEvent::ClaudeCandidateRejected(_)
            | DomainEvent::ClaudeSessionStarted { .. }
            | DomainEvent::ClaudeNewSessionFailed(_)
            | DomainEvent::ClaudeSessionCreationUncertain { .. }
            | DomainEvent::ClaudeSessionRestored { .. }
            | DomainEvent::ClaudeSessionSwitchFailed { .. }
            | DomainEvent::ClaudeResumeFailed { .. }
            | DomainEvent::ClaudeTurnStarted { .. }
            | DomainEvent::ClaudeInitialized { .. }
            | DomainEvent::ClaudeDelta { .. }
            | DomainEvent::ClaudeTurnFinished { .. }) => self.reduce_claude_event(event),
            event @ (DomainEvent::OpenRouterStartup { .. }
            | DomainEvent::OpenRouterAuthChanged(_)
            | DomainEvent::OpenRouterCatalogLoaded(_)
            | DomainEvent::OpenRouterCatalogLoadedForAutomaticResume(_)
            | DomainEvent::OpenRouterOperationFailed(_)
            | DomainEvent::OpenRouterCandidateRejected(_)
            | DomainEvent::OpenRouterConversationStarted { .. }
            | DomainEvent::OpenRouterConversationRestored { .. }
            | DomainEvent::OpenRouterConversationSwitchFailed { .. }
            | DomainEvent::OpenRouterResumeFailed { .. }
            | DomainEvent::OpenRouterTurnStarted { .. }
            | DomainEvent::OpenRouterDelta { .. }
            | DomainEvent::OpenRouterUsage { .. }
            | DomainEvent::OpenRouterTurnFinished { .. }) => self.reduce_openrouter_event(event),
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
