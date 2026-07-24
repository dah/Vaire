use super::*;

impl AppState {
    pub(in crate::app) fn thread_action_block_reason(
        &self,
        require_account_identity: bool,
    ) -> Option<String> {
        if self.pending_new_thread_scope.is_some() {
            return Some("wait for the new thread request to finish".to_owned());
        }
        if self.conversation_popup().is_some() {
            return Some(
                "close the thread picker before starting another thread action".to_owned(),
            );
        }
        if !matches!(self.connection, ConnectionState::Ready { .. }) {
            return Some("app-server is not connected".to_owned());
        }
        let AuthState::SignedIn { scope } = &self.auth else {
            return Some("sign in with /login before managing threads".to_owned());
        };
        if self.turn.is_active() {
            return Some("wait for or interrupt the active turn".to_owned());
        }
        if require_account_identity && scope.is_none() {
            return Some(
                "ChatGPT account identity is unavailable; thread history cannot be opened safely"
                    .to_owned(),
            );
        }
        if self.models.is_empty()
            || !matches!(
                self.selected_model,
                Some(ModelKey {
                    provider: ProviderId::Codex,
                    ..
                })
            )
        {
            return Some("model catalog is not ready".to_owned());
        }
        None
    }

    pub(in crate::app) fn begin_login(&mut self, effect: Effect) -> Vec<Effect> {
        if !matches!(self.connection, ConnectionState::Ready { .. }) {
            self.notice = Some("app-server is not connected".to_owned());
        } else if matches!(self.auth, AuthState::SignedOut) {
            return vec![effect];
        } else if matches!(self.auth, AuthState::SigningIn { .. }) {
            self.notice =
                Some("sign-in is already in progress; use /logout to cancel it".to_owned());
        } else {
            self.notice = Some("logout before starting another login".to_owned());
        }
        Vec::new()
    }

    pub(in crate::app) fn send_block_reason(&self) -> Option<String> {
        if self.active_provider == ProviderId::Claude {
            if !matches!(self.claude.availability, ClaudeAvailability::Ready) {
                return Some("Claude Code runtime is not available".to_owned());
            }
            if self.claude.auth != ClaudeAuthStatus::Valid {
                return Some("configure Claude Code with /login before sending".to_owned());
            }
            let Some(model) = self
                .selected_model
                .as_ref()
                .filter(|key| key.provider == ProviderId::Claude)
            else {
                return Some("select a Claude model alias with /model".to_owned());
            };
            if !self.model_key_is_available(model) {
                return Some("select a Claude model alias with /model".to_owned());
            }
            if self.pending_new_claude_session {
                return Some("wait for the new Claude conversation request to finish".to_owned());
            }
            if self.turn.is_active() {
                return Some("wait for or interrupt the active turn".to_owned());
            }
            if self.conversation_popup().is_some() {
                return Some("close the conversation picker before sending".to_owned());
            }
            if matches!(
                self.claude.conversation,
                ClaudeConversationState::ResumeFailed { .. }
                    | ClaudeConversationState::CreationUncertain { .. }
            ) {
                return Some("resolve the saved Claude session with /resume or /new".to_owned());
            }
            return None;
        }
        if self.active_provider == ProviderId::OpenRouter {
            if self.openrouter.auth != crate::openrouter::OpenRouterAuthStatus::Valid {
                return Some("configure OpenRouter with /login before sending".to_owned());
            }
            let Some(model) = self
                .selected_model
                .as_ref()
                .filter(|key| key.provider == ProviderId::OpenRouter)
            else {
                return Some("select an enabled OpenRouter model with /model".to_owned());
            };
            if !self.model_key_is_available(model) {
                return Some("select an enabled OpenRouter model with /model".to_owned());
            }
            if self.turn.is_active() {
                return Some("wait for or interrupt the active turn".to_owned());
            }
            if self.conversation_popup().is_some() {
                return Some("close the conversation picker before sending".to_owned());
            }
            return None;
        }
        if !matches!(self.connection, ConnectionState::Ready { .. }) {
            return Some("app-server is not connected".to_owned());
        }
        if !matches!(self.auth, AuthState::SignedIn { .. }) {
            return Some("sign in with /login before sending".to_owned());
        }
        if self.models.is_empty()
            || !matches!(
                self.selected_model,
                Some(ModelKey {
                    provider: ProviderId::Codex,
                    ..
                })
            )
        {
            return Some("model catalog is not ready".to_owned());
        }
        if self.pending_new_thread_scope.is_some() {
            return Some("wait for the new thread request to finish".to_owned());
        }
        if self.turn.is_active() {
            return Some("wait for or interrupt the active turn".to_owned());
        }
        if self.conversation_popup().is_some() {
            return Some("close the thread picker before sending".to_owned());
        }
        if matches!(self.thread, ThreadState::Resuming { .. }) {
            return Some("wait for thread resume to finish".to_owned());
        }
        if matches!(
            self.thread,
            ThreadState::ResumeFailed { .. } | ThreadState::AccountMismatch { .. }
        ) {
            return Some(
                "resolve the saved thread with /resume or the matching account".to_owned(),
            );
        }
        None
    }
}
